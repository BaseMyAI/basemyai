//! Native vector index — LM-DiskANN family (flat Vamana graph), decided by
//! `docs/adr/ADR-026-native-vector-index-lm-diskann.md`.
//!
//! Current N3 slice (format → RAM graph → KV persistence → deletes):
//! - [`node`] — the versioned, `format.lock`-anchored node block
//!   (vector + neighbor list + tombstone flag, `VectorNode:2`), THE wire
//!   format the index persists as.
//! - [`distance`] — cosine distance, dimension-parametric (default 384).
//! - [`graph`] — the Vamana algorithm, written once against a node-provider
//!   abstraction: greedy beam search, robust prune (α), incremental insert,
//!   tombstone deletes with FreshDiskANN repair/consolidation (ADR-026 §4).
//!   Exposes [`VectorIndex`], the in-RAM flavor, judged by
//!   `tests/vector_recall.rs` and `tests/vector_churn.rs` against an exact
//!   brute-force oracle (recall@10 ≥ 0.9, **including after insert/delete
//!   churn** — ADR-026 §6).
//! - [`meta`] — index parameters (dim, R, L, α) with the ADR defaults, plus
//!   the persisted metadata record (`VectorIndexMeta:1`, `format.lock`-
//!   anchored: params, entry point, epoch, live count).
//! - [`persistent`] — [`PersistentVectorIndex`], the KV-persisted flavor:
//!   one node = one KV record under `idx/vector/` inside the Layer-1
//!   [`crate::Engine`], every insert/delete one atomic `apply_batch`,
//!   explicit `consolidate()` (crash-safe by ordering), rebuildable from
//!   the stored vectors when the metadata is absent/corrupt (data = source
//!   of truth; tombstoned blocks are purged, never resurrected). Judged by
//!   `tests/vector_persistence.rs`, `tests/vector_churn.rs`, and the
//!   `vector` (churn) mode of the crash-consistency harness.
//!
//! Next N3 step (not here): the M6 parity bench (ADR-026 §6 latency/build
//! thresholds).

pub mod distance;
pub mod graph;
pub mod meta;
pub mod node;
pub mod persistent;

pub use graph::VectorIndex;
pub use meta::{VectorIndexMeta, VectorIndexParams};
pub use node::VectorNode;
pub use persistent::PersistentVectorIndex;
