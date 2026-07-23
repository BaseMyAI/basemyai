// SPDX-License-Identifier: BUSL-1.1
//! Cross-structure logical verification (ADR-040 §2, N9.3) — the
//! `FullLogical` half of [`super::verify`]: given the store's merged live
//! key-value view (SSTs oldest-to-newest + WAL overlay, tombstones dropped —
//! exactly what a fresh `Engine::open` would serve), check that the four
//! reserved `idx/` keyspaces agree with each other.
//!
//! What is an **error** vs a **warning** follows one rule (ADR-040 §1's
//! data classification): an inconsistency the engine heals *automatically
//! and correctly* on its own (vector metadata rebuilt at open, missing BM25
//! stats healed on the next search, missing allocator healed from data) is
//! a warning; anything the engine would trust as-is — and therefore act
//! wrongly on — is an error (a stale-but-decodable allocator, a
//! wrong-but-decodable stats record, a broken record↔vecmap link).
//!
//! Scope guard: the record ↔ vecmap ↔ vector-node linkage checks only run
//! when the memory keyspace (`idx/memory/`) is populated — a standalone
//! vector index (the engine is a mechanism, consumers compose it) is a
//! legitimate store where "live node with no vecmap entry" means nothing.
//! The self-contained checks (vector neighbors/dims, FTS forward↔inverted,
//! graph endpoints) always run over whatever keyspaces exist.
//!
//! One [`LogicalView`] pass ([`parse`]) parses every reserved key before any
//! check runs — [`memory`], [`vector`], [`fts`], [`graph`] each look only at
//! the parsed view, never back at raw bytes.

use std::collections::BTreeMap;
use std::path::Path;

use crate::idx::fts::{FtsStats, docterms};
use crate::idx::graph::entity as graph_entity;
use crate::idx::memory::{meta as memory_meta, record, vecmap};
use crate::idx::vector::{VectorIndexMeta, node};
use crate::store::Value;
use crate::store::verify::VerifyReport;

mod fts;
mod graph;
mod memory;
mod parse;
#[cfg(test)]
mod test_support;
mod vector;

/// Everything the single parse pass extracts from the merged view — the
/// checks below only ever look at this, never back at raw bytes.
#[derive(Default)]
struct LogicalView {
    /// `(agent, id)` → decoded record.
    records: BTreeMap<(String, String), record::MemoryRecord>,
    /// `vec_id` → decoded reverse mapping.
    vecmap: BTreeMap<u64, vecmap::VecMapEntry>,
    memory_meta: Option<memory_meta::MemoryIndexMeta>,
    /// `vec_id` → decoded vector node.
    nodes: BTreeMap<u64, node::VectorNode>,
    vector_meta: Option<VectorIndexMeta>,
    /// `(agent, term, vec_id)` → `tf`.
    postings: BTreeMap<(String, String, u64), u32>,
    /// `(agent, vec_id)` → decoded doc-terms.
    docterms: BTreeMap<(String, u64), docterms::FtsDocTerms>,
    /// `agent` → stored BM25 stats.
    fts_stats: BTreeMap<String, FtsStats>,
    /// `(agent, entity_id)` present in the graph.
    entities: BTreeMap<(String, String), graph_entity::GraphEntity>,
    /// `(agent, src, relation, dst)` — endpoints are what the checks need;
    /// the decoded meta is only kept implicitly (decode is the check).
    edges: Vec<(String, String, String, String)>,
}

/// Entry point called by [`super::verify::verify_store`] in `FullLogical`
/// mode, over a physically-verified merged live view. Issues carry `dir` as
/// their path: every finding here spans structures, not one file.
pub(crate) fn check_logical(kv: &BTreeMap<Vec<u8>, Value>, dir: &Path, report: &mut VerifyReport) {
    let view = parse::parse_view(kv, dir, report);
    let memory_populated = !view.records.is_empty() || !view.vecmap.is_empty() || view.memory_meta.is_some();
    if memory_populated {
        memory::check_memory_links(&view, dir, report);
    }
    vector::check_vector_index(&view, dir, report);
    fts::check_fts(&view, memory_populated, dir, report);
    graph::check_graph(&view, dir, report);
}

#[cfg(test)]
mod tests {
    use super::test_support::{build_composed_store, full_logical, has_error, has_warning};
    use crate::store::verify::IssueKind;

    #[test]
    fn composed_store_with_tombstones_and_wal_tail_is_logically_healthy() {
        let dir = build_composed_store();
        let report = full_logical(dir.path());
        assert!(report.healthy, "errors: {:?}", report.errors);
        assert!(report.warnings.is_empty(), "warnings: {:?}", report.warnings);
        assert!(report.blocks_checked > 0);
    }

    #[test]
    fn logical_pass_is_skipped_with_a_warning_when_physical_errors_exist() {
        use crate::format::sst_block::SST_HEADER_TOTAL_LEN;
        let dir = build_composed_store();
        let sst = std::fs::read_dir(dir.path())
            .expect("read_dir")
            .find_map(|e| {
                let path = e.expect("entry").path();
                (path.extension().and_then(|x| x.to_str()) == Some("sst")).then_some(path)
            })
            .expect("one sst");
        let mut raw = std::fs::read(&sst).expect("read sst");
        raw[SST_HEADER_TOTAL_LEN + 4] ^= 0xFF;
        std::fs::write(&sst, &raw).expect("write tampered");

        let report = full_logical(dir.path());
        assert!(!report.healthy);
        assert!(
            has_warning(&report, IssueKind::LogicalChecksSkipped),
            "warnings: {:?}",
            report.warnings
        );
        // No cascaded logical noise: the only errors are physical ones.
        assert!(
            !has_error(&report, IssueKind::MemoryLinkBroken) && !has_error(&report, IssueKind::FtsLinkBroken),
            "errors: {:?}",
            report.errors
        );
    }
}
