// SPDX-License-Identifier: BUSL-1.1
//! Logical index structures layered on top of the Layer-1 KV store
//! (Couche 2, `docs/PLAN-NATIVE-ENGINE.md` §2) — never a second durability
//! engine: indexes are reconstructible from the data, which stays the single
//! source of truth (ADR-026 §Décision 3).
//!
//! - [`vector`] — LM-DiskANN-style vector index (ADR-026).
//! - [`graph`] — entity/edge graph index with bounded BFS traversal (N4),
//!   a literal behavioral port of `basemyai`'s recursive-CTE graph.
//! - [`memory`] — memory records + vector-id mapping + monotonic allocator
//!   (N5.1, ADR-027), the persistence half of the `MemoryStore` wiring.
//! - [`fts`] — inverted index + BM25 scoring (N5.2, ADR-028), scoped to the
//!   narrow `match_expr` subset `basemyai` actually produces.

pub mod fts;
pub mod graph;
pub mod memory;
pub mod vector;
