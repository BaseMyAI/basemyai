// SPDX-License-Identifier: BUSL-1.1
//! Vector-index internal consistency: neighbor existence, dimensions, and
//! the (healable) metadata gauges — the vector-keyspace half of
//! [`super::check_logical`]. Runs whether or not the memory keyspace
//! exists; these invariants are the index's own.

use std::path::Path;

use crate::store::verify::{IssueKind, VerifyReport};

use super::LogicalView;

pub(super) fn check_vector_index(view: &LogicalView, dir: &Path, report: &mut VerifyReport) {
    let expected_dim = view
        .vector_meta
        .as_ref()
        .map(|m| m.params.dim)
        .or_else(|| view.nodes.values().next().map(|n| n.vector.len()));
    for (id, n) in &view.nodes {
        if let Some(dim) = expected_dim
            && n.vector.len() != dim
        {
            report.error(
                IssueKind::VectorDimMismatch,
                dir,
                format!(
                    "vector node {id} has dimension {} but the index is dimension {dim}",
                    n.vector.len()
                ),
            );
        }
        for neighbor in &n.neighbors {
            if !view.nodes.contains_key(neighbor) {
                report.error(
                    IssueKind::VectorNeighborMissing,
                    dir,
                    format!("vector node {id} lists neighbor {neighbor}, which has no node block"),
                );
            }
        }
    }
    if let Some(meta) = &view.vector_meta {
        // Every one of these is exactly what `PersistentVectorIndex::open`
        // detects and heals by rebuilding from the data — warnings, per the
        // module-doc rule.
        let live = view.nodes.values().filter(|n| !n.deleted).count() as u64;
        if meta.count != live {
            report.warning(
                IssueKind::VectorMetaInconsistent,
                dir,
                format!(
                    "metadata counts {} live vectors but {live} are stored (rebuilt at the next open)",
                    meta.count
                ),
            );
        }
        match meta.entry_point {
            Some(entry) if !view.nodes.contains_key(&entry) => report.warning(
                IssueKind::VectorMetaInconsistent,
                dir,
                format!("metadata entry point {entry} has no node block (rebuilt at the next open)"),
            ),
            None if live > 0 => report.warning(
                IssueKind::VectorMetaInconsistent,
                dir,
                format!("metadata has no entry point while {live} live vectors are stored (rebuilt at the next open)"),
            ),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{full_logical, small_options};
    use crate::idx::vector::{PersistentVectorIndex, VectorIndexParams};
    use crate::store::Engine;

    const DIM: usize = 4;

    #[test]
    fn standalone_vector_index_without_memory_keyspace_is_healthy() {
        // Not built from the shared `build_composed_store` fixture on
        // purpose: this test's whole point is a store with *no* memory
        // keyspace at all.
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let mut engine = Engine::open_with_options(dir.path(), small_options()).expect("open");
            let mut vectors =
                PersistentVectorIndex::open(&mut engine, VectorIndexParams::with_dim(DIM)).expect("open vectors");
            for i in 0..5u64 {
                vectors
                    .insert(&mut engine, i, vec![0.1 * (i + 1) as f32; DIM])
                    .expect("insert");
            }
            engine.close().expect("close");
        }
        let report = full_logical(dir.path());
        assert!(
            report.healthy,
            "live nodes without vecmap are legitimate without a memory keyspace: {:?}",
            report.errors
        );
    }
}
