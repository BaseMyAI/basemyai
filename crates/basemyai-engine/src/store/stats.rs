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
}
