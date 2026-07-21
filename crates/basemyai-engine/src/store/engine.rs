// SPDX-License-Identifier: BUSL-1.1
//! The public single-writer KV engine: WAL + memtable + SST, with crash
//! recovery on `open`. See the `store` module docs for the write-path
//! ordering guarantee.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::crypto::{self, CryptoContext};
use crate::error::{EngineError, Result};
use crate::fail_point;
use crate::format::generation_meta;
use crate::format::sst_manifest;
use crate::format::store_meta::{self, StoreMeta};
use crate::format::wal::{BatchOp, WalOp};
use crate::key::Key;
use crate::store::Value;
use crate::store::block_cache::BlockCache;
use crate::store::memtable::Memtable;
use crate::store::sst_block::{self, BlockSstFile};
use crate::store::stats::{Counters, EngineStats};
use crate::store::version::{Snapshot, SstHandle, Version, VersionEdit};
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
    /// Stable store root. `dir` may move to `gen-N` after a full rotation,
    /// while root metadata and the writer lock always remain here.
    root_dir: PathBuf,
    dir: PathBuf,
    /// Logical generation selected from root `generation.meta`; legacy root
    /// stores remain generation zero until their first full DEK rotation.
    generation_id: u64,
    /// Held for this engine's full lifetime. Dropping the file releases the
    /// advisory OS lock, so no second writable opener can reach WAL/SST.
    _writer_lock: File,
    /// Stable identity persisted in `store.meta` (`StoreMeta:2`, ADR-042).
    store_id: uuid::Uuid,
    wal: Wal,
    memtable: Memtable,
    /// The published, immutable set of live SSTs (ADR-043 §2 amended, J3).
    /// Replaced wholesale by [`Self::apply_version_edit`] — never mutated
    /// in place (INV-VS-1/2). [`Self::snapshot`] pins it by cloning the
    /// `Arc`.
    current: Arc<Version>,
    next_sst_id: u64,
    options: EngineOptions,
    /// `Some` = encrypted at rest (ADR-030): WAL records and SST files are
    /// sealed under the store's DEK; `crypto.meta` holds the DEK wrapped by
    /// the user key.
    crypto: Option<CryptoContext>,
    /// Monotonic activity counters since open (N7.1) — the gauge half of
    /// [`EngineStats`] is derived from live state in [`Engine::stats`].
    counters: Counters,
    /// Bytes of orphaned `*.tmp` files found in `dir` at open time (R0,
    /// [`EngineStats::orphan_bytes`]) — a one-time snapshot, never
    /// refreshed for the life of this `Engine`.
    orphan_bytes_at_open: u64,
    /// Deferred SST removals that still failed after retries (ENG-DUR-002)
    /// — an `Arc<AtomicU64>` shared with every [`SstHandle`], because the
    /// failing attempt now runs at handle drop (possibly when a snapshot
    /// releases, outside any `&mut Engine` context), no longer inline in
    /// `compact()`. See [`EngineStats::compaction_remove_failures`].
    sst_remove_failures: Arc<AtomicU64>,
    /// Live [`Snapshot`]s not yet dropped (ENG-RES-003 / ADR-043 J3 exit
    /// criterion) — incremented by [`Self::snapshot`], decremented by
    /// `Snapshot::drop`. See [`EngineStats::active_snapshots`].
    active_snapshots: Arc<AtomicU64>,
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

    /// Opens (creating if absent) an encrypted store with a human
    /// passphrase. Fresh stores persist `CryptoMeta:2` in Argon2id mode;
    /// existing records must already declare that same mode, so raw keys and
    /// passphrases never silently substitute for one another (ADR-042).
    pub fn open_with_passphrase(path: impl AsRef<Path>, passphrase: &[u8]) -> Result<Self> {
        Self::open_with_passphrase_and_options(path, passphrase, EngineOptions::default())
    }

    /// Opens (or creates) a passphrase store using an explicit Argon2id cost
    /// profile when a new `crypto.meta` must be written. Existing stores
    /// always replay the parameters persisted in their metadata.
    pub fn open_with_passphrase_and_profile(
        path: impl AsRef<Path>,
        passphrase: &[u8],
        profile: crypto::Argon2idProfile,
    ) -> Result<Self> {
        Self::open_with_passphrase_profile_and_options(path, passphrase, profile, EngineOptions::default())
    }

    /// Same as [`Engine::open_encrypted`] with explicit tunables.
    pub fn open_encrypted_with_options(path: impl AsRef<Path>, key: &[u8], options: EngineOptions) -> Result<Self> {
        Self::open_inner(
            path.as_ref(),
            options,
            Some((key, crypto::KeyMode::RawKey, crypto::Argon2idProfile::Default)),
        )
    }

    /// Same as [`Engine::open_with_passphrase`] with explicit tunables.
    pub fn open_with_passphrase_and_options(
        path: impl AsRef<Path>,
        passphrase: &[u8],
        options: EngineOptions,
    ) -> Result<Self> {
        Self::open_with_passphrase_profile_and_options(path, passphrase, crypto::Argon2idProfile::Default, options)
    }

    fn open_with_passphrase_profile_and_options(
        path: impl AsRef<Path>,
        passphrase: &[u8],
        profile: crypto::Argon2idProfile,
        options: EngineOptions,
    ) -> Result<Self> {
        Self::open_inner(
            path.as_ref(),
            options,
            Some((passphrase, crypto::KeyMode::Passphrase, profile)),
        )
    }

    fn open_inner(
        path: &Path,
        options: EngineOptions,
        key: Option<(&[u8], crypto::KeyMode, crypto::Argon2idProfile)>,
    ) -> Result<Self> {
        let root_dir = path.to_path_buf();
        fs::create_dir_all(&root_dir).map_err(|e| EngineError::io(root_dir.clone(), e))?;

        // This lock deliberately precedes *all* store metadata checks and
        // mutations. In particular, legacy StoreMeta:1 upgrading must never
        // race another open which could stamp a different store_id.
        let writer_lock = acquire_writer_lock(&root_dir)?;

        // Store-generation check (N8.9, ADR-039 §7) — before touching
        // crypto/WAL/SST state at all: an incompatible or pre-ADR-039 store
        // must fail fast and typed, not as inexplicable corruption further
        // in. A genuinely fresh directory publishes a new `store.meta` here.
        let store_meta = check_or_create_store_meta(&root_dir)?;
        let store_id = store_meta
            .store_id
            .expect("check_or_create_store_meta always returns a stamped StoreMeta:2");

        let (dir, generation_id) = resolve_active_generation(&root_dir)?;

        // `crypto.meta`'s presence is the single source of truth for the
        // store's mode (ADR-030 §2) — never guessed from file contents.
        let meta_exists = crypto::crypto_meta_path(&dir).exists();
        if generation_id != 0 && !meta_exists {
            return Err(EngineError::CorruptCryptoMeta {
                path: crypto::crypto_meta_path(&dir),
                reason: "published generation is missing crypto.meta".to_string(),
            });
        }
        let crypto = match (meta_exists, key) {
            (true, Some((key, crypto::KeyMode::RawKey, _))) if generation_id == 0 => {
                Some(crypto::load_meta(&dir, key)?)
            }
            (true, Some((key, crypto::KeyMode::RawKey, _))) => Some(crypto::load_meta_for_generation(
                &dir,
                key,
                crypto::KeyMode::RawKey,
                generation_id,
            )?),
            (true, Some((key, crypto::KeyMode::Passphrase, _))) => Some(crypto::load_meta_for_generation(
                &dir,
                key,
                crypto::KeyMode::Passphrase,
                generation_id,
            )?),
            (true, None) => return Err(EngineError::MissingEncryptionKey { path: dir }),
            (false, Some((key, mode, profile))) => {
                // Refuse to mix modes: a directory that already holds
                // plaintext artifacts cannot be encrypted a posteriori.
                let has_wal = dir.join("wal.log").exists();
                let has_sst = sst_files_present(&dir)?;
                if has_wal || has_sst {
                    return Err(EngineError::PlaintextStoreKeySupplied { path: dir });
                }
                Some(match (mode, generation_id) {
                    (crypto::KeyMode::RawKey, 0) => crypto::create_meta(&dir, key)?,
                    (mode, generation_id) => {
                        crypto::create_meta_for_generation_with_profile(&dir, key, mode, profile, generation_id)?
                    }
                })
            }
            (false, None) => None,
        };

        let scanned = sst_block::scan_existing(&dir, crypto.as_ref())?;
        // `next_sst_id` derives from the *unfiltered* scan — including any
        // id `confront_manifest_with_disk` is about to drop as an orphan —
        // so a fresh SST never reuses an id an orphan file still occupies
        // on disk (ENG-DUR-002).
        let next_sst_id = scanned.iter().map(|s| s.id + 1).max().unwrap_or(0);
        let (ssts, manifest_generation) = confront_manifest_with_disk(&dir, scanned)?;
        // Observation only (R0): counts pre-existing `*.tmp` orphans without
        // touching or removing them — `scan_existing`/`sst_files_present`
        // above already ignored them exactly as before this counter existed.
        let orphan_bytes_at_open = scan_orphan_tmp_bytes(&dir)?;

        // Everything loaded at open was read from disk: the O(metadata)
        // bytes each SST's lazy `load` actually reads (N8.4 — never the
        // whole file), plus the WAL bytes replayed just below.
        let mut counters = Counters {
            bytes_read: ssts.iter().map(|s| s.bytes_read_at_open).sum(),
            ..Counters::default()
        };

        let sst_remove_failures = Arc::new(AtomicU64::new(0));
        let current = Arc::new(Version {
            manifest_generation,
            ssts: ssts
                .into_iter()
                .map(|file| SstHandle::new(file, Arc::clone(&sst_remove_failures)))
                .collect(),
        });

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
        gc_inactive_generations(&root_dir, generation_id);
        Ok(Self {
            root_dir,
            dir,
            generation_id,
            _writer_lock: writer_lock,
            store_id,
            wal,
            memtable,
            current,
            next_sst_id,
            options,
            crypto,
            counters,
            orphan_bytes_at_open,
            sst_remove_failures,
            active_snapshots: Arc::new(AtomicU64::new(0)),
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
            sst_count: self.current.ssts.len(),
            sst_bytes: self.current.ssts.iter().map(|h| h.file.file_bytes).sum(),
            tombstone_count: memtable_tombstones + self.current.ssts.iter().map(|h| h.file.tombstones).sum::<u64>(),
            flush_count: self.counters.flush_count,
            compaction_count: self.counters.compaction_count,
            compaction_input_bytes: self.counters.compaction_input_bytes,
            compaction_output_bytes: self.counters.compaction_output_bytes,
            bytes_read: self.counters.bytes_read,
            bytes_written: self.counters.bytes_written,
            block_cache_hits: self.block_cache.hits(),
            block_cache_misses: self.block_cache.misses(),
            point_lookup_full_sst_read: self.point_lookup_full_sst_read.load(Ordering::Relaxed),
            fsync_count: self.counters.fsync_count + self.wal.fsync_count(),
            orphan_bytes: self.orphan_bytes_at_open,
            compaction_remove_failures: self.sst_remove_failures.load(Ordering::Relaxed),
            manifest_generation: self.current.manifest_generation,
            active_snapshots: self.active_snapshots.load(Ordering::Relaxed),
        })
    }

    /// `true` if this instance is encrypted at rest (ADR-030).
    #[must_use]
    pub fn is_encrypted(&self) -> bool {
        self.crypto.is_some()
    }

    /// Stable identifier of this store, persisted in `store.meta` and safe to
    /// use as an external account name (it is an identifier, not a secret).
    #[must_use]
    pub fn store_id(&self) -> uuid::Uuid {
        self.store_id
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
        self.rotate_key_with_mode(new_key, crypto::KeyMode::RawKey, crypto::Argon2idProfile::Default)
    }

    /// Passphrase counterpart to [`Self::rotate_key`]. The existing DEK is
    /// re-wrapped under an Argon2id-derived KEK without rewriting data files.
    pub fn rotate_passphrase(&mut self, new_passphrase: &[u8]) -> Result<()> {
        self.rotate_passphrase_with_profile(new_passphrase, crypto::Argon2idProfile::Default)
    }

    /// Re-wraps the existing DEK with a passphrase under an explicit Argon2id
    /// profile. The profile must be repeated at each rotation that should keep
    /// using the constrained-hardware parameters (ADR-042).
    pub fn rotate_passphrase_with_profile(
        &mut self,
        new_passphrase: &[u8],
        profile: crypto::Argon2idProfile,
    ) -> Result<()> {
        self.rotate_key_with_mode(new_passphrase, crypto::KeyMode::Passphrase, profile)
    }

    fn rotate_key_with_mode(
        &mut self,
        new_key: &[u8],
        mode: crypto::KeyMode,
        profile: crypto::Argon2idProfile,
    ) -> Result<()> {
        let Some(crypto) = &self.crypto else {
            return Err(EngineError::NotEncrypted { path: self.dir.clone() });
        };
        crypto::write_meta_with_mode_for_generation_and_profile(
            &self.dir,
            new_key,
            crypto,
            mode,
            profile,
            self.generation_id,
        )
    }

    /// Re-encrypts every live record under a fresh DEK and atomically makes
    /// the resulting generation current (ADR-042). Unlike [`Self::rotate_key`],
    /// this is an O(store size) full merge and removes tombstones and shadowed
    /// records from the active generation.
    ///
    /// # Errors
    /// [`EngineError::NotEncrypted`] for a plaintext store, plus I/O or
    /// corruption errors encountered while reading or publishing the new
    /// generation. Before pointer publication an error leaves this instance
    /// and the active generation unchanged.
    pub fn rotate_key_full(&mut self, new_key: &[u8]) -> Result<()> {
        self.rotate_full(new_key, crypto::KeyMode::RawKey, crypto::Argon2idProfile::Default)
    }

    /// Passphrase counterpart to [`Self::rotate_key_full`]. The fresh DEK is
    /// wrapped by an Argon2id-derived KEK persisted in `CryptoMeta:2`.
    pub fn rotate_passphrase_full(&mut self, new_passphrase: &[u8]) -> Result<()> {
        self.rotate_passphrase_full_with_profile(new_passphrase, crypto::Argon2idProfile::Default)
    }

    /// Full-DEK counterpart to [`Self::rotate_passphrase_with_profile`].
    pub fn rotate_passphrase_full_with_profile(
        &mut self,
        new_passphrase: &[u8],
        profile: crypto::Argon2idProfile,
    ) -> Result<()> {
        self.rotate_full(new_passphrase, crypto::KeyMode::Passphrase, profile)
    }

    fn rotate_full(&mut self, new_key: &[u8], mode: crypto::KeyMode, profile: crypto::Argon2idProfile) -> Result<()> {
        if self.crypto.is_none() {
            return Err(EngineError::NotEncrypted { path: self.dir.clone() });
        }

        let next_generation = self
            .generation_id
            .checked_add(1)
            .ok_or_else(|| EngineError::CorruptGenerationMeta {
                path: self.root_dir.join(generation_meta::GENERATION_META_FILENAME),
                reason: "active generation id cannot be incremented".to_string(),
            })?;
        let next_dir = generation_dir(&self.root_dir, next_generation);

        // A pre-publication crash may leave this exact directory behind.
        // Never reuse its crypto.meta/DEK: remove it completely and create a
        // fresh generation from scratch.
        if next_dir.exists() {
            fs::remove_dir_all(&next_dir).map_err(|e| EngineError::io(next_dir.clone(), e))?;
        }
        fs::create_dir(&next_dir).map_err(|e| EngineError::io(next_dir.clone(), e))?;

        let build = (|| -> Result<(CryptoContext, Option<BlockSstFile>, Wal)> {
            let new_crypto =
                crypto::create_meta_for_generation_with_profile(&next_dir, new_key, mode, profile, next_generation)?;
            fail_point!("after_full_rotation_new_dek");

            // Same precedence as reads/compaction: old SSTs first, newest
            // layers overwrite them, and the WAL-replayed memtable wins last.
            // This directly folds the unflushed tail into the output, so no
            // intermediate old-DEK SST and no WAL re-sealing pass is needed.
            let mut merged: BTreeMap<Key, Option<Value>> = BTreeMap::new();
            for h in &self.current.ssts {
                for (key, value) in h.file.entries()? {
                    merged.insert(key, value);
                }
            }
            for (key, value) in self.memtable.iter() {
                merged.insert(key.clone(), value.clone());
            }
            let entries: Vec<(Key, Option<Value>)> = merged.into_iter().filter(|(_, value)| value.is_some()).collect();
            let new_sst = if entries.is_empty() {
                None
            } else {
                let sst = BlockSstFile::write_new(&next_dir, 0, entries, self.options.block_size, Some(&new_crypto))?;
                fail_point!("after_full_rotation_sst_write");
                Some(sst)
            };

            // ENG-DUR-001: `next_dir` gets its own manifest, listing its
            // (at most one) merged SST — part of the same all-or-nothing
            // build as the rest of this closure. If anything below fails,
            // the existing `remove_dir_all(&next_dir)` error path (below)
            // discards this manifest along with everything else half-built.
            publish_sst_manifest(&next_dir, 0, &new_sst.iter().map(|s| s.id).collect::<Vec<_>>())?;

            // The new generation is published only after even its empty WAL
            // exists durably. Keep this handle ready for the live-state swap.
            let wal_path = next_dir.join("wal.log");
            let wal_file = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&wal_path)
                .map_err(|e| EngineError::io(wal_path.clone(), e))?;
            wal_file.sync_all().map_err(|e| EngineError::io(wal_path.clone(), e))?;
            drop(wal_file);
            let new_wal = Wal::open_for_append(wal_path, Some(new_crypto.clone()))?;
            fail_point!("before_full_rotation_publish");
            Ok((new_crypto, new_sst, new_wal))
        })();

        let (new_crypto, new_sst, new_wal) = match build {
            Ok(build) => build,
            Err(error) => {
                let _ = fs::remove_dir_all(&next_dir);
                return Err(error);
            }
        };

        publish_generation(&self.root_dir, next_generation)?;

        // From this point forward the in-memory writer must follow the
        // published pointer. Every operation below is infallible; notably,
        // the new WAL handle was opened before publication.
        let old_dir = std::mem::replace(&mut self.dir, next_dir);
        self.generation_id = next_generation;
        let old_wal = std::mem::replace(&mut self.wal, new_wal);
        // The retired `Wal` never counts its fsyncs anywhere else — fold
        // them into `Counters` now, or they vanish once `self.wal` (fresh,
        // starting from zero) replaces it as the source `Engine::stats`
        // reads from.
        self.counters.fsync_count += old_wal.fsync_count();
        drop(old_wal); // mandatory before remove_dir_all on Windows
        // `next_dir`'s manifest was already published inside `build` above
        // (generation 0, listing this same 0-or-1 merged SST) — the new
        // `Version` mirrors it so `Engine::stats`/future flushes agree with
        // what's on disk. The old generation's handles are deliberately
        // *not* retired: their whole directory is GC'd wholesale below
        // (`gc_old_generation`), never file by file — a `Snapshot` taken
        // before a full rotation does not survive it (typed I/O error on
        // its next read, per ADR-043 §2 amended).
        let new_version = Arc::new(Version {
            manifest_generation: 0,
            ssts: new_sst
                .into_iter()
                .map(|file| SstHandle::new(file, Arc::clone(&self.sst_remove_failures)))
                .collect(),
        });
        let old_version = std::mem::replace(&mut self.current, new_version);
        let input_bytes = old_version.ssts.iter().map(|h| h.file.file_bytes).sum::<u64>();
        let output_bytes = self.current.ssts.iter().map(|h| h.file.file_bytes).sum::<u64>();
        for old in &old_version.ssts {
            self.block_cache.invalidate_sst(old.file.id);
        }
        drop(old_version);
        self.memtable.clear();
        self.next_sst_id = u64::from(!self.current.ssts.is_empty());
        self.crypto = Some(new_crypto);
        self.counters.compaction_count += 1;
        self.counters.compaction_input_bytes += input_bytes;
        self.counters.compaction_output_bytes += output_bytes;
        self.counters.bytes_written += output_bytes;

        fail_point!("after_full_rotation_publish");
        gc_old_generation(&self.root_dir, &old_dir, next_generation);
        Ok(())
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
        for h in self.current.ssts.iter().rev() {
            let (hit, blocks_read) = h.file.get(&key, &self.block_cache)?;
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
        for h in &self.current.ssts {
            let (matches, _blocks_read) = h.file.entries_with_prefix(prefix)?;
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
        for h in &self.current.ssts {
            let (matches, _blocks_read) = h.file.entries_with_range(start, end)?;
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
        for h in &self.current.ssts {
            let (matches, truncated, _blocks_read) = h.file.entries_with_range_limited(start, end, limit)?;
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
        // Claim the id as soon as the file exists on disk — same placement
        // as `compact()`. Incrementing only after `wal.reset()` (the old
        // ordering) would let a caller that keeps using this instance after
        // a reset error re-`write_new` the same id, overwriting a file the
        // published version already reads.
        self.next_sst_id += 1;
        self.counters.flush_count += 1;
        self.counters.bytes_written += new_sst.file_bytes;
        self.counters.fsync_count += 1; // write_new's one sync_all of the new SST

        // ENG-DUR-001: publish the manifest listing this new SST *before*
        // truncating the WAL — SST → manifest → WAL truncate, in that exact
        // order (the roadmap's own ordering rule for J2). If a crash lands
        // before the manifest is durable, the WAL still holds the untouched
        // tail to replay from, and `confront_manifest_with_disk` correctly
        // treats the new SST as not-yet-live (dropped as an orphan) on
        // reopen — never a case where truncated data depended on a
        // manifest entry that was never published. A flush is the edit
        // `{ added: [S_new], deleted: [] }` (ADR-043 §2 amended).
        self.apply_version_edit(VersionEdit {
            added: vec![SstHandle::new(new_sst, Arc::clone(&self.sst_remove_failures))],
            deleted: Vec::new(),
        })?;

        // The new SST *and* its manifest entry are fsynced and durably
        // renamed at this point — only now is it safe to truncate the WAL
        // (ADR-025 ordering rule).
        fail_point!("before_wal_truncate");
        self.wal.reset()?;

        self.memtable.clear();

        if self.current.ssts.len() > self.options.compaction_sst_threshold {
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
        if self.current.ssts.is_empty() {
            return Ok(());
        }
        self.compact()
    }

    /// Pins the current version as a stable read view — an **S1 snapshot**
    /// (ADR-043 §2 amended, audit §6): it freezes the *files*, not the
    /// *view*. The memtable is not captured — an unflushed write present at
    /// snapshot time is invisible through the snapshot, and writes/flushes/
    /// compactions after it stay visible through this `Engine` only. What
    /// it guarantees: every SST of the pinned version remains on disk and
    /// readable for the snapshot's whole lifetime, however many compactions
    /// supersede it (deferred physical removal, INV-VS-6). Cost: one `Arc`
    /// clone — no lock held beyond this call, no data copied.
    ///
    /// A snapshot does not usefully survive [`Self::rotate_key_full`] (the
    /// old generation directory is GC'd wholesale; later reads fail with a
    /// typed I/O error) nor a drop-and-reopen of the `Engine` (the reopen
    /// sweeps the pinned files as manifest orphans).
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        Snapshot::new(Arc::clone(&self.current), Arc::clone(&self.active_snapshots))
    }

    /// Naive full-merge compaction: folds every existing SST (oldest to
    /// newest, later writes win) into a single new SST, dropping tombstones
    /// entirely — safe because this merge covers *all* existing data, so a
    /// deleted key has no older layer left to resurrect from. Correctness
    /// first; a tiered/leveled strategy is deferred (ADR-025).
    fn compact(&mut self) -> Result<()> {
        fail_point!("during_compaction");
        // The merge's input set. Under J3 flush/compaction still run under
        // `&mut self`, so this is trivially the version the edit below
        // commits against; under J4 the merge runs off-lock from this pinned
        // snapshot while `self.current` may advance (a concurrent flush) —
        // the edit's `deleted` names exactly these inputs, so anything
        // published in between survives (INV-VS-5, ENG-COR-001).
        let input = Arc::clone(&self.current);
        let input_bytes: u64 = input.ssts.iter().map(|h| h.file.file_bytes).sum();
        let mut merged: BTreeMap<Key, Option<Value>> = BTreeMap::new();
        for h in &input.ssts {
            for (k, v) in h.file.entries()? {
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
        self.counters.fsync_count += 1; // write_new's one sync_all of the merged output SST

        // ENG-DUR-001/ENG-DUR-002: the edit publishes the manifest *before*
        // any old file can be removed — from that point a leftover old SST
        // is an orphan per the manifest, never resurrectable as live on a
        // future reopen. Physical removal itself is deferred to each
        // handle's last-`Arc` drop (INV-VS-6): with no snapshot alive that
        // happens synchronously inside this call (the old version's only
        // reference is `self.current`, replaced by the edit), preserving
        // the historical inline-removal timing; with a snapshot alive the
        // files persist, still readable, until the snapshot drops.
        let deleted: Vec<u64> = input.ssts.iter().map(|h| h.file.id).collect();
        drop(input);
        self.apply_version_edit(VersionEdit {
            added: vec![SstHandle::new(new_sst, Arc::clone(&self.sst_remove_failures))],
            deleted,
        })?;
        Ok(())
    }

    /// Publishes `edit` (ADR-043 §2 amended, J3): validates that every
    /// `deleted` id is present in the current version (INV-VS-4), computes
    /// `V_next = (V_current ∖ deleted) ∪ added`, publishes `manifest.meta`
    /// listing exactly `V_next` (INV-VS-7), then atomically replaces
    /// `self.current` (INV-VS-2) after retiring the deleted handles for
    /// deferred physical removal (INV-VS-6). On error nothing is published
    /// and `self.current` is unchanged.
    ///
    /// Under J3's exclusive `&mut self` the current version cannot move
    /// between the caller reading it and this commit; the validation exists
    /// so J4's out-of-lock compaction fails typed instead of publishing a
    /// manifest that silently drops a concurrently-flushed SST
    /// (ENG-COR-001).
    fn apply_version_edit(&mut self, edit: VersionEdit) -> Result<()> {
        let current_ids: std::collections::HashSet<u64> = self.current.ssts.iter().map(|h| h.file.id).collect();
        for id in &edit.deleted {
            if !current_ids.contains(id) {
                return Err(EngineError::VersionEditMissingInput { id: *id });
            }
        }
        let deleted: std::collections::HashSet<u64> = edit.deleted.iter().copied().collect();
        let mut next_ssts: Vec<_> = self
            .current
            .ssts
            .iter()
            .filter(|h| !deleted.contains(&h.file.id))
            .map(Arc::clone)
            .collect();
        next_ssts.extend(edit.added);
        let next = Version {
            manifest_generation: self.current.manifest_generation + 1,
            ssts: next_ssts,
        };
        publish_sst_manifest(&self.dir, next.manifest_generation, &next.ids())?;
        self.counters.fsync_count += 1; // publish_sst_manifest's one sync_all

        // The manifest no longer lists the deleted ids: retire their
        // handles (their files are removed at last-`Arc` drop) and evict
        // their cached blocks now — the cache belongs to the live view, and
        // a snapshot re-reading a retired SST uses its own private cache.
        for h in &self.current.ssts {
            if deleted.contains(&h.file.id) {
                self.block_cache.invalidate_sst(h.file.id);
                h.retire();
            }
        }
        // Single visibility point (INV-VS-2). Dropping the old version's
        // `Arc` here is what triggers the synchronous removal in the
        // no-snapshot case.
        self.current = Arc::new(next);
        Ok(())
    }
}

impl Engine {
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

/// Sums the on-disk size of every `*.tmp` file directly inside `dir` — the
/// [`EngineStats::orphan_bytes`] observation (R0): `*.sst.tmp`,
/// `crypto.meta.tmp`, `generation.meta.tmp`, `store.meta.tmp` left behind by
/// a crash mid atomic-replace. Purely observational: does not remove or
/// otherwise touch any file, and does not change which artifacts
/// `scan_existing`/`sst_files_present` treat as live (they already skip
/// `.tmp` files by extension, unaffected by this function).
fn scan_orphan_tmp_bytes(dir: &Path) -> Result<u64> {
    if !dir.exists() {
        return Ok(0);
    }
    let mut total = 0u64;
    for entry in fs::read_dir(dir).map_err(|e| EngineError::io(dir.to_path_buf(), e))? {
        let entry = entry.map_err(|e| EngineError::io(dir.to_path_buf(), e))?;
        let path = entry.path();
        let is_tmp = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".tmp"));
        if is_tmp {
            let metadata = entry.metadata().map_err(|e| EngineError::io(path.clone(), e))?;
            total += metadata.len();
        }
    }
    Ok(total)
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
fn acquire_writer_lock(dir: &Path) -> Result<File> {
    let path = dir.join(".basemyai.lock");
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        // This stable lock file has no payload. Never truncate it while a
        // previous opener may still hold the OS-level advisory lock.
        .truncate(false)
        .open(&path)
        .map_err(|e| EngineError::io(path.clone(), e))?;
    match file.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            return Err(EngineError::StoreLocked {
                path: dir.to_path_buf(),
            });
        }
        Err(std::fs::TryLockError::Error(error)) => return Err(EngineError::io(path, error)),
    }
    Ok(file)
}

fn generation_dir(root_dir: &Path, generation_id: u64) -> PathBuf {
    root_dir.join(format!("gen-{generation_id}"))
}

fn publish_generation(root_dir: &Path, generation_id: u64) -> Result<()> {
    let final_path = root_dir.join(generation_meta::GENERATION_META_FILENAME);
    let tmp_path = final_path.with_extension("meta.tmp");
    let bytes = generation_meta::encode(&generation_meta::GenerationMeta {
        current_generation: generation_id,
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
    fs::rename(&tmp_path, &final_path).map_err(|e| EngineError::io(final_path, e))?;
    // ENG-DUR-003/004: the generation pointer's rename must be durable
    // before any caller (`rotate_key_full`) acts on the strength of it —
    // notably the old-generation GC that follows immediately after.
    crate::fs_util::sync_dir(root_dir)?;
    Ok(())
}

/// Publishes `dir`'s durable SST manifest (ENG-DUR-001): tmp+fsync+rename,
/// then [`crate::fs_util::sync_dir`] (ENG-DUR-003) so the rename itself
/// survives a crash before any caller acts on the strength of it. Called
/// after `live_sst_ids`' SSTs are themselves already fsynced and durably
/// renamed — the manifest is the last thing published in a flush/compaction,
/// never the first (same ordering discipline as WAL-after-SST, ADR-025).
fn publish_sst_manifest(dir: &Path, manifest_generation: u64, live_sst_ids: &[u64]) -> Result<()> {
    let final_path = dir.join(sst_manifest::SST_MANIFEST_FILENAME);
    let tmp_path = final_path.with_extension("meta.tmp");
    let bytes = sst_manifest::encode(&sst_manifest::SstManifest {
        manifest_generation,
        live_sst_ids: live_sst_ids.to_vec(),
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
    fail_point!("before_sst_manifest_publish");
    fs::rename(&tmp_path, &final_path).map_err(|e| EngineError::io(final_path, e))?;
    crate::fs_util::sync_dir(dir)?;
    Ok(())
}

/// Confronts `scan_existing`'s result against `dir`'s durable manifest
/// (ENG-DUR-001, `docs/audits/2026-07-engine-architecture-safety-audit.md`).
/// Returns the live subset of `found` (orphans dropped) and the manifest's
/// current publication counter.
///
/// - **Manifest absent**: bootstrap — publish one now listing exactly
///   `found`'s ids (generation 0), keep everything `found` contains. Under
///   `STORE_FORMAT_VERSION` 3, `check_or_create_store_meta` already rejects
///   any store built before this milestone, so the only way to reach this
///   branch is a genuinely fresh store, or a crash between a flush/
///   compaction's SST publish and the manifest publish meant to record it
///   — both are safe to treat as "adopt what's on disk as the live set
///   right now" (a deliberate simplification over the ADR-043 §1 draft's
///   additive-migration proposal, enabled by the version bump — see the
///   design note in that ADR).
/// - **Manifest present**: an id it lists but `found` doesn't contain is
///   [`EngineError::MissingLiveSst`] — a live SST silently missing from
///   disk, closing the N11.3 gap
///   (`corruption_smoke.rs::deleted_sst_is_detected_once_catalog_lands`).
///   An id `found` contains but the manifest doesn't list is an orphan:
///   dropped from the returned set and removed best-effort — a failed
///   removal here is inert, not a resurrection risk, because the manifest
///   (not the directory listing) decides liveness from here on.
fn confront_manifest_with_disk(dir: &Path, found: Vec<BlockSstFile>) -> Result<(Vec<BlockSstFile>, u64)> {
    let manifest_path = dir.join(sst_manifest::SST_MANIFEST_FILENAME);
    if !manifest_path.exists() {
        let live_sst_ids: Vec<u64> = found.iter().map(|s| s.id).collect();
        publish_sst_manifest(dir, 0, &live_sst_ids)?;
        return Ok((found, 0));
    }
    let bytes = fs::read(&manifest_path).map_err(|e| EngineError::io(manifest_path.clone(), e))?;
    let manifest = sst_manifest::decode(&bytes, &manifest_path)?;

    let found_ids: std::collections::HashSet<u64> = found.iter().map(|s| s.id).collect();
    for id in &manifest.live_sst_ids {
        if !found_ids.contains(id) {
            return Err(EngineError::MissingLiveSst {
                id: *id,
                path: sst_block::sst_path(dir, *id),
            });
        }
    }

    let live: std::collections::HashSet<u64> = manifest.live_sst_ids.iter().copied().collect();
    let mut kept = Vec::with_capacity(found.len());
    for sst in found {
        if live.contains(&sst.id) {
            kept.push(sst);
        } else {
            let _ = fs::remove_file(&sst.path);
        }
    }
    Ok((kept, manifest.manifest_generation))
}

fn gc_old_generation(root_dir: &Path, old_dir: &Path, current_generation: u64) {
    if old_dir == root_dir {
        let _ = fs::remove_file(root_dir.join("wal.log"));
        hit_infallible_failpoint("during_full_rotation_gc");
        let _ = fs::remove_file(crypto::crypto_meta_path(root_dir));
        let _ = fs::remove_file(root_dir.join("crypto.meta.tmp"));
        if let Ok(entries) = fs::read_dir(root_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
                if path.extension().and_then(|extension| extension.to_str()) == Some("sst")
                    || name.ends_with(".sst.tmp")
                {
                    let _ = fs::remove_file(path);
                }
            }
        }
    } else if old_dir != generation_dir(root_dir, current_generation) {
        if let Ok(mut entries) = fs::read_dir(old_dir)
            && let Some(Ok(entry)) = entries.next()
        {
            let path = entry.path();
            if path.is_dir() {
                let _ = fs::remove_dir_all(path);
            } else {
                let _ = fs::remove_file(path);
            }
            hit_infallible_failpoint("during_full_rotation_gc");
        }
        let _ = fs::remove_dir_all(old_dir);
    }
}

#[cfg(any(test, feature = "test-util"))]
fn hit_infallible_failpoint(name: &'static str) {
    // Post-publication GC is deliberately best-effort and cannot return an
    // error to the caller. Abort actions still terminate inside `hit`; an
    // injected ordinary error is ignored to preserve that contract.
    let _ = crate::failpoint::hit(name);
}

#[cfg(not(any(test, feature = "test-util")))]
fn hit_infallible_failpoint(_name: &'static str) {}

fn gc_inactive_generations(root_dir: &Path, current_generation: u64) {
    let Ok(entries) = fs::read_dir(root_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(id) = name.strip_prefix("gen-").and_then(|id| id.parse::<u64>().ok()) else {
            continue;
        };
        if id != current_generation {
            let _ = fs::remove_dir_all(path);
        }
    }
    if current_generation != 0 {
        gc_old_generation(root_dir, root_dir, current_generation);
    }
}

/// True if `root_dir` contains at least one `gen-<id>` directory. Used only
/// by [`resolve_active_generation`] to tell a genuinely fresh/legacy store
/// (no pointer, no generation directories) apart from a store whose
/// generation pointer was lost while real data still lives under `gen-N`
/// (ENG-DUR-004, `docs/audits/2026-07-engine-architecture-safety-audit.md`).
fn any_generation_dir_present(root_dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(root_dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix("gen-"))
            .is_some_and(|id| id.parse::<u64>().is_ok())
    })
}

/// True if `root_dir` itself still holds its own generation-0 artifacts
/// (`wal.log`, `crypto.meta`, or any `*.sst`). Distinguishes the two ways a
/// `gen-N` directory can sit next to a missing pointer:
///
/// - A **full rotation aborted before `publish_generation`** (e.g. killed at
///   `after_full_rotation_new_dek`) leaves a half-built `gen-N` next to a
///   root that is still fully intact — generation 0 never stopped being
///   live, and `gc_inactive_generations` sweeping the incomplete `gen-N`
///   away at the end of a normal open is the correct, already-tested
///   recovery (`full_rotation_abort_boundaries_keep_the_published_generation_healthy`).
/// - A **lost pointer rename after a completed rotation** (ENG-DUR-004's
///   actual danger) leaves root *empty* — `gc_old_generation` already swept
///   `wal.log`/`crypto.meta`/`*.sst` out of it, durably, as part of the very
///   rotation whose pointer publication didn't survive. Only this case is
///   the impossible, refuse-typed state.
fn root_generation_zero_artifacts_present(root_dir: &Path) -> Result<bool> {
    Ok(
        root_dir.join("wal.log").exists()
            || crypto::crypto_meta_path(root_dir).exists()
            || sst_files_present(root_dir)?,
    )
}

/// Resolves the only data directory an opener may inspect. Before a full
/// rotation there is no pointer and the root is the legacy logical generation
/// zero. Once a pointer exists, arbitrary root artifacts and sibling
/// generations are ignored; only `gen-<current>` is active.
fn resolve_active_generation(root_dir: &Path) -> Result<(PathBuf, u64)> {
    let pointer_path = root_dir.join(generation_meta::GENERATION_META_FILENAME);
    if !pointer_path.exists() {
        // ENG-DUR-004: a missing pointer next to a live `gen-N` *and* a root
        // with no generation-0 artifacts of its own is an impossible state
        // outside a crash mid-rotation *after* the old generation was
        // already swept — treating it as "generation 0" would make the
        // unconditional post-open GC (`gc_inactive_generations`) delete the
        // only real generation, destroying the store it was meant to
        // protect. Refuse instead of "repairing". A `gen-N` next to a root
        // that *still* has its own artifacts is the ordinary "rotation
        // aborted before publish" case — root is still genuinely live, and
        // ignoring the half-built `gen-N` (swept up by the routine
        // post-open GC below) is correct, not a state to refuse.
        if any_generation_dir_present(root_dir) && !root_generation_zero_artifacts_present(root_dir)? {
            return Err(EngineError::CorruptGenerationMeta {
                path: pointer_path,
                reason: "generation pointer missing, a gen-N directory exists, and the root has no \
                         generation-0 artifacts of its own — refusing to treat this store as a \
                         fresh generation 0"
                    .to_string(),
            });
        }
        return Ok((root_dir.to_path_buf(), 0));
    }
    let bytes = fs::read(&pointer_path).map_err(|e| EngineError::io(pointer_path.clone(), e))?;
    let meta = generation_meta::decode(&bytes, &pointer_path)?;
    if meta.current_generation == 0 {
        return Err(EngineError::CorruptGenerationMeta {
            path: pointer_path,
            reason: "generation.meta must point to a non-zero gen-N directory".to_string(),
        });
    }
    let active_dir = root_dir.join(format!("gen-{}", meta.current_generation));
    if !active_dir.is_dir() {
        return Err(EngineError::CorruptGenerationMeta {
            path: pointer_path,
            reason: format!("active generation directory {} is missing", active_dir.display()),
        });
    }
    Ok((active_dir, meta.current_generation))
}

fn check_or_create_store_meta(dir: &Path) -> Result<StoreMeta> {
    let meta_path = dir.join("store.meta");
    if meta_path.exists() {
        let bytes = fs::read(&meta_path).map_err(|e| EngineError::io(meta_path.clone(), e))?;
        let mut meta = store_meta::decode(&bytes, &meta_path)?;
        if meta.store_format_version != store_meta::STORE_FORMAT_VERSION {
            return Err(EngineError::UnsupportedStoreFormat {
                path: dir.to_path_buf(),
                expected: store_meta::STORE_FORMAT_VERSION,
                found: meta.store_format_version,
            });
        }
        if meta.store_id.is_none() {
            meta.store_id = Some(uuid::Uuid::now_v7());
            write_store_meta(&meta_path, &meta)?;
        }
        return Ok(meta);
    }

    let has_generation_dir = fs::read_dir(dir)
        .map_err(|e| EngineError::io(dir.to_path_buf(), e))?
        .filter_map(std::result::Result::ok)
        .any(|entry| {
            entry.path().is_dir()
                && entry
                    .file_name()
                    .to_str()
                    .and_then(|name| name.strip_prefix("gen-"))
                    .is_some_and(|id| id.parse::<u64>().is_ok())
        });
    let has_old_artifacts = dir.join("wal.log").exists()
        || crypto::crypto_meta_path(dir).exists()
        || dir.join(generation_meta::GENERATION_META_FILENAME).exists()
        || has_generation_dir
        || sst_files_present(dir)?;
    if has_old_artifacts {
        return Err(EngineError::UnsupportedStoreFormat {
            path: dir.to_path_buf(),
            expected: store_meta::STORE_FORMAT_VERSION,
            found: 0, // sentinel: no store.meta at all (pre-ADR-039 store)
        });
    }

    let meta = StoreMeta {
        store_format_version: store_meta::STORE_FORMAT_VERSION,
        store_id: Some(uuid::Uuid::now_v7()),
    };
    write_store_meta(&meta_path, &meta)?;
    Ok(meta)
}

fn write_store_meta(meta_path: &Path, meta: &StoreMeta) -> Result<()> {
    let tmp_path = meta_path.with_extension("meta.tmp");
    let bytes = store_meta::encode(meta);
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
    fs::rename(&tmp_path, meta_path).map_err(|e| EngineError::io(meta_path, e))?;
    // ENG-DUR-003: see `crate::fs_util`. `store.meta` has no meaningful
    // parent besides the store root itself.
    if let Some(dir) = meta_path.parent() {
        crate::fs_util::sync_dir(dir)?;
    }
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
    fn legacy_store_meta_is_stamped_with_a_stable_id_under_the_writer_lock() {
        let dir = tempfile::tempdir().expect("tempdir");
        let meta_path = dir.path().join("store.meta");
        let legacy = StoreMeta {
            store_format_version: store_meta::STORE_FORMAT_VERSION,
            store_id: None,
        };
        fs::write(&meta_path, store_meta::encode(&legacy)).expect("write legacy StoreMeta:1");

        let engine = Engine::open_encrypted(dir.path(), KEY).expect("open upgrades StoreMeta:1");
        let decoded = store_meta::decode(&fs::read(&meta_path).expect("read upgraded store.meta"), &meta_path)
            .expect("decode upgraded StoreMeta:2");
        assert_eq!(decoded.store_id, Some(engine.store_id()));
    }

    #[test]
    fn generation_pointer_never_falls_back_to_root_artifacts() {
        let root = tempfile::tempdir().expect("tempdir");
        drop(Engine::open_encrypted(root.path(), KEY).expect("create legacy root store"));
        let pointer_path = root.path().join(generation_meta::GENERATION_META_FILENAME);
        fs::write(
            &pointer_path,
            generation_meta::encode(&generation_meta::GenerationMeta { current_generation: 1 }),
        )
        .expect("write pointer");

        let Err(err) = Engine::open_encrypted(root.path(), KEY) else {
            panic!("missing active generation must fail");
        };
        assert!(matches!(err, EngineError::CorruptGenerationMeta { .. }));
    }

    #[test]
    fn generation_pointer_requires_matching_crypto_meta_generation() {
        let root = tempfile::tempdir().expect("tempdir");
        drop(Engine::open_encrypted(root.path(), KEY).expect("create root store metadata"));
        let generation_dir = root.path().join("gen-1");
        fs::create_dir(&generation_dir).expect("create generation directory");
        // Deliberately write a generation-zero wrap under gen-1: an active
        // pointer must never make this silently usable.
        crypto::create_meta_for_generation(&generation_dir, KEY, crypto::KeyMode::RawKey, 0)
            .expect("create mismatched crypto meta");
        fs::write(
            root.path().join(generation_meta::GENERATION_META_FILENAME),
            generation_meta::encode(&generation_meta::GenerationMeta { current_generation: 1 }),
        )
        .expect("write pointer");

        let Err(err) = Engine::open_encrypted(root.path(), KEY) else {
            panic!("mismatched generation must fail");
        };
        assert!(matches!(err, EngineError::CorruptCryptoMeta { .. }));
    }

    #[test]
    fn published_generation_missing_crypto_meta_is_never_reinitialized() {
        let root = tempfile::tempdir().expect("tempdir");
        {
            let mut engine = Engine::open_encrypted(root.path(), KEY).expect("open");
            engine.put(b"key", b"value").expect("put");
            engine.rotate_key_full(b"fresh key").expect("full rotate");
        }
        fs::remove_file(root.path().join("gen-1").join("crypto.meta")).expect("remove active crypto meta");

        for key in [None, Some(&b"fresh key"[..])] {
            let result = match key {
                Some(key) => Engine::open_encrypted(root.path(), key),
                None => Engine::open(root.path()),
            };
            assert!(matches!(result, Err(EngineError::CorruptCryptoMeta { .. })));
        }
    }

    #[test]
    fn published_generation_without_store_meta_is_not_restamped() {
        let root = tempfile::tempdir().expect("tempdir");
        {
            let mut engine = Engine::open_encrypted(root.path(), KEY).expect("open");
            engine.rotate_key_full(b"fresh key").expect("full rotate");
        }
        fs::remove_file(root.path().join("store.meta")).expect("remove store meta");

        let Err(error) = Engine::open_encrypted(root.path(), b"fresh key") else {
            panic!("missing store.meta must not be recreated for a published generation");
        };
        assert!(matches!(error, EngineError::UnsupportedStoreFormat { found: 0, .. }));
        assert!(!root.path().join("store.meta").exists());
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
    fn full_rotation_publishes_fresh_generation_and_keeps_live_engine_usable() {
        let root = tempfile::tempdir().expect("tempdir");
        {
            let mut engine = Engine::open_encrypted_with_options(root.path(), KEY, small_options()).expect("open");
            engine.put(b"kept", b"current").expect("put current");
            engine.put(b"deleted", b"old").expect("put deleted");
            engine.flush().expect("flush old generation");
            engine.delete(b"deleted").expect("delete");
            fs::write(root.path().join("crypto.meta.tmp"), b"old wrap").expect("seed crypto tmp");
            fs::write(root.path().join("999.sst.tmp"), b"old ciphertext").expect("seed sst tmp");

            engine.rotate_key_full(b"fresh key").expect("full rotate");
            assert_eq!(engine.get(b"kept").expect("get kept").as_deref(), Some(&b"current"[..]));
            assert_eq!(engine.get(b"deleted").expect("get deleted"), None);
            engine.put(b"after", b"publish").expect("write after publish");
        }

        assert!(!root.path().join("crypto.meta.tmp").exists());
        assert!(!root.path().join("999.sst.tmp").exists());

        let Err(old) = Engine::open_encrypted(root.path(), KEY) else {
            panic!("old key must not open current generation");
        };
        assert!(matches!(old, EngineError::WrongEncryptionKey { .. }));
        let reopened = Engine::open_encrypted(root.path(), b"fresh key").expect("new key opens");
        assert_eq!(
            reopened.get(b"kept").expect("get kept").as_deref(),
            Some(&b"current"[..])
        );
        assert_eq!(reopened.get(b"deleted").expect("get deleted"), None);
        assert_eq!(
            reopened.get(b"after").expect("get after").as_deref(),
            Some(&b"publish"[..])
        );
    }

    /// ADR-042 §5 exit criterion: the old key **combined with a genuine copy
    /// of the pre-rotation `crypto.meta`** — exactly the gap ADR-030 §4
    /// documented as uncovered for `--full` — must not decrypt a single byte
    /// of the new generation, neither WAL nor SST. `generation_pointer_
    /// requires_matching_crypto_meta_generation` already proves the
    /// generation-id self-check in `crypto.meta` rejects this; this test
    /// goes one level lower and proves the AEAD itself (DEK binding, not
    /// just the generation-id field) rejects it, by attempting a raw
    /// `CryptoContext::open` against real ciphertext from the new
    /// generation using a context loaded from the *old* generation's
    /// `crypto.meta` while it still existed, pre-rotation.
    #[test]
    fn old_crypto_meta_copied_beside_a_new_generation_cannot_open_its_wal_or_sst() {
        let root = tempfile::tempdir().expect("tempdir");
        let old_ctx = {
            let mut engine = Engine::open_encrypted_with_options(root.path(), KEY, small_options()).expect("open");
            engine.put(b"kept", b"current").expect("put current");
            engine.flush().expect("seed a real sealed SST under generation 0");
            let ctx = crypto::load_meta(root.path(), KEY).expect("load pre-rotation context");
            engine.rotate_key_full(b"fresh key").expect("full rotate");
            engine
                .put(b"after", b"publish")
                .expect("write into the new generation's WAL");
            ctx
        };

        let pointer_bytes =
            fs::read(root.path().join(generation_meta::GENERATION_META_FILENAME)).expect("read generation pointer");
        let pointer = generation_meta::decode(
            &pointer_bytes,
            &root.path().join(generation_meta::GENERATION_META_FILENAME),
        )
        .expect("decode generation pointer");
        let new_gen_dir = generation_dir(root.path(), pointer.current_generation);

        let wal_path = new_gen_dir.join("wal.log");
        let wal_bytes = fs::read(&wal_path).expect("read new generation wal");
        let (nonce, ciphertext, _consumed) = crate::format::crypto::decode_wal_envelope(&wal_bytes, &wal_path)
            .expect("structurally decode the first wal envelope")
            .expect("new generation wal must hold at least one complete record");
        let wal_aad = crate::format::crypto::wal_envelope_aad();
        assert!(
            old_ctx.open(&nonce, ciphertext, &wal_aad).is_none(),
            "the pre-rotation key + crypto.meta must not decrypt the new generation's WAL"
        );
        // Positive control: the same extracted (nonce, ciphertext, aad)
        // genuinely decrypts under the *new* generation's real context —
        // proves the `None` above is the DEK mismatch this test targets,
        // not a byte-slicing mistake that would return `None` regardless.
        let new_ctx = crypto::load_meta_for_generation(
            &new_gen_dir,
            b"fresh key",
            crypto::KeyMode::RawKey,
            pointer.current_generation,
        )
        .expect("load the new generation's own context");
        assert!(
            new_ctx.open(&nonce, ciphertext, &wal_aad).is_some(),
            "sanity check failed: the new generation's own key must decrypt its own WAL"
        );

        let sst_path = fs::read_dir(&new_gen_dir)
            .expect("read new generation directory")
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.path())
            .find(|path| path.extension().and_then(|ext| ext.to_str()) == Some("sst"))
            .expect("full rotation must have re-sealed at least one SST into the new generation");
        let sst_bytes = fs::read(&sst_path).expect("read new generation sst");
        let header_len = crate::format::sst_block::SST_HEADER_TOTAL_LEN;
        let header = crate::format::sst_block::decode_sst_header(&sst_bytes[..header_len], &sst_path)
            .expect("decode the plaintext sst header");
        // `decode_encrypted_sst_block` requires the exact envelope slice (no
        // torn-tail tolerance) — peek the block's own `ct_len` field first
        // so the slice passed in is neither short nor carries the next
        // section's bytes.
        let block_bytes = &sst_bytes[header_len..];
        let nonce_len = crate::format::crypto::NONCE_LEN;
        let envelope_header_len = 4 + 2 + nonce_len + 4;
        let ct_len = u32::from_le_bytes(
            block_bytes[4 + 2 + nonce_len..envelope_header_len]
                .try_into()
                .expect("4-byte ct_len field"),
        ) as usize;
        let (sst_nonce, sst_ciphertext) =
            crate::format::crypto::decode_encrypted_sst_block(&block_bytes[..envelope_header_len + ct_len], &sst_path)
                .expect("structurally decode the first sealed data block");
        // Block 0 of the Data section, right after the plaintext header —
        // `sst_id` comes from the file's own header so the AAD matches
        // exactly what the writer used, isolating the assertion to the DEK
        // mismatch rather than an incidental AAD mismatch.
        let sst_aad = crate::format::crypto::encrypted_sst_block_aad(
            header.sst_id,
            crate::format::crypto::SstSectionType::Data,
            0,
        );
        assert!(
            old_ctx.open(&sst_nonce, sst_ciphertext, &sst_aad).is_none(),
            "the pre-rotation key + crypto.meta must not decrypt the new generation's SST, \
             even with the correct AAD shape reconstructed"
        );
        assert!(
            new_ctx.open(&sst_nonce, sst_ciphertext, &sst_aad).is_some(),
            "sanity check failed: the new generation's own key must decrypt its own SST block"
        );
    }

    #[test]
    fn full_rotation_can_switch_to_passphrase_mode() {
        let root = tempfile::tempdir().expect("tempdir");
        {
            let mut engine = Engine::open_encrypted(root.path(), KEY).expect("open raw-key store");
            engine.put(b"key", b"value").expect("put");
            engine
                .rotate_passphrase_full(b"new human passphrase")
                .expect("full rotate to passphrase");
        }

        let Err(raw) = Engine::open_encrypted(root.path(), b"new human passphrase") else {
            panic!("same bytes in raw-key mode must be refused");
        };
        assert!(matches!(raw, EngineError::WrongEncryptionKey { .. }));
        let reopened = Engine::open_with_passphrase(root.path(), b"new human passphrase").expect("passphrase opens");
        assert_eq!(reopened.get(b"key").expect("get").as_deref(), Some(&b"value"[..]));
    }

    #[test]
    fn in_place_rotation_can_switch_to_passphrase_mode() {
        let root = tempfile::tempdir().expect("tempdir");
        {
            let mut engine = Engine::open_encrypted(root.path(), KEY).expect("open raw-key store");
            engine.put(b"key", b"value").expect("put");
            engine
                .rotate_passphrase(b"new human passphrase")
                .expect("rotate to passphrase");
            assert_eq!(engine.get(b"key").expect("get live").as_deref(), Some(&b"value"[..]));
        }

        let Err(raw) = Engine::open_encrypted(root.path(), b"new human passphrase") else {
            panic!("same bytes in raw-key mode must be refused");
        };
        assert!(matches!(raw, EngineError::WrongEncryptionKey { .. }));
        let reopened = Engine::open_with_passphrase(root.path(), b"new human passphrase").expect("passphrase opens");
        assert_eq!(reopened.get(b"key").expect("get").as_deref(), Some(&b"value"[..]));
    }

    #[test]
    fn consecutive_full_rotations_advance_and_gc_generations() {
        let root = tempfile::tempdir().expect("tempdir");
        {
            let mut engine = Engine::open_encrypted(root.path(), KEY).expect("open");
            engine.put(b"before", b"one").expect("put before");
            engine.rotate_key_full(b"second key").expect("first full rotate");
            engine.put(b"between", b"two").expect("put between");
            engine.rotate_key_full(b"third key").expect("second full rotate");
        }

        assert!(!root.path().join("gen-1").exists(), "previous generation must be GC'd");
        assert!(root.path().join("gen-2").is_dir());
        let engine = Engine::open_encrypted(root.path(), b"third key").expect("open latest generation");
        assert_eq!(engine.get(b"before").expect("get before").as_deref(), Some(&b"one"[..]));
        assert_eq!(
            engine.get(b"between").expect("get between").as_deref(),
            Some(&b"two"[..])
        );
    }

    #[test]
    fn full_rotation_preserves_monotonic_block_cache_counters() {
        let root = tempfile::tempdir().expect("tempdir");
        let mut engine = Engine::open_encrypted_with_options(root.path(), KEY, small_options()).expect("open");
        engine.put(b"key", b"value").expect("put");
        engine.flush().expect("flush");
        assert_eq!(engine.get(b"key").expect("miss").as_deref(), Some(&b"value"[..]));
        assert_eq!(engine.get(b"key").expect("hit").as_deref(), Some(&b"value"[..]));
        let before = engine.stats().expect("stats before");

        engine.rotate_key_full(b"fresh key").expect("full rotate");
        let after = engine.stats().expect("stats after");
        assert!(after.block_cache_hits >= before.block_cache_hits);
        assert!(after.block_cache_misses >= before.block_cache_misses);
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

    /// INV-VS-4 (ADR-043 §2 amendé) : un `VersionEdit` dont un id de
    /// `deleted` n'est pas dans le `Version` courant est refusé typé, sans
    /// rien publier — ni manifest, ni bascule de `current`. Inatteignable
    /// via l'API publique tant que flush/compaction tiennent `&mut self`
    /// (J3) ; forgé ici directement, c'est le garde-fou que J4 (compaction
    /// hors verrou) viendra exercer pour de vrai.
    #[test]
    fn forged_version_edit_with_unknown_deleted_id_is_refused_and_publishes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut engine = Engine::open(dir.path()).expect("open");
        engine.put(b"a", b"1").expect("put");
        engine.flush().expect("flush -> SST 0");

        let manifest_path = dir.path().join(sst_manifest::SST_MANIFEST_FILENAME);
        let manifest_before = fs::read(&manifest_path).expect("read manifest before");
        let ids_before = engine.current.ids();
        let generation_before = engine.current.manifest_generation;

        let err = engine
            .apply_version_edit(VersionEdit {
                added: Vec::new(),
                deleted: vec![999],
            })
            .expect_err("an edit deleting an id absent from the current version must be refused");
        assert!(matches!(err, EngineError::VersionEditMissingInput { id: 999 }));

        // Rien n'a été publié : manifest bit-à-bit identique, `current`
        // inchangé (génération et ids), moteur toujours pleinement utilisable.
        assert_eq!(
            fs::read(&manifest_path).expect("read manifest after"),
            manifest_before,
            "a refused edit must not publish any manifest"
        );
        assert_eq!(engine.current.manifest_generation, generation_before);
        assert_eq!(engine.current.ids(), ids_before);
        assert_eq!(engine.get(b"a").expect("get").as_deref(), Some(&b"1"[..]));
        engine
            .put(b"b", b"2")
            .expect("the engine stays fully usable after a refused edit");
        assert_eq!(engine.get(b"b").expect("get").as_deref(), Some(&b"2"[..]));
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
