// SPDX-License-Identifier: BUSL-1.1
//! Native graph index — Layer 2 (Couche 3 per `docs/PLAN-NATIVE-ENGINE.md`
//! §2, N4 in `docs/TODO-NATIVE-ENGINE.md`): entities and directed edges, one
//! KV record each under the reserved `idx/graph/` keyspace
//! ([`crate::key::graph_index`]), breadth-first bounded traversal shared
//! (zero-drift) between an in-RAM flavor ([`RamGraph`]) and a KV-persisted
//! flavor ([`PersistentGraph`]).
//!
//! This is a **literal behavioral port** of the original `graph_traverse`
//! (a recursive CTE over `entity`/`edge` tables) — see [`traverse`]'s
//! module doc for the exact semantics preserved, and
//! `tests/graph_parity.rs` for the ported `crates/basemyai/tests/graph.rs`
//! scenarios, run against both flavors.
//!
//! - [`entity`] — the `GraphEntity` wire block (`GraphEntity:1`).
//! - [`edge`] — the `GraphEdgeMeta` wire block (`GraphEdge:1`); `relation`
//!   and `dst` live in the *key* ([`crate::key::graph_index`]), not the
//!   value — that's what makes "every outgoing edge of a node" a single
//!   prefix scan.
//! - [`traverse`] — the shared BFS algorithm.
//! - [`ram`] — [`RamGraph`], the harness/oracle flavor.
//! - [`persistent`] — [`PersistentGraph`], the KV-persisted flavor; see its
//!   module doc for why no metadata/rebuild machinery is needed here,
//!   unlike `idx::vector`.

pub mod edge;
pub mod entity;
pub mod persistent;
pub mod ram;
pub mod traverse;

pub use edge::GraphEdgeMeta;
pub use entity::GraphEntity;
pub use persistent::PersistentGraph;
pub use ram::RamGraph;
pub use traverse::Reached;
