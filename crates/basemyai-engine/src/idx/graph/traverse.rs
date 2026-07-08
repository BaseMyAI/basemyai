// SPDX-License-Identifier: BUSL-1.1
//! Shared BFS traversal algorithm (N4). Written once against a
//! [`GraphProvider`] abstraction so the in-RAM [`super::ram::RamGraph`] and
//! the KV-persisted [`super::persistent::PersistentGraph`] read exactly the
//! same traversal logic — zero drift possible, mirroring the discipline
//! `idx::vector::graph`'s shared Vamana algorithm already established
//! between its RAM and persistent flavors.
//!
//! This is a **literal behavioral port** of `basemyai`'s
//! `LibsqlMemoryStore::graph_traverse` (a recursive CTE over `entity`/`edge`
//! SQL tables, `crates/basemyai/src/storage/libsql_store.rs`), not a
//! reinvention — `crates/basemyai/tests/graph.rs` is the spec of behavior to
//! equal exactly (ported scenario-for-scenario into
//! `crates/basemyai-engine/tests/graph_parity.rs`), and one deliberate
//! preservation is worth calling out: **only `valid_until` gates visibility
//! of an entity or an edge at traversal time — `valid_from` is never
//! checked here**, because the ported SQL's own `WHERE` clauses
//! (`e.valid_until IS NULL OR e.valid_until > ?`) never check it either. It
//! reads like it could be an oversight in the original, but replicating the
//! *actual* behavior being ported is the point — "improving" on it here
//! would silently change semantics for a caller relying on parity.
//!
//! ## BFS, not the CTE's recursive `UNION`
//!
//! The ported CTE explores depth-by-depth via `UNION` (deduplicating
//! `(node, depth)` pairs) and aggregates with `MIN(depth)` per node in its
//! final `GROUP BY`. An iterative, visited-set BFS produces the identical
//! result without a SQL engine: a `VecDeque` frontier only ever holds items
//! in non-decreasing depth order (every push is exactly one hop past what's
//! currently being popped), so the **first** time a node is reached is
//! always its **minimum** depth — exactly what `MIN(depth)` computes — and
//! marking a node visited the instant it's first reached both gives that
//! minimum-depth property AND guarantees termination on a cycle (a node
//! already visited is never re-queued, so no walk can loop forever), the
//! same two things the CTE gets from `UNION` + the `max_depth` bound.

use std::collections::{HashMap, HashSet, VecDeque};

use super::edge::GraphEdgeMeta;
use super::entity::GraphEntity;
use crate::error::Result;

/// One entity reached by [`run`], with its hop distance from the
/// traversal's start. The start itself is never in the result — present or
/// not, valid or not — matching the ported CTE's `r.node <> ?1`.
#[derive(Debug, Clone, PartialEq)]
pub struct Reached {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub depth: u32,
}

/// One outgoing edge as returned by [`GraphProvider::out_edges`]:
/// `(relation, dst, meta)`.
pub(crate) type OutEdge = (String, String, GraphEdgeMeta);

/// Abstraction [`run`] is written against — implemented by [`super::ram::RamGraph`]
/// (infallible) and [`super::persistent::PersistentGraph`] (reads through the
/// Layer-1 [`crate::store::Engine`], which can surface I/O or corruption
/// errors).
pub(crate) trait GraphProvider {
    /// The entity `(agent, id)`, if any — regardless of its validity window
    /// (callers apply [`visible_at`] themselves, matching where the ported
    /// SQL applies its own `WHERE` filter: after the join, not before).
    fn entity(&mut self, agent: &str, id: &str) -> Result<Option<GraphEntity>>;
    /// Every outgoing edge of `(agent, src)`, in no particular order —
    /// [`run`] does not depend on the order edges are returned in (output
    /// ordering is imposed afterward, see [`run`]'s doc).
    fn out_edges(&mut self, agent: &str, src: &str) -> Result<Vec<OutEdge>>;
}

/// The `valid_until` gate shared by entities and edges — see the module doc
/// for why `valid_from` is deliberately never checked.
#[must_use]
pub(crate) fn visible_at(valid_until: Option<i64>, now: i64) -> bool {
    valid_until.is_none_or(|until| now < until)
}

/// Breadth-first traversal from `start`, following directed edges up to
/// `max_depth` hops, scoped to `agent`. See the module doc for the exact,
/// ported-from-CTE semantics; summarized:
///
/// - `start` is never in the result.
/// - A node is only *expanded* (its own out-edges walked) when reached at a
///   depth strictly less than `max_depth`; a node reached at exactly
///   `max_depth` still appears in the result, just isn't expanded further.
/// - An edge is only followed if [`visible_at`] holds for its `valid_until`.
/// - A reached node only appears in the output if it has an entity record
///   AND [`visible_at`] holds for that entity's `valid_until` — this check
///   is independent of edge visibility, exactly like the CTE's separate
///   `JOIN entity ... WHERE (...)` clause.
/// - Output is ordered by `(depth, id)` ascending, matching the ported
///   `ORDER BY d, e.id`.
pub(crate) fn run(
    provider: &mut impl GraphProvider,
    agent: &str,
    start: &str,
    max_depth: u32,
    now: i64,
) -> Result<Vec<Reached>> {
    let mut depth_of: HashMap<String, u32> = HashMap::new();
    let mut visited: HashSet<String> = HashSet::new();
    let mut frontier: VecDeque<(String, u32)> = VecDeque::new();

    visited.insert(start.to_string());
    frontier.push_back((start.to_string(), 0));

    while let Some((node, depth)) = frontier.pop_front() {
        if depth >= max_depth {
            continue;
        }
        for (_relation, dst, meta) in provider.out_edges(agent, &node)? {
            if !visible_at(meta.valid_until, now) {
                continue;
            }
            if !visited.insert(dst.clone()) {
                // Already reached at <= this depth (BFS level order) — a
                // re-visit here can never lower an already-recorded depth.
                continue;
            }
            let next_depth = depth + 1;
            depth_of.insert(dst.clone(), next_depth);
            frontier.push_back((dst, next_depth));
        }
    }

    let mut ids: Vec<&String> = depth_of.keys().collect();
    ids.sort_unstable();
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let depth = depth_of[id];
        if let Some(entity) = provider.entity(agent, id)?
            && visible_at(entity.valid_until, now)
        {
            out.push(Reached {
                id: id.clone(),
                kind: entity.kind,
                label: entity.label,
                depth,
            });
        }
    }
    out.sort_by(|a, b| a.depth.cmp(&b.depth).then_with(|| a.id.cmp(&b.id)));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap as Map;

    /// A trivial in-memory provider used only to unit-test `run` in
    /// isolation from both real flavors — the flavor-level scenarios (ported
    /// 1:1 from `crates/basemyai/tests/graph.rs`) live in
    /// `tests/graph_parity.rs` and run against [`super::super::ram::RamGraph`]
    /// and [`super::super::persistent::PersistentGraph`] directly.
    #[derive(Default)]
    struct TestGraph {
        entities: Map<(String, String), GraphEntity>,
        edges: Map<(String, String), Vec<OutEdge>>,
    }

    impl TestGraph {
        fn entity(&mut self, agent: &str, id: &str, kind: &str, label: &str, valid_until: Option<i64>) {
            self.entities.insert(
                (agent.to_string(), id.to_string()),
                GraphEntity {
                    kind: kind.to_string(),
                    label: label.to_string(),
                    valid_from: 0,
                    valid_until,
                },
            );
        }

        fn edge(&mut self, agent: &str, src: &str, relation: &str, dst: &str) {
            self.edges
                .entry((agent.to_string(), src.to_string()))
                .or_default()
                .push((
                    relation.to_string(),
                    dst.to_string(),
                    GraphEdgeMeta {
                        weight: 1.0,
                        valid_from: 0,
                        valid_until: None,
                    },
                ));
        }
    }

    impl GraphProvider for TestGraph {
        fn entity(&mut self, agent: &str, id: &str) -> Result<Option<GraphEntity>> {
            Ok(self.entities.get(&(agent.to_string(), id.to_string())).cloned())
        }
        fn out_edges(&mut self, agent: &str, src: &str) -> Result<Vec<OutEdge>> {
            Ok(self
                .edges
                .get(&(agent.to_string(), src.to_string()))
                .cloned()
                .unwrap_or_default())
        }
    }

    #[test]
    fn empty_graph_returns_no_hits() {
        let mut g = TestGraph::default();
        let out = run(&mut g, "a", "start", 3, 0).expect("run");
        assert!(out.is_empty());
    }

    #[test]
    fn max_depth_zero_returns_nothing() {
        let mut g = TestGraph::default();
        g.entity("a", "x", "t", "X", None);
        g.entity("a", "y", "t", "Y", None);
        g.edge("a", "x", "rel", "y");
        let out = run(&mut g, "a", "x", 0, 0).expect("run");
        assert!(out.is_empty());
    }

    #[test]
    fn diamond_shape_keeps_minimum_depth() {
        // start -> a -> end, start -> b -> end : end must be depth 2, once.
        let mut g = TestGraph::default();
        for id in ["start", "a", "b", "end"] {
            g.entity("a", id, "t", id, None);
        }
        g.edge("a", "start", "r", "a");
        g.edge("a", "start", "r", "b");
        g.edge("a", "a", "r", "end");
        g.edge("a", "b", "r", "end");
        let out = run(&mut g, "a", "start", 5, 0).expect("run");
        let end_hits: Vec<_> = out.iter().filter(|r| r.id == "end").collect();
        assert_eq!(end_hits.len(), 1, "end must appear exactly once, not once per path");
        assert_eq!(end_hits[0].depth, 2);
    }
}
