// SPDX-License-Identifier: BUSL-1.1
//! The public single-writer KV engine: WAL + memtable + SST, with crash
//! recovery on `open`. See the `store` module docs for the write-path
//! ordering guarantee.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::crypto::{self, CryptoContext};
use crate::error::{EngineError, Result};
use crate::fail_point;
use crate::format::store_meta::{self, StoreMeta};
use crate::format::wal::{BatchOp, WalOp};
use crate::key::Key;
use crate::store::Value;
use crate::store::block_cache::BlockCache;
use crate::store::memtable::Memtable;
use crate::store::sst_block::{self, BlockSstFile};
use crate::store::stats::{Counters, EngineStats};
use crate::store::wal::Wal;

/// Default `EngineOptions::block_size` — the winning value from the N8.1
/// spike (`docs/benchmarks/n8.1-block-size-spike-2026-07-10.md`), measured
/// against 16/32/64 KiB on the canonical `kv`/`vecnode` workloads, clear and
/// encrypted.
pub const DEFAULT_BLOCK_SIZE: u32 = 16 * 1024;

/// Default `EngineOptions::block_cache_capacity_bytes` (N8.7, ADR-039 §5.6)
/// — an order-of-magnitude starting point, not yet measured against a
/// dedicated cache-sizing bench (that's a follow-up, not this milestone's
/// brief: "no speculative sophistication").
pub const DEFAULT_BLOCK_CACHE_CAPACITY_BYTES: usize = 32 * 1024 * 1024;

/// One page of [`Engine::scan_range_page`] (ADR-041 §7.3): the definitive
/// live entries of the page in ascending key order, plus the inclusive
/// `start` to resume from — `None` when the range is exhausted. `entries`
/// can legitimately be empty while `next_start` is `Some` (a tombstone-only
/// stretch): callers loop on `next_start`, never on `entries.is_empty()`.
#[derive(Debug, Clone)]
pub struct ScanPage {
    pub entries: Vec<(Key, Value)>,
    pub next_start: Option<Vec<u8>>,
}

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

    /// Approximate size of the WAL record this batch would produce: key +
    /// value payloads plus a small per-op framing constant. An estimate, not
    /// the exact encoded length — the byte-budget accounting behind bounded
    /// multi-record deletion (ADR-041 §7.4) needs a sizing target, not
    /// wire-format precision.
    #[must_use]
    pub fn approx_wire_bytes(&self) -> usize {
        self.ops
            .iter()
            .map(|(key, value)| key.as_bytes().len() + value.as_ref().map_or(0, Vec::len) + 16)
            .sum()
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
    /// Target size (bytes) of one SST data block before the writer starts a
    /// new one — a target, not an exact bound (ADR-039 §1). Read back from
    /// each SST's own header at open, so a store may (and typically will)
    /// contain SSTs written under different `block_size` values over its
    /// lifetime; the reader has a single code path regardless. Default:
    /// [`DEFAULT_BLOCK_SIZE`] (16 KiB, the N8.1 spike's winning value).
    pub block_size: u32,
    /// Byte budget for the engine-wide decoded-block cache (N8.7, ADR-039
    /// §5.6) — one shared LRU across every SST this `Engine` holds, keyed
    /// by `(sst_id, block_no)`. Default: [`DEFAULT_BLOCK_CACHE_CAPACITY_BYTES`]
    /// (32 MiB).
    pub block_cache_capacity_bytes: usize,
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            memtable_flush_threshold: 1000,
            compaction_sst_threshold: 4,
            block_size: DEFAULT_BLOCK_SIZE,
            block_cache_capacity_bytes: DEFAULT_BLOCK_CACHE_CAPACITY_BYTES,
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
    ssts: Vec<BlockSstFile>,
    next_sst_id: u64,
    options: EngineOptions,
    /// `Some` = encrypted at rest (ADR-030): WAL records and SST files are
    /// sealed under the store's DEK; `crypto.meta` holds the DEK wrapped by
    /// the user key.
    crypto: Option<CryptoContext>,
    /// Monotonic activity counters since open (N7.1) — the gauge half of
    /// [`EngineStats`] is derived from live state in [`Engine::stats`].
    counters: Counters,
    /// Counter: point lookups that read more than one on-disk data block
    /// within a single SST (ADR-039 §4/§5.5). `AtomicU64`, not a plain
    /// field in [`Counters`], because [`Engine::get`] is `&self` (Engine's
    /// public API shape does not change with this milestone) yet still
    /// needs to record this invariant.
    point_lookup_full_sst_read: AtomicU64,
    /// Engine-wide bounded LRU cache of decoded SST data blocks (N8.7,
    /// ADR-039 §5.6), consulted only by [`Engine::get`]'s point-lookup path
    /// — never by `scan_prefix`/`compact`'s full walks, which would just
    /// pollute it with cold data. Interior-mutable so `get` can stay
    /// `&self`; see `store::block_cache` for the "no lock across I/O"
    /// contract.
    block_cache: BlockCache,
}

impl Engine {
    /// Opens (creating if absent) the store at `path` with default
    /// [`EngineOptions`]: loads existing SSTs, then replays the WAL
    /// (tolerating a torn trailing record) to rebuild whatever memtable
    /// state hadn't been flushed yet.
    ///
    /// # Errors
    /// Besides I/O and corruption: [`EngineError::MissingEncryptionKey`] if
    /// the store at `path` is encrypted (`crypto.meta` present) — use
    /// [`Engine::open_encrypted`] for it.
    ///
    /// Réservé aux tests (`test-util`) : la production ouvre toujours via
    /// [`Engine::open_encrypted`] (ADR-030/033).
    #[cfg(any(test, feature = "test-util"))]
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_options(path, EngineOptions::default())
    }

    /// Same as [`Engine::open`] with explicit tunables.
    #[cfg(any(test, feature = "test-util"))]
    pub fn open_with_options(path: impl AsRef<Path>, options: EngineOptions) -> Result<Self> {
        Self::open_inner(path.as_ref(), options, None)
    }

    /// Opens (creating if absent) an **encrypted** store at `path`
    /// (ADR-030). On first open of a fresh directory, generates the store's
    /// DEK and writes `crypto.meta`; on reopen, unwrapping the DEK verifies
    /// `key` — a wrong key fails here, fast and typed, never as
    /// inexplicable corruption further in.
    ///
    /// # Errors
    /// [`EngineError::WrongEncryptionKey`] if `key` doesn't unwrap the
    /// store's DEK; [`EngineError::PlaintextStoreKeySupplied`] if `path`
    /// already holds a plaintext store (encrypting a posteriori is
    /// deliberately unsupported, ADR-030 §2); plus the usual I/O/corruption
    /// errors.
    pub fn open_encrypted(path: impl AsRef<Path>, key: &[u8]) -> Result<Self> {
        Self::open_encrypted_with_options(path, key, EngineOptions::default())
    }

    /// Same as [`Engine::open_encrypted`] with explicit tunables.
    pub fn open_encrypted_with_options(path: impl AsRef<Path>, key: &[u8], options: EngineOptions) -> Result<Self> {
        Self::open_inner(path.as_ref(), options, Some(key))
    }

    fn open_inner(path: &Path, options: EngineOptions, key: Option<&[u8]>) -> Result<Self> {
        let dir = path.to_path_buf();
        fs::create_dir_all(&dir).map_err(|e| EngineError::io(dir.clone(), e))?;

        // Store-generation check (N8.9, ADR-039 §7) — before touching
        // crypto/WAL/SST state at all: an incompatible or pre-ADR-039 store
        // must fail fast and typed, not as inexplicable corruption further
        // in. A genuinely fresh directory publishes a new `store.meta` here.
        check_or_create_store_meta(&dir)?;

        // `crypto.meta`'s presence is the single source of truth for the
        // store's mode (ADR-030 §2) — never guessed from file contents.
        let meta_exists = crypto::crypto_meta_path(&dir).exists();
        let crypto = match (meta_exists, key) {
            (true, Some(key)) => Some(crypto::load_meta(&dir, key)?),
            (true, None) => return Err(EngineError::MissingEncryptionKey { path: dir }),
            (false, Some(key)) => {
                // Refuse to mix modes: a directory that already holds
                // plaintext artifacts cannot be encrypted a posteriori.
                let has_wal = dir.join("wal.log").exists();
                let has_sst = sst_files_present(&dir)?;
                if has_wal || has_sst {
                    return Err(EngineError::PlaintextStoreKeySupplied { path: dir });
                }
                Some(crypto::create_meta(&dir, key)?)
            }
            (false, None) => None,
        };

        let ssts = sst_block::scan_existing(&dir, crypto.as_ref())?;
        let next_sst_id = ssts.iter().map(|s| s.id + 1).max().unwrap_or(0);

        // Everything loaded at open was read from disk: the O(metadata)
        // bytes each SST's lazy `load` actually reads (N8.4 — never the
        // whole file), plus the WAL bytes replayed just below.
        let mut counters = Counters {
            bytes_read: ssts.iter().map(|s| s.bytes_read_at_open).sum(),
            ..Counters::default()
        };

        let wal_path = dir.join("wal.log");
        let mut wal = Wal::open_for_append(wal_path, crypto.clone())?;
        counters.bytes_read += wal.size_on_disk()?;
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

        let block_cache = BlockCache::new(options.block_cache_capacity_bytes);
        Ok(Self {
            dir,
            wal,
            memtable,
            ssts,
            next_sst_id,
            options,
            crypto,
            counters,
            point_lookup_full_sst_read: AtomicU64::new(0),
            block_cache,
        })
    }

    /// Point-in-time observability snapshot (N7.1): monotonic counters since
    /// open plus gauges over the current state. See [`EngineStats`] for
    /// per-field semantics. Cheap — the only iteration is over the memtable,
    /// bounded by `memtable_flush_threshold`.
    ///
    /// # Errors
    /// I/O errors from statting the WAL file.
    pub fn stats(&self) -> Result<EngineStats> {
        let mut memtable_bytes = 0u64;
        let mut memtable_tombstones = 0u64;
        for (k, v) in self.memtable.iter() {
            memtable_bytes += k.as_bytes().len() as u64;
            match v {
                Some(value) => memtable_bytes += value.len() as u64,
                None => memtable_tombstones += 1,
            }
        }
        Ok(EngineStats {
            wal_bytes: self.wal.size_on_disk()?,
            wal_records: self.counters.wal_records,
            memtable_bytes,
            sst_count: self.ssts.len(),
            sst_bytes: self.ssts.iter().map(|s| s.file_bytes).sum(),
            tombstone_count: memtable_tombstones + self.ssts.iter().map(|s| s.tombstones).sum::<u64>(),
            flush_count: self.counters.flush_count,
            compaction_count: self.counters.compaction_count,
            compaction_input_bytes: self.counters.compaction_input_bytes,
            compaction_output_bytes: self.counters.compaction_output_bytes,
            bytes_read: self.counters.bytes_read,
            bytes_written: self.counters.bytes_written,
            block_cache_hits: self.block_cache.hits(),
            block_cache_misses: self.block_cache.misses(),
            point_lookup_full_sst_read: self.point_lookup_full_sst_read.load(Ordering::Relaxed),
        })
    }

    /// `true` if this instance is encrypted at rest (ADR-030).
    #[must_use]
    pub fn is_encrypted(&self) -> bool {
        self.crypto.is_some()
    }

    /// Rotates the user key **in place** (ADR-030 §4): the store's DEK is
    /// re-wrapped under a KEK derived from `new_key` (fresh salt) and
    /// `crypto.meta` is atomically replaced (tmp + fsync + rename). O(1) —
    /// no data file is rewritten — and crash-safe by construction: after a
    /// crash, `crypto.meta` is either the old wrap (old key opens) or the
    /// new one (new key opens), never a mixed state.
    ///
    /// Unlike libSQL's `Store::rotate_key`, **this instance stays fully
    /// usable after the call** (the DEK itself never changes). The assumed,
    /// documented deviation: an attacker holding the old key *and* a copy
    /// of the old `crypto.meta` can still unwrap the DEK — see ADR-030 §4
    /// for the threat-model discussion and the deferred full-re-encryption
    /// follow-up.
    ///
    /// # Errors
    /// [`EngineError::NotEncrypted`] if this store was opened without
    /// encryption (nothing to rotate — parity with ADR-007's posture);
    /// otherwise I/O errors from the atomic replace.
    pub fn rotate_key(&mut self, new_key: &[u8]) -> Result<()> {
        let Some(crypto) = &self.crypto else {
            return Err(EngineError::NotEncrypted { path: self.dir.clone() });
        };
        crypto::write_meta(&self.dir, new_key, crypto)
    }

    /// Inserts or overwrites `key`. Durable once this returns `Ok` — the WAL
    /// record is fsynced before the memtable is updated.
    pub fn put(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        let written = self.wal.append(WalOp::Put, key, Some(value))?;
        self.note_wal_record(written);
        self.memtable.put(Key::from(key), value.to_vec());
        self.maybe_flush()
    }

    /// Deletes `key` (a no-op if it wasn't present). Durable once this
    /// returns `Ok`.
    pub fn delete(&mut self, key: &[u8]) -> Result<()> {
        let written = self.wal.append(WalOp::Delete, key, None)?;
        self.note_wal_record(written);
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
        let written = self.wal.append_batch(&wal_ops)?;
        self.note_wal_record(written);

        for (key, value) in &batch.ops {
            match value {
                Some(v) => self.memtable.put(key.clone(), v.clone()),
                None => self.memtable.delete(key.clone()),
            }
        }
        self.maybe_flush()
    }

    /// Point lookup: memtable first, then SSTs newest to oldest — the first
    /// hit (value or tombstone) wins. Each SST consulted resolves through
    /// its own bloom-filter -> block-index -> single-block-read path — never
    /// a full SST read. Feeds the `point_lookup_full_sst_read` invariant
    /// counter surfaced by [`Self::stats`] (ADR-039 §4/§5.5).
    pub fn get(&self, key: &[u8]) -> Result<Option<Value>> {
        let key = Key::from(key);
        if let Some(hit) = self.memtable.get(&key) {
            return Ok(hit.cloned());
        }
        for s in self.ssts.iter().rev() {
            let (hit, blocks_read) = s.get(&key, &self.block_cache)?;
            if blocks_read > 1 {
                self.point_lookup_full_sst_read.fetch_add(1, Ordering::Relaxed);
            }
            if let Some(value) = hit {
                return Ok(value);
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
    ///
    /// Per SST, only the data blocks overlapping the prefix range are
    /// decoded, via binary search on the block index
    /// ([`BlockSstFile::entries_with_prefix`]) — never a full-file decode.
    pub fn scan_prefix(&self, prefix: &[u8]) -> Result<Vec<(Key, Value)>> {
        let mut merged: BTreeMap<Key, Option<Value>> = BTreeMap::new();
        for s in &self.ssts {
            let (matches, _blocks_read) = s.entries_with_prefix(prefix)?;
            for (k, v) in matches {
                merged.insert(k, v);
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

    /// Every live entry with a key in `[start, end)` — the genuine
    /// range-query counterpart to [`Self::scan_prefix`] (ADR-041 §7.2):
    /// unlike a prefix scan, `end` bounds the query on both sides, so SST
    /// blocks entirely below `start` or at/past `end` are skipped without
    /// being decoded ([`BlockSstFile::entries_with_range`]), not just
    /// filtered after a full read. `start >= end` is an empty range, not an
    /// error.
    pub fn scan_range(&self, start: &[u8], end: &[u8]) -> Result<Vec<(Key, Value)>> {
        if start >= end {
            return Ok(Vec::new());
        }
        let mut merged: BTreeMap<Key, Option<Value>> = BTreeMap::new();
        for s in &self.ssts {
            let (matches, _blocks_read) = s.entries_with_range(start, end)?;
            for (k, v) in matches {
                merged.insert(k, v);
            }
        }
        for (k, v) in self.memtable.iter() {
            if k.as_bytes() >= start && k.as_bytes() < end {
                merged.insert(k.clone(), v.clone());
            }
        }
        Ok(merged
            .into_iter()
            .filter_map(|(k, v)| v.map(|value| (k, value)))
            .collect())
    }

    /// One bounded page of [`Self::scan_range`] (ADR-041 §7.3): at most
    /// ~`limit` live entries from `[start, end)`, in ascending key order,
    /// with memory bounded by `O(sources × limit)` instead of the full
    /// matching set — the primitive a paged full-population scan needs
    /// (`scan_range` materializes everything, which is exactly what a
    /// bounded-memory maintenance pass must avoid).
    ///
    /// Paging protocol: re-invoke with `start = next_start` until
    /// `next_start` is `None`. **An empty `entries` with a `Some(next_start)`
    /// means progress, not exhaustion** — a stretch of keys whose newest
    /// layer is a tombstone yields no live entries yet still advances the
    /// cursor. Loop on `next_start`, never on `entries.is_empty()`.
    ///
    /// How the bound stays correct under LSM layering: each source (every
    /// SST, plus the memtable) is read up to at most `limit` in-range
    /// entries. A source that got truncated is only complete up to its last
    /// returned key, so the page's *frontier* is the smallest such key
    /// across truncated sources — every key `<= frontier` has been seen by
    /// every source (each one returned all its keys at least that far), so
    /// last-write-wins merging is definitive there. Merged keys past the
    /// frontier are discarded (a not-yet-read older layer can't change them,
    /// but a not-yet-read *newer* one could) and re-read by the next page.
    pub fn scan_range_page(&self, start: &[u8], end: &[u8], limit: usize) -> Result<ScanPage> {
        if start >= end || limit == 0 {
            return Ok(ScanPage {
                entries: Vec::new(),
                next_start: None,
            });
        }
        let mut merged: BTreeMap<Key, Option<Value>> = BTreeMap::new();
        // Frontier = min over truncated sources of "the last key that source
        // returned". `None` until some source truncates.
        let mut frontier: Option<Vec<u8>> = None;
        let clip = |candidate: Option<Vec<u8>>, current: Option<Vec<u8>>| match (candidate, current) {
            (Some(c), Some(f)) => Some(c.min(f)),
            (Some(c), None) => Some(c),
            (None, f) => f,
        };
        for s in &self.ssts {
            let (matches, truncated, _blocks_read) = s.entries_with_range_limited(start, end, limit)?;
            if truncated {
                let last = matches.last().map(|(k, _)| k.as_bytes().to_vec());
                frontier = clip(last, frontier);
            }
            for (k, v) in matches {
                merged.insert(k, v);
            }
        }
        let mut taken = 0usize;
        let mut last_taken: Option<Vec<u8>> = None;
        for (k, v) in self.memtable.iter() {
            if k.as_bytes() < start || k.as_bytes() >= end {
                continue;
            }
            if taken == limit {
                // The memtable is complete only up to the last key actually
                // taken — the key we stopped at may shadow (overwrite or
                // tombstone) a same-key entry an older SST already merged,
                // so it must fall past the frontier and into the next page.
                frontier = clip(last_taken.take(), frontier);
                break;
            }
            last_taken = Some(k.as_bytes().to_vec());
            merged.insert(k.clone(), v.clone());
            taken += 1;
        }
        match frontier {
            None => Ok(ScanPage {
                entries: merged
                    .into_iter()
                    .filter_map(|(k, v)| v.map(|value| (k, value)))
                    .collect(),
                next_start: None,
            }),
            Some(f) => {
                let entries = merged
                    .into_iter()
                    .take_while(|(k, _)| k.as_bytes() <= f.as_slice())
                    .filter_map(|(k, v)| v.map(|value| (k, value)))
                    .collect();
                let mut next = f;
                next.push(0x00);
                Ok(ScanPage {
                    entries,
                    next_start: Some(next),
                })
            }
        }
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
        let new_sst = BlockSstFile::write_new(&self.dir, id, entries, self.options.block_size, self.crypto.as_ref())?;
        self.counters.flush_count += 1;
        self.counters.bytes_written += new_sst.file_bytes;
        // The new SST is fsynced and durably renamed at this point — only
        // now is it safe to truncate the WAL (ADR-025 ordering rule).
        fail_point!("before_wal_truncate");
        self.wal.reset()?;

        self.next_sst_id += 1;
        self.ssts.push(new_sst);
        self.memtable.clear();

        if self.ssts.len() > self.options.compaction_sst_threshold {
            self.compact()?;
        }
        Ok(())
    }

    /// Operator-triggered compaction (ADR-040 §3, N9.4 — the engine half of
    /// the `basemyai compact` CLI surface): flushes any pending memtable
    /// data, then runs the full-merge compaction unconditionally — unlike
    /// the automatic path, which only fires past
    /// `EngineOptions::compaction_sst_threshold`. Useful below the
    /// threshold too: merging even a single SST rewrites it without its
    /// tombstones (safe here for the same reason as the automatic path —
    /// the merge covers *all* existing data, so a deleted key has no older
    /// layer left to resurrect from). A no-op on a store with no SSTs and
    /// nothing to flush.
    pub fn compact_now(&mut self) -> Result<()> {
        self.flush()?;
        if self.ssts.is_empty() {
            return Ok(());
        }
        self.compact()
    }

    /// Naive full-merge compaction: folds every existing SST (oldest to
    /// newest, later writes win) into a single new SST, dropping tombstones
    /// entirely — safe because this merge covers *all* existing data, so a
    /// deleted key has no older layer left to resurrect from. Correctness
    /// first; a tiered/leveled strategy is deferred (ADR-025).
    fn compact(&mut self) -> Result<()> {
        fail_point!("during_compaction");
        let input_bytes: u64 = self.ssts.iter().map(|s| s.file_bytes).sum();
        let mut merged: BTreeMap<Key, Option<Value>> = BTreeMap::new();
        for s in &self.ssts {
            for (k, v) in s.entries()? {
                merged.insert(k, v);
            }
        }
        let entries: Vec<(Key, Option<Value>)> = merged.into_iter().filter(|(_, v)| v.is_some()).collect();

        let id = self.next_sst_id;
        let new_sst = BlockSstFile::write_new(&self.dir, id, entries, self.options.block_size, self.crypto.as_ref())?;
        self.next_sst_id += 1;
        self.counters.compaction_count += 1;
        self.counters.compaction_input_bytes += input_bytes;
        self.counters.compaction_output_bytes += new_sst.file_bytes;
        self.counters.bytes_written += new_sst.file_bytes;

        let old_ssts = std::mem::replace(&mut self.ssts, vec![new_sst]);
        for old in old_ssts {
            // Best-effort cleanup: the merged SST above is already fsynced
            // and durably renamed, so failing to remove an old (now
            // redundant) file is a space leak, not a correctness issue —
            // `get` always finds the newest SST first, and there is now
            // exactly one.
            let _ = fs::remove_file(&old.path);
            // A stale block from a deleted SST must never survive in the
            // cache: its `sst_id` could be reused by a future SST (or, if
            // not, would just be dead weight) — either way, drop it now.
            self.block_cache.invalidate_sst(old.id);
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

    fn note_wal_record(&mut self, bytes_written: u64) {
        self.counters.wal_records += 1;
        self.counters.bytes_written += bytes_written;
    }
}

/// `true` if `dir` contains at least one `*.sst` file — the "existing
/// plaintext store" half of the mode check in `Engine::open_inner` (the
/// other half is `wal.log`'s existence), and also half of the old-store
/// detection in [`check_or_create_store_meta`].
fn sst_files_present(dir: &Path) -> Result<bool> {
    if !dir.exists() {
        return Ok(false);
    }
    for entry in fs::read_dir(dir).map_err(|e| EngineError::io(dir.to_path_buf(), e))? {
        let entry = entry.map_err(|e| EngineError::io(dir.to_path_buf(), e))?;
        if entry.path().extension().and_then(|e| e.to_str()) == Some("sst") {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Store-generation gate (N8.9, ADR-039 §7) — the very first thing
/// `Engine::open_inner` does to `dir`, before crypto/WAL/SST state is
/// touched at all:
///
/// - `store.meta` present and its `store_format_version` matches
///   [`store_meta::STORE_FORMAT_VERSION`]: nothing to do, this build
///   understands the store.
/// - `store.meta` present but its version does not match: typed
///   [`EngineError::UnsupportedStoreFormat`] with `found` set to the actual
///   on-disk version.
/// - `store.meta` absent, but `wal.log` or a `*.sst` file already exists:
///   this is a pre-ADR-039 store (or, in principle, any store this build's
///   writer never produced) — [`EngineError::UnsupportedStoreFormat`] with
///   the sentinel `found: 0` (no store.meta was ever written by any
///   version, since [`store_meta::STORE_FORMAT_VERSION`] starts at 2 — "no
///   generation-1 `store.meta`", see that module's doc).
/// - `store.meta` absent and no other artifact present: a genuinely fresh
///   directory — publish a new `store.meta` now, crash-safe (tmp + fsync +
///   rename), behind the `before_manifest_publish` failpoint reserved for
///   this since N7.4.
///
/// Deliberately checks only `wal.log`/`*.sst`, not `crypto.meta`: for a
/// brand-new *encrypted* store, `crypto.meta` is created moments later in
/// the very same `open_inner` call (after this function returns) — treating
/// its presence as an "old artifact" here would make every first-ever
/// encrypted-store open falsely look like an incompatible reopen.
fn check_or_create_store_meta(dir: &Path) -> Result<()> {
    let meta_path = dir.join("store.meta");
    if meta_path.exists() {
        let bytes = fs::read(&meta_path).map_err(|e| EngineError::io(meta_path.clone(), e))?;
        let meta = store_meta::decode(&bytes, &meta_path)?;
        if meta.store_format_version != store_meta::STORE_FORMAT_VERSION {
            return Err(EngineError::UnsupportedStoreFormat {
                path: dir.to_path_buf(),
                expected: store_meta::STORE_FORMAT_VERSION,
                found: meta.store_format_version,
            });
        }
        return Ok(());
    }

    let has_old_artifacts = dir.join("wal.log").exists() || sst_files_present(dir)?;
    if has_old_artifacts {
        return Err(EngineError::UnsupportedStoreFormat {
            path: dir.to_path_buf(),
            expected: store_meta::STORE_FORMAT_VERSION,
            found: 0, // sentinel: no store.meta at all (pre-ADR-039 store)
        });
    }

    let tmp_path = meta_path.with_extension("meta.tmp");
    let bytes = store_meta::encode(&StoreMeta {
        store_format_version: store_meta::STORE_FORMAT_VERSION,
    });
    {
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp_path)
            .map_err(|e| EngineError::io(tmp_path.clone(), e))?;
        file.write_all(&bytes)
            .map_err(|e| EngineError::io(tmp_path.clone(), e))?;
        file.sync_all().map_err(|e| EngineError::io(tmp_path.clone(), e))?;
    }
    fail_point!("before_manifest_publish");
    fs::rename(&tmp_path, &meta_path).map_err(|e| EngineError::io(meta_path.clone(), e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &[u8] = b"test user key";

    /// Options that force flush + compaction quickly, so the encrypted
    /// roundtrip exercises sealed SST sections and compaction, not just the
    /// WAL. Small `block_size` too, so these small stores still span more
    /// than one data block per SST.
    fn small_options() -> EngineOptions {
        EngineOptions {
            memtable_flush_threshold: 4,
            compaction_sst_threshold: 2,
            block_size: 256,
            ..EngineOptions::default()
        }
    }

    #[test]
    fn encrypted_roundtrip_survives_flush_compaction_and_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let mut engine = Engine::open_encrypted_with_options(dir.path(), KEY, small_options()).expect("open");
            assert!(engine.is_encrypted());
            for i in 0..20u32 {
                engine
                    .put(format!("key-{i:03}").as_bytes(), format!("value-{i}").as_bytes())
                    .expect("put");
            }
            engine.delete(b"key-003").expect("delete");
            // Unflushed tail stays in the WAL on purpose: reopen must
            // replay encrypted WAL records, not just load SSTs.
            engine.put(b"tail", b"unflushed").expect("put tail");
        }
        let engine = Engine::open_encrypted_with_options(dir.path(), KEY, small_options()).expect("reopen");
        assert_eq!(engine.get(b"key-000").expect("get").as_deref(), Some(&b"value-0"[..]));
        assert_eq!(engine.get(b"key-019").expect("get").as_deref(), Some(&b"value-19"[..]));
        assert_eq!(engine.get(b"key-003").expect("get"), None);
        assert_eq!(engine.get(b"tail").expect("get").as_deref(), Some(&b"unflushed"[..]));
    }

    #[test]
    fn encrypted_batch_is_atomic_across_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let mut engine = Engine::open_encrypted(dir.path(), KEY).expect("open");
            let mut batch = Batch::new();
            batch.put(b"a", b"1");
            batch.put(b"b", b"2");
            batch.delete(b"a");
            engine.apply_batch(&batch).expect("apply");
        }
        let engine = Engine::open_encrypted(dir.path(), KEY).expect("reopen");
        assert_eq!(engine.get(b"a").expect("get"), None);
        assert_eq!(engine.get(b"b").expect("get").as_deref(), Some(&b"2"[..]));
    }

    #[test]
    fn wrong_key_fails_fast_and_typed() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let mut engine = Engine::open_encrypted(dir.path(), KEY).expect("open");
            engine.put(b"a", b"1").expect("put");
        }
        let Err(err) = Engine::open_encrypted(dir.path(), b"not the key") else {
            panic!("wrong key must fail")
        };
        assert!(matches!(err, EngineError::WrongEncryptionKey { .. }));
    }

    #[test]
    fn encrypted_store_without_key_is_missing_key_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            Engine::open_encrypted(dir.path(), KEY).expect("open");
        }
        let Err(err) = Engine::open(dir.path()) else {
            panic!("plaintext open of an encrypted store must fail")
        };
        assert!(matches!(err, EngineError::MissingEncryptionKey { .. }));
    }

    #[test]
    fn encrypted_reopen_rejects_short_plaintext_wal_without_truncating() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            Engine::open_encrypted(dir.path(), KEY).expect("create encrypted meta");
        }
        let wal_path = dir.path().join("wal.log");
        let plaintext = crate::format::wal::encode(crate::format::wal::WalOp::Put, b"a", Some(b"1"));
        std::fs::write(&wal_path, &plaintext).expect("write plaintext wal");

        let Err(err) = Engine::open_encrypted(dir.path(), KEY) else {
            panic!("plaintext wal must be corrupt in encrypted mode")
        };
        assert!(matches!(err, EngineError::CorruptWal { .. }));
        assert_eq!(
            std::fs::metadata(&wal_path).expect("wal metadata").len(),
            plaintext.len() as u64
        );
    }

    #[test]
    fn key_on_existing_plaintext_store_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let mut engine = Engine::open(dir.path()).expect("open plaintext");
            engine.put(b"a", b"1").expect("put");
        }
        let Err(err) = Engine::open_encrypted(dir.path(), KEY) else {
            panic!("a posteriori encryption is refused")
        };
        assert!(matches!(err, EngineError::PlaintextStoreKeySupplied { .. }));
    }

    #[test]
    fn rotate_key_switches_keys_without_reopen_and_preserves_data() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let mut engine = Engine::open_encrypted_with_options(dir.path(), KEY, small_options()).expect("open");
            for i in 0..10u32 {
                engine
                    .put(format!("k{i}").as_bytes(), format!("v{i}").as_bytes())
                    .expect("put");
            }
            engine.rotate_key(b"the new key").expect("rotate");
            // The instance stays fully usable after rotation (ADR-030 §4) —
            // unlike libSQL, no drop-and-reopen dance.
            engine
                .put(b"post-rotation", b"still writable")
                .expect("put after rotate");
            assert_eq!(engine.get(b"k5").expect("get").as_deref(), Some(&b"v5"[..]));
        }

        let Err(err) = Engine::open_encrypted(dir.path(), KEY) else {
            panic!("old key must no longer open")
        };
        assert!(matches!(err, EngineError::WrongEncryptionKey { .. }));

        let engine = Engine::open_encrypted_with_options(dir.path(), b"the new key", small_options()).expect("reopen");
        assert_eq!(engine.get(b"k0").expect("get").as_deref(), Some(&b"v0"[..]));
        assert_eq!(engine.get(b"k9").expect("get").as_deref(), Some(&b"v9"[..]));
        assert_eq!(
            engine.get(b"post-rotation").expect("get").as_deref(),
            Some(&b"still writable"[..])
        );
    }

    #[test]
    fn rotate_key_on_plaintext_store_is_not_encrypted_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut engine = Engine::open(dir.path()).expect("open plaintext");
        assert!(!engine.is_encrypted());
        let err = engine.rotate_key(b"whatever").expect_err("nothing to rotate");
        assert!(matches!(err, EngineError::NotEncrypted { .. }));
    }

    #[test]
    fn compact_now_merges_below_threshold_and_purges_tombstones() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut engine = Engine::open_encrypted_with_options(dir.path(), KEY, small_options()).expect("open");
        for i in 0..8u32 {
            engine
                .put(format!("key-{i:03}").as_bytes(), format!("value-{i}").as_bytes())
                .expect("put");
        }
        engine.delete(b"key-002").expect("delete");
        engine.delete(b"key-005").expect("delete");
        // Leave an unflushed tail too: compact_now must fold it in.
        engine.put(b"tail", b"unflushed").expect("put tail");

        engine.compact_now().expect("compact");
        let stats = engine.stats().expect("stats");
        assert_eq!(stats.sst_count, 1, "everything folds into one SST");
        assert_eq!(stats.tombstone_count, 0, "a full merge drops every tombstone");
        assert_eq!(stats.wal_bytes, 0, "flushed before compacting");

        drop(engine);
        let engine = Engine::open_encrypted_with_options(dir.path(), KEY, small_options()).expect("reopen");
        assert_eq!(engine.get(b"key-000").expect("get").as_deref(), Some(&b"value-0"[..]));
        assert_eq!(engine.get(b"key-002").expect("get"), None);
        assert_eq!(engine.get(b"key-005").expect("get"), None);
        assert_eq!(engine.get(b"tail").expect("get").as_deref(), Some(&b"unflushed"[..]));
    }

    #[test]
    fn compact_now_on_an_empty_store_is_a_noop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut engine = Engine::open(dir.path()).expect("open");
        engine.compact_now().expect("compact empty");
        let stats = engine.stats().expect("stats");
        assert_eq!(stats.sst_count, 0);
        assert_eq!(stats.compaction_count, 0, "nothing to compact, nothing counted");
    }

    #[test]
    fn rotation_orphan_tmp_is_ignored_on_reopen() {
        // A crash between tmp-write and rename during rotation leaves a
        // `crypto.meta.tmp` orphan; the store must reopen on the committed
        // wrap as if the rotation never started.
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let mut engine = Engine::open_encrypted(dir.path(), KEY).expect("open");
            engine.put(b"a", b"1").expect("put");
        }
        std::fs::write(dir.path().join("crypto.meta.tmp"), b"torn rotation garbage").expect("write orphan");
        let engine = Engine::open_encrypted(dir.path(), KEY).expect("reopen ignores the orphan");
        assert_eq!(engine.get(b"a").expect("get").as_deref(), Some(&b"1"[..]));
    }
}
