// SPDX-License-Identifier: BUSL-1.1
//! The public single-writer KV engine: WAL + memtable + SST, with crash
//! recovery on `open`. See the `store` module docs for the write-path
//! ordering guarantee.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::crypto::{self, CryptoContext};
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
    /// `Some` = encrypted at rest (ADR-030): WAL records and SST files are
    /// sealed under the store's DEK; `crypto.meta` holds the DEK wrapped by
    /// the user key.
    crypto: Option<CryptoContext>,
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
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_options(path, EngineOptions::default())
    }

    /// Same as [`Engine::open`] with explicit tunables.
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

        let ssts = sst::scan_existing(&dir, crypto.as_ref())?;
        let next_sst_id = ssts.iter().map(|s| s.id + 1).max().unwrap_or(0);

        let wal_path = dir.join("wal.log");
        let wal = Wal::open_for_append(wal_path, crypto.clone())?;
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
            crypto,
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
        let new_sst = SstFile::write_new(&self.dir, id, entries, self.crypto.as_ref())?;
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
        let new_sst = SstFile::write_new(&self.dir, id, entries, self.crypto.as_ref())?;
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

/// `true` if `dir` contains at least one `*.sst` file — the "existing
/// plaintext store" half of the mode check in `Engine::open_inner` (the
/// other half is `wal.log`'s existence).
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

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &[u8] = b"test user key";

    /// Options that force flush + compaction quickly, so the encrypted
    /// roundtrip exercises SST envelopes and compaction, not just the WAL.
    fn small_options() -> EngineOptions {
        EngineOptions {
            memtable_flush_threshold: 4,
            compaction_sst_threshold: 2,
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
