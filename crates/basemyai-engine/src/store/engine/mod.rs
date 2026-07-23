// SPDX-License-Identifier: BUSL-1.1
//! The public single-writer KV engine: WAL + memtable + SST, with crash
//! recovery on `open`. See the `store` module docs for the write-path
//! ordering guarantee.
//!
//! Split by responsibility: [`io`] the durable-write primitives shared
//! across phases, [`open`] open/recovery, [`write`] the ingestion path,
//! [`read`] point lookups + scans, [`compact`] flush + compaction + version
//! publishing (kept as one file — `apply_version_edit` is the documented
//! shared choke point both `flush` and `compact_commit` rely on, and
//! `flush` conditionally triggers `compact`, so splitting them further would
//! fragment a genuinely indivisible unit), [`rotate`] key/passphrase
//! rotation (in-place and full).

use std::fs::File;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::crypto::CryptoContext;
use crate::error::Result;
use crate::key::Key;
use crate::store::Value;
use crate::store::block_cache::BlockCache;
use crate::store::memtable::Memtable;
use crate::store::stats::{Counters, EngineStats};
use crate::store::version::{Snapshot, Version};
use crate::store::wal::Wal;

mod compact;
mod io;
mod open;
mod read;
mod rotate;
#[cfg(test)]
mod test_support;
mod write;

pub use compact::CompactionJob;

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
    /// Whether `flush()` runs compaction inline, synchronously, once
    /// `compaction_sst_threshold` is exceeded — `true` (the historical
    /// behavior, ADR-025) keeps `Engine` self-contained: any direct caller
    /// (a standalone binary, a test harness — nothing wraps `Engine` in a
    /// lock it could hold too long) gets bounded SST growth for free, at the
    /// cost of that caller's `put`/`delete`/`apply_batch` occasionally
    /// paying a full-merge pass. `false` (ADR-043 §3/J4) opts out of that:
    /// `flush()` only exposes [`Self::compaction_pending`], the merge never
    /// runs inline, and it is the caller's responsibility to poll that and
    /// drive [`Self::compact_prepare`]/[`Self::compact_commit`] itself —
    /// correct only for a caller with somewhere better to run the merge off
    /// its own lock (`NativeInner::with_inner`, the sole `false` setter).
    /// Getting this wrong for a direct `Engine` user doesn't corrupt
    /// anything (SSTs just never merge), but every lookup degrades to
    /// scanning an ever-growing, never-compacted SST list — this is exactly
    /// what made `crash_consistency.rs`'s kill-loop balloon from ~35s to
    /// unbounded before this option existed, since `crash_writer` opens an
    /// `Engine` directly with no `NativeInner` equivalent driving compaction
    /// for it.
    pub auto_compact_on_flush: bool,
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            memtable_flush_threshold: 1000,
            compaction_sst_threshold: 4,
            block_size: DEFAULT_BLOCK_SIZE,
            block_cache_capacity_bytes: DEFAULT_BLOCK_CACHE_CAPACITY_BYTES,
            auto_compact_on_flush: true,
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
    /// Replaced wholesale by [`compact::apply_version_edit`] — never mutated
    /// in place (INV-VS-1/2). [`Self::snapshot`] pins it by cloning the
    /// `Arc`.
    current: Arc<Version>,
    /// Next SST id to allocate. `Arc<AtomicU64>`, not a plain field, for two
    /// stacked reasons: `Engine::compact_prepare` (ADR-043 §3/J4) reserves
    /// an id from `&self` — the merge it stages runs off the write lock, so
    /// id allocation can no longer assume exclusive access to `Engine` — and
    /// `Engine::compaction_snapshot` (CONC-P1 fix) hands a clone of this
    /// `Arc` out of `Engine` entirely, so a caller wrapping `Engine` in its
    /// own outer lock (`NativeMemoryStore`) can reserve the merged SST's id
    /// without holding *that* lock either. Reservation is a single
    /// `fetch_add`; nothing else about compaction depends on ordering
    /// relative to other memory, so `Relaxed` — same reasoning as
    /// `active_snapshots`/`sst_remove_failures` below. `file.id` doubles as
    /// `Version`'s canonical visibility-order key (INV-VS-8,
    /// `store::version`) precisely because reservation always happens here,
    /// atomically, at content-freeze time — never anywhere else.
    next_sst_id: Arc<AtomicU64>,
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
    /// — an `Arc<AtomicU64>` shared with every [`crate::store::version::SstHandle`],
    /// because the failing attempt now runs at handle drop (possibly when a
    /// snapshot releases, outside any `&mut Engine` context), no longer
    /// inline in `compact()`. See [`EngineStats::compaction_remove_failures`].
    sst_remove_failures: Arc<AtomicU64>,
    /// Old-generation-directory removals that still failed after retries
    /// following a full key/passphrase rotation (GC-RETRY-P2). See
    /// [`EngineStats::generation_remove_failures`].
    generation_remove_failures: Arc<AtomicU64>,
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
            sst_count: self.current.ssts().len(),
            sst_bytes: self.current.ssts().iter().map(|h| h.file.file_bytes).sum(),
            tombstone_count: memtable_tombstones + self.current.ssts().iter().map(|h| h.file.tombstones).sum::<u64>(),
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
            generation_remove_failures: self.generation_remove_failures.load(Ordering::Relaxed),
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

    /// Flushes any pending memtable data and releases the store. Skipping
    /// `close` is safe too — durability is already established per-`put`/
    /// `delete` via WAL fsync — it just avoids leaving unflushed data to be
    /// replayed from the WAL on the next `open`.
    pub fn close(mut self) -> Result<()> {
        self.flush()
    }
}
