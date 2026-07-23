// SPDX-License-Identifier: BUSL-1.1
//! Graph edge endpoints — warnings only: the engine's graph API never
//! enforced endpoint existence, so a dangling endpoint is a tolerated (if
//! suspicious) state, not a broken invariant. The graph-keyspace half of
//! [`super::check_logical`].

use std::path::Path;

use crate::store::verify::{IssueKind, VerifyReport};

use super::LogicalView;

pub(super) fn check_graph(view: &LogicalView, dir: &Path, report: &mut VerifyReport) {
    for (agent, src, relation, dst) in &view.edges {
        for (role, endpoint) in [("source", src), ("destination", dst)] {
            if !view.entities.contains_key(&(agent.clone(), endpoint.clone())) {
                report.warning(
                    IssueKind::GraphEdgeDangling,
                    dir,
                    format!(
                        "edge ({agent}, {src}) -[{relation}]-> {dst}: {role} entity {endpoint:?} has no entity block"
                    ),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{build_composed_store, full_logical, has_warning, small_options};
    use crate::idx::graph::{GraphEdgeMeta, PersistentGraph};
    use crate::store::Engine;
    use crate::store::verify::IssueKind;

    #[test]
    fn dangling_graph_edge_is_a_warning_not_an_error() {
        let dir = build_composed_store();
        {
            let mut engine = Engine::open_with_options(dir.path(), small_options()).expect("open");
            let graph = PersistentGraph::new();
            graph
                .upsert_edge(
                    &mut engine,
                    "agent-a",
                    "alice",
                    "connait",
                    "fantome",
                    GraphEdgeMeta {
                        weight: 1.0,
                        valid_from: 0,
                        valid_until: None,
                    },
                )
                .expect("upsert dangling edge");
            engine.close().expect("close");
        }
        let report = full_logical(dir.path());
        assert!(report.healthy, "a dangling edge is tolerated: {:?}", report.errors);
        assert!(
            has_warning(&report, IssueKind::GraphEdgeDangling),
            "warnings: {:?}",
            report.warnings
        );
    }
}
