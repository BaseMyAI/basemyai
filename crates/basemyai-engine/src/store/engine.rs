//! The public single-writer KV engine: WAL + memtable + SST, with crash
//! recovery on `open`. See the `store` module docs for the write-path
//! ordering guarantee.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{EngineError, Result};
use crate::format::wal::{BatchOp, WalOp};
use crate::key::Key;
use crate::store::Value;
use crate::store::memtable::Memtable;
use crate::store::sst::{self, SstFile};
use crate::store::wal::Wal;

/// A group of `put`/`delete` operations applied atomically by
/// [`Engine::apply_batch`]: on reopen after a crash mid-batch, either every
/// operation in the batch is visible or none are — see that method's doc for
/// how the WAL framing guarantees this. An empty batch is a valid no-op.
#[derive(Debug, Clone, Default)]
pub struct Batch {
    ops: Vec<(Key, Option<Value>)>,
}

impl Batch {
    /// Creates an empty batch.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Stages an insert-or-overwrite of `key`. Later ops in the same batch
    /// for the same key win over earlier ones, same as issuing them as
    /// separate `put`/`delete` calls in order.
    pub fn put(&mut self, key: &[u8], value: &[u8]) {
        self.ops.push((Key::from(key), Some(value.to_vec())));
    }

    /// Stages a delete of `key` (a no-op if it wasn't present).
    pub fn delete(&mut self, key: &[u8]) {
        self.ops.push((Key::from(key), None));
    }

    /// Number of staged operations.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ops.len()
    }

    /// Whether no operations have been staged yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// Appends every operation staged in `other`, in order, after the ones
    /// already staged here. This is how a consumer's companion records ride
    /// the *same* atomic batch as an index mutation
    /// (`PersistentVectorIndex::insert_with`/`delete_with`, ADR-027 §3):
    /// merged batches share the single WAL record, so they commit or vanish
    /// together.
    pub fn extend_from(&mut self, other: &Batch) {
        self.ops.extend(other.ops.iter().cloned());
    }
}

/// Tunables for [`Engine`]. Defaults favor correctness / small-scale testing
/// over throughput — this phase's brief is "correctness first", not tuned
/// for the eventual crash-consistency/fuzz harnesses' scale.
#[derive(Debug, Clone, Copy)]
pub struct EngineOptions {
    /// Number of memtable entries at which `put`/`delete` auto-triggers a
    /// `flush()`.
    pub memtable_flush_threshold: usize,
    /// Number of on-disk SSTs at which the next `flush()` also merges every
    /// existing SST into one (naive full-merge compaction — ADR-025
    /// explicitly defers a tiered/leveled strategy to a later N2 step).
    pub compaction_sst_threshold: usize,
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            memtable_flush_threshold: 1000,
            compaction_sst_threshold: 4,
        }
    }
}

/// A single-node, single-writer, crash-safe KV store: WAL first, then
/// memtable; flush as an ordered SST (fsync, rename, *then* truncate the
/// WAL — never the other order, per ADR-025).
pub struct Engine {
    dir: PathBuf,
    wal: Wal,
    memtable: Memtable,
    /// Ordered oldest to newest.
    ssts: Vec<SstFile>,
    next_sst_id: u64,
    options: EngineOptions,
}

impl Engine {
    /// Opens (creating if absent) the store at `path` with default
    /// [`EngineOptions`]: loads existing SSTs, then replays the WAL
    /// (tolerating a torn trailing record) to rebuild whatever memtable
    /// state hadn't been flushed yet.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_options(path, EngineOptions::default())
    }

    /// Same as [`Engine::open`] with explicit tunables.
    pub fn open_with_options(path: impl AsRef<Path>, options: EngineOptions) -> Result<Self> {
        let dir = path.as_ref().to_path_buf();
        fs::create_dir_all(&dir).map_err(|e| EngineError::io(dir.clone(), e))?;

        let ssts = sst::scan_existing(&dir)?;
        let next_sst_id = ssts.iter().map(|s| s.id + 1).max().unwrap_or(0);

        let wal_path = dir.join("wal.log");
        let wal = Wal::open_for_append(wal_path)?;
        let mut memtable = Memtable::new();
        for record in wal.replay()? {
            match record.op {
                WalOp::Put => {
                    memtable.put(Key::from(record.key), record.value.unwrap_or_default());
                }
                WalOp::Delete => memtable.delete(Key::from(record.key)),
                // `Wal::replay` always expands `Batch` records into their
                // individual `Put`/`Delete` sub-operations before returning
                // — a bare `Batch` here would be a bug in that expansion,
                // not a reachable on-disk state.
                WalOp::Batch => {
                    unreachable!("Wal::replay expands Batch records into Put/Delete before returning")
                }
            }
        }

        Ok(Self {
            dir,
            wal,
            memtable,
            ssts,
            next_sst_id,
            options,
        })
    }

    /// Inserts or overwrites `key`. Durable once this returns `Ok` — the WAL
    /// record is fsynced before the memtable is updated.
    pub fn put(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        self.wal.append(WalOp::Put, key, Some(value))?;
        self.memtable.put(Key::from(key), value.to_vec());
        self.maybe_flush()
    }

    /// Deletes `key` (a no-op if it wasn't present). Durable once this
    /// returns `Ok`.
    pub fn delete(&mut self, key: &[u8]) -> Result<()> {
        self.wal.append(WalOp::Delete, key, None)?;
        self.memtable.delete(Key::from(key));
        self.maybe_flush()
    }

    /// Applies every operation in `batch` atomically: on reopen after a
    /// crash, either all of them are visible or none are — never a partial
    /// subset. A no-op (does not touch the WAL at all) if `batch` is empty.
    ///
    /// Durability/atomicity comes entirely from the WAL framing: the whole
    /// batch is appended as a single `Batch` WAL record (one `write_all` +
    /// one `sync_all`, one checksum over every sub-operation — see
    /// `format::wal`'s "Batch records" section and `store::wal::Wal::
    /// append_batch`), so replay either finds the complete record and
    /// applies every sub-operation, or finds a torn trailing record and
    /// applies none of them — the same torn-tail tolerance the engine
    /// already relies on for single `put`/`delete` records, just covering
    /// the whole batch's bytes instead of one op's.
    pub fn apply_batch(&mut self, batch: &Batch) -> Result<()> {
        if batch.is_empty() {
            return Ok(());
        }
        let wal_ops: Vec<BatchOp> = batch
            .ops
            .iter()
            .map(|(key, value)| BatchOp {
                op: if value.is_some() { WalOp::Put } else { WalOp::Delete },
                key: key.as_bytes().to_vec(),
                value: value.clone(),
            })
            .collect();
        self.wal.append_batch(&wal_ops)?;

        for (key, value) in &batch.ops {
            match value {
                Some(v) => self.memtable.put(key.clone(), v.clone()),
                None => self.memtable.delete(key.clone()),
            }
        }
        self.maybe_flush()
    }

    /// Point lookup: memtable first, then SSTs newest to oldest — the first
    /// hit (value or tombstone) wins.
    pub fn get(&self, key: &[u8]) -> Result<Option<Value>> {
        let key = Key::from(key);
        if let Some(hit) = self.memtable.get(&key) {
            return Ok(hit.cloned());
        }
        for s in self.ssts.iter().rev() {
            if let Some(hit) = s.get(&key) {
                return Ok(hit.cloned());
            }
        }
        Ok(None)
    }

    /// Range scan: every live key starting with `prefix`, with its current
    /// value, in ascending key order. Tombstoned keys are omitted.
    ///
    /// Same layering rule as [`Engine::get`], expressed as a merge: SSTs
    /// oldest to newest, then the memtable, later layers overwriting earlier
    /// ones — the newest state per key wins, then tombstones are dropped.
    ///
    /// Materializes the matching set in memory (no streaming iterator yet) —
    /// fine for its current caller, the vector-index rebuild path
    /// (`idx::vector::persistent`), which needs every node block anyway;
    /// a streaming scan is deliberately deferred until something needs it.
    pub fn scan_prefix(&self, prefix: &[u8]) -> Result<Vec<(Key, Value)>> {
        let mut merged: BTreeMap<Key, Option<Value>> = BTreeMap::new();
        for s in &self.ssts {
            for (k, v) in s.entries() {
                if k.as_bytes().starts_with(prefix) {
                    merged.insert(k.clone(), v.clone());
                }
            }
        }
        for (k, v) in self.memtable.iter() {
            if k.as_bytes().starts_with(prefix) {
                merged.insert(k.clone(), v.clone());
            }
        }
        Ok(merged
            .into_iter()
            .filter_map(|(k, v)| v.map(|value| (k, value)))
            .collect())
    }

    /// Forces the memtable out to a new SST regardless of the configured
    /// threshold, then truncates the WAL — in that order (ADR-025). A no-op
    /// if the memtable is empty.
    pub fn flush(&mut self) -> Result<()> {
        if self.memtable.is_empty() {
            return Ok(());
        }
        let entries: Vec<(Key, Option<Value>)> = self.memtable.iter().map(|(k, v)| (k.clone(), v.clone())).collect();

        let id = self.next_sst_id;
        let new_sst = SstFile::write_new(&self.dir, id, entries)?;
        // The new SST is fsynced and durably renamed at this point — only
        // now is it safe to truncate the WAL (ADR-025 ordering rule).
        self.wal.reset()?;

        self.next_sst_id += 1;
        self.ssts.push(new_sst);
        self.memtable.clear();

        if self.ssts.len() > self.options.compaction_sst_threshold {
            self.compact()?;
        }
        Ok(())
    }

    /// Naive full-merge compaction: folds every existing SST (oldest to
    /// newest, later writes win) into a single new SST, dropping tombstones
    /// entirely — safe because this merge covers *all* existing data, so a
    /// deleted key has no older layer left to resurrect from. Correctness
    /// first; a tiered/leveled strategy is deferred (ADR-025).
    fn compact(&mut self) -> Result<()> {
        let mut merged: BTreeMap<Key, Option<Value>> = BTreeMap::new();
        for s in &self.ssts {
            for (k, v) in s.entries() {
                merged.insert(k.clone(), v.clone());
            }
        }
        let entries: Vec<(Key, Option<Value>)> = merged.into_iter().filter(|(_, v)| v.is_some()).collect();

        let id = self.next_sst_id;
        let new_sst = SstFile::write_new(&self.dir, id, entries)?;
        self.next_sst_id += 1;

        let old_ssts = std::mem::replace(&mut self.ssts, vec![new_sst]);
        for old in old_ssts {
            // Best-effort cleanup: the merged SST above is already fsynced
            // and durably renamed, so failing to remove an old (now
            // redundant) file is a space leak, not a correctness issue —
            // `get` always finds the newest SST first, and there is now
            // exactly one.
            let _ = fs::remove_file(&old.path);
        }
        Ok(())
    }

    /// Flushes any pending memtable data and releases the store. Skipping
    /// `close` is safe too — durability is already established per-`put`/
    /// `delete` via WAL fsync — it just avoids leaving unflushed data to be
    /// replayed from the WAL on the next `open`.
    pub fn close(mut self) -> Result<()> {
        self.flush()
    }

    fn maybe_flush(&mut self) -> Result<()> {
        if self.memtable.len() >= self.options.memtable_flush_threshold {
            self.flush()?;
        }
        Ok(())
    }
}
