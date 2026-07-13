// SPDX-License-Identifier: BUSL-1.1
//! `basemyai-engine` — home-grown LSM-tree storage foundation for BaseMyAI's
//! native engine (Layer 1 per `docs/PLAN-NATIVE-ENGINE.md` §3.1, decided by
//! `docs/adr/ADR-025-native-engine-storage-foundation.md`).
//!
//! Internal crate: **not published** (`publish = false`), and **not wired**
//! into `basemyai-core` or `basemyai` yet — plugging in `EngineKind::Native`
//! and a `MemoryStore` impl is a separate, later N2 item, out of scope here.
//! Today this crate only has to build and test itself.
//!
//! Design reference (ADR-025, from the N1 spike): WAL first, then memtable;
//! flush as an ordered SST — fsync the new SST, rename it into place, and
//! only *then* truncate the WAL. Never truncate before the new SST is
//! durably renamed. Compaction is intentionally naive (full merge past a
//! threshold) — correctness first, a tiered/leveled strategy is deferred.
//!
//! Layout:
//! - [`key`] — keyspace encoding. Currently a single generic byte-key type;
//!   this crate has no notion of "entity" yet (mirrors the `basemyai-core`
//!   agnosticism rule — mechanism here, sense at whatever consumes this
//!   later). Entity-specific encoders land here as their own modules.
//! - [`format`] — versioned on-disk record layouts (WAL records, SST files).
//!   Every persisted type carries an explicit version constant plus a
//!   documented byte layout, so the `format.lock` mechanism (built in
//!   parallel, elsewhere) has something concrete to hash against.
//! - [`store`] — WAL, memtable, SST, and the [`Engine`] that ties them
//!   together: `open`/`put`/`get`/`delete`/`flush`/`close`, with crash
//!   recovery on `open` (WAL replay + SST load).
//! - [`idx`] — logical index structures on top of the KV store (Couche 2).
//!   [`idx::vector`]: the LM-DiskANN-style vector index (ADR-026) —
//!   versioned node/meta block formats, the Vamana algorithm (shared
//!   between the in-RAM [`VectorIndex`] and the KV-persisted
//!   [`PersistentVectorIndex`], whose inserts and tombstone deletes ride
//!   one atomic `apply_batch` each, with an explicit FreshDiskANN
//!   `consolidate()` pass and a graph rebuildable from the stored
//!   vectors). [`idx::graph`] (N4): entity/edge graph index with bounded
//!   BFS traversal, a literal behavioral port of `basemyai`'s recursive-CTE
//!   graph — shared between an in-RAM [`RamGraph`] and a KV-persisted
//!   [`PersistentGraph`] (no metadata/rebuild machinery needed — see that
//!   module's doc for why). [`idx::fts`] (N5.2): inverted index + BM25
//!   scoring over the narrow `match_expr` subset `basemyai` produces —
//!   [`PersistentFts`] stages postings/doc-terms/stats updates into the
//!   caller's batch (never its own `apply_batch`), fused by
//!   [`idx::memory::PersistentMemoryIndex`] into the same atomic write as
//!   the memory record and vector node.
//! - [`error`] — [`EngineError`] (thiserror, `#[non_exhaustive]`).
//! - [`harness`] — deterministic key/value content shared by the
//!   crash-consistency kill-loop harness (`src/bin/crash_writer.rs` +
//!   `tests/crash_consistency.rs`, wired as `cargo xtask
//!   test-crash-consistency`).

pub mod error;
#[cfg(any(test, feature = "test-util"))]
pub mod failpoint;
pub mod format;
pub mod harness;
pub mod idx;
pub mod key;
pub mod store;

pub(crate) mod crypto;

/// Fault-injection site (N7.4). Compiles to **nothing** without
/// `test-util`/`cfg(test)`; with them, consults the [`failpoint`] registry
/// and either continues, returns an injected error (`?`), or aborts the
/// process — see the [`failpoint`] module docs for sites and configuration.
macro_rules! fail_point {
    ($name:literal) => {
        #[cfg(any(test, feature = "test-util"))]
        crate::failpoint::hit($name)?;
    };
}
pub(crate) use fail_point;

pub use error::{EngineError, Result};
pub use idx::fts::{FtsStats, PersistentFts};
pub use idx::graph::{GraphEdgeMeta, GraphEntity, PersistentGraph, RamGraph, Reached};
pub use idx::memory::{ForgetBatchOptions, MemoryRecord, NewMemoryRecord, PersistentMemoryIndex, VecMapEntry};
pub use idx::vector::{PersistentVectorIndex, VectorIndex, VectorIndexParams};
pub use key::Key;
pub use store::{
    Batch, DEFAULT_BLOCK_SIZE, Engine, EngineOptions, EngineStats, IntegrityIssue, IssueKind, RebuildReport,
    RepairAction, RepairPlan, ScanPage, Value, VerifyMode, VerifyReport, plan_repair, rebuild_indexes, verify_store,
};
