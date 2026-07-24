// SPDX-License-Identifier: BUSL-1.1
//! Engine observability (N7, `docs/PLAN-NATIVE-ENGINE.md` §4.1): a cheap
//! snapshot of the store's internal state and I/O activity, for benchmarks,
//! soak runs and the future `verify`/repair tooling (N9).
//!
//! Two kinds of fields coexist, documented per field:
//! - **counters** — monotonic since `Engine::open*` (never persisted, reset
//!   on every open);
//! - **gauges** — the current state at the moment [`Engine::stats`] is
//!   called.
//!
//! The block-cache fields (N8.7, ADR-039 §5.6) and `point_lookup_
//! full_sst_read` (N8.4) are all real, live counters — see their own docs
//! below for exactly what each measures.

/// Point-in-time snapshot returned by [`Engine::stats`](crate::Engine::stats).
///
/// Cheap to produce: gauges over the memtable iterate at most
/// `memtable_flush_threshold` entries; everything else is precomputed or a
/// plain counter read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct EngineStats {
    /// Gauge: current WAL file length on disk, in bytes.
    pub wal_bytes: u64,
    /// Counter: WAL records appended since open (a batch = one record).
    pub wal_records: u64,
    /// Gauge: approximate memtable payload (sum of key + value lengths).
    pub memtable_bytes: u64,
    /// Gauge: number of live SST files.
    pub sst_count: usize,
    /// Gauge: total on-disk bytes of live SST files.
    pub sst_bytes: u64,
    /// Gauge: tombstones currently held (memtable + live SSTs).
    pub tombstone_count: u64,
    /// Counter: memtable flushes since open.
    pub flush_count: u64,
    /// Counter: compactions since open.
    pub compaction_count: u64,
    /// Counter: on-disk bytes of the SSTs consumed by compactions since open.
    pub compaction_input_bytes: u64,
    /// Counter: on-disk bytes of the SSTs produced by compactions since open.
    pub compaction_output_bytes: u64,
    /// Counter: bytes read from disk since open — WAL replay plus, per SST,
    /// only the metadata [`crate::store::sst_block::BlockSstFile::load`]
    /// actually reads (header + footer + block index + bloom filter), never
    /// the full on-disk file size (ADR-039 §4/§8.1 — the O(metadata)-open
    /// exit criterion this counter exists to make measurable). Point-lookup
    /// data-block reads (`get`) are **not** folded into this counter: `get`
    /// stays `&self` (Engine's public API shape is unchanged, ADR-039 §5.3),
    /// so per-call I/O accounting there would need interior-mutable state
    /// beyond the one dedicated invariant counter this milestone adds
    /// ([`Self::point_lookup_full_sst_read`]) — a deliberate, documented
    /// scope line, not an oversight.
    pub bytes_read: u64,
    /// Counter: bytes written to disk since open (WAL appends + SST files).
    pub bytes_written: u64,
    /// Counter: point-lookup block-cache hits since open (N8.7, ADR-039
    /// §5.6) — a hit means `Engine::get` resolved a key's data block from
    /// `store::block_cache::BlockCache` instead of reading it from disk.
    /// Only [`Engine::get`]'s path consults the cache; `scan_prefix`/
    /// compaction's full walks never do (they would just evict hot blocks
    /// with cold ones read exactly once).
    pub block_cache_hits: u64,
    /// Counter: point-lookup block-cache misses since open (N8.7). A lookup
    /// resolved without ever needing a data block — a bloom-filter negative,
    /// or a key sorting outside every block's key range — is neither a hit
    /// nor a miss here: no cache lookup was attempted, because none was
    /// needed.
    pub block_cache_misses: u64,
    /// Counter: point lookups where resolving a single key within one SST
    /// required reading more than one on-disk data block (ADR-039 §4/§5.5).
    /// Structurally `0` given
    /// [`crate::store::sst_block::BlockSstFile::get`]'s
    /// bloom-filter -> block-index-binary-search -> single-block-read path
    /// — instrumented as a regression canary, not a "known to sometimes
    /// happen" counter: a future change that falls back to scanning
    /// multiple blocks per lookup would show up here instead of silently
    /// degrading. See `tests/engine_stats.rs` for the test pinning this at
    /// zero across a canonical multi-block workload.
    pub point_lookup_full_sst_read: u64,
    /// Counter: `sync_all()` calls on a `File` performed since open, on the
    /// two write-path hot loops only — every WAL append/truncate fsync
    /// (`store::wal::Wal`) plus one fsync per flushed/compacted SST
    /// (`store::sst_block::BlockSstFile::write_new`, called from
    /// `Engine::flush`/`Engine::compact`). Feeds the group-commit sizing
    /// decision (ADR-047, ENGINE-TARGET-ARCHITECTURE.md §17/§20 R0). Scope
    /// deliberately excludes the rare metadata fsyncs of `store.meta`,
    /// `generation.meta` and `crypto.meta` (open/rotation only, never under
    /// write load) and the SST/WAL fsyncs of `Engine::rotate_key_full` (a
    /// rare full-rewrite operation, not the per-op hot path this counter
    /// targets) — a deliberate, documented scope line, not an oversight.
    pub fsync_count: u64,
    /// Gauge: bytes of orphaned `*.tmp` files found in the active store
    /// directory at the last [`Engine::open`](crate::Engine::open) (or
    /// equivalent) call — `*.sst.tmp`, `crypto.meta.tmp`,
    /// `generation.meta.tmp`, `store.meta.tmp` left behind by a crash mid
    /// atomic-replace. A one-time snapshot taken at open, never refreshed
    /// afterward; these files are already ignored by `scan_existing`/
    /// `sst_files_present` exactly as before this counter existed — this
    /// only observes them, it does not touch or remove anything. Feeds a
    /// future GC-aggressiveness decision (ENGINE-TARGET-ARCHITECTURE.md
    /// §17/§20 R0).
    pub orphan_bytes: u64,
    /// Counter: old SST removals that still failed after `compact()`'s
    /// retries (ENG-DUR-002 minimal correction,
    /// `docs/audits/2026-07-engine-architecture-safety-audit.md`). Zero on a
    /// healthy filesystem. A persistently failed removal is a real, if
    /// narrow, risk — a leftover pre-compaction SST can resurrect a deleted
    /// key on a future reopen until the durable manifest (ENG-DUR-001, N13
    /// jalon J2) makes orphan detection a construction guarantee instead of
    /// a best-effort cleanup. This counter turns a previously fully silent
    /// failure into an observable one; it does not by itself close the gap.
    pub compaction_remove_failures: u64,
    /// Counter: old-generation-directory removals that still failed after
    /// retries following a full key/passphrase rotation (GC-RETRY-P2,
    /// BaseMyAI adversarial audit, 2026-07-22) — the directory-level
    /// counterpart to [`Self::compaction_remove_failures`], same discipline
    /// (`gc_old_generation`'s removal attempts previously made exactly one
    /// try and silently discarded the error, unlike the per-SST removal
    /// path this mirrors). Zero on a healthy filesystem. A persistently
    /// failed removal here is notable specifically because a full rotation's
    /// entire purpose is to leave no bytes readable under the old DEK —
    /// this counter turns a silent failure of that guarantee into an
    /// observable one; the leftover directory is still swept at the next
    /// `Engine::open` (`gc_inactive_generations`).
    pub generation_remove_failures: u64,
    /// Gauge: the durable SST-manifest's current publication counter
    /// (ENG-DUR-001, `manifest.meta` — incremented on every flush that adds
    /// an SST and every compaction that replaces the set).
    pub manifest_generation: u64,
    /// Gauge: live [`Snapshot`](crate::store::Snapshot)s not yet dropped
    /// (ADR-043 §2 amended, J3 — the "active snapshots" metric ENG-RES-003
    /// asks for). Every live snapshot pins its version's SST files on disk
    /// (deferred removal, INV-VS-6): a snapshot that never drops is a
    /// space leak in the making, and this gauge is how it gets seen before
    /// it is felt.
    pub active_snapshots: u64,
}

/// The engine's private monotonic counters (the gauges of [`EngineStats`]
/// are derived from live state at snapshot time instead of being tracked).
///
/// Plain `u64`s, no atomics: every increment site is on a `&mut self` path
/// (`put`/`delete`/`apply_batch`/`flush`/`compact`/`open`) — the `&self`
/// read paths (`get`/`scan_prefix`) touch no disk today. Revisit when N8
/// block reads make `&self` paths do I/O.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct Counters {
    pub(crate) wal_records: u64,
    pub(crate) flush_count: u64,
    pub(crate) compaction_count: u64,
    pub(crate) compaction_input_bytes: u64,
    pub(crate) compaction_output_bytes: u64,
    pub(crate) bytes_read: u64,
    pub(crate) bytes_written: u64,
    /// See [`EngineStats::fsync_count`] for exact scope (WAL + flush/compact
    /// SST fsyncs only). WAL fsyncs are tracked on the live `Wal` handle
    /// itself (not here) and folded in here only when a `Wal` is retired
    /// mid-life (`Engine::rotate_key_full`'s old-WAL swap) — see
    /// `Engine::stats`/`Engine::rotate_full`.
    pub(crate) fsync_count: u64,
}
