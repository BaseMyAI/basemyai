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

use std::collections::BTreeMap;
use std::path::Path;

use crate::idx::fts::{FtsStats, docterms, postings, stats};
use crate::idx::graph::{edge as graph_edge, entity as graph_entity};
use crate::idx::memory::{meta as memory_meta, record, vecmap};
use crate::idx::vector::{VectorIndexMeta, meta as vector_meta, node};
use crate::key::{fts_index, graph_index, memory_index, vector_index};
use crate::store::Value;
use crate::store::verify::{IssueKind, VerifyReport};

/// Splits one `u32`-length-prefixed field off `buf`: `(field, rest)`.
/// Wire-distrust discipline: the length is bounded against the actual
/// remaining bytes before any slicing — malformed input yields `None`,
/// never a panic.
fn take_len_prefixed(buf: &[u8]) -> Option<(&[u8], &[u8])> {
    let len_bytes: [u8; 4] = buf.get(0..4)?.try_into().ok()?;
    let len = u32::from_be_bytes(len_bytes) as usize;
    let rest = buf.get(4..)?;
    Some((rest.get(..len)?, rest.get(len..)?))
}

fn take_str(buf: &[u8]) -> Option<(String, &[u8])> {
    let (field, rest) = take_len_prefixed(buf)?;
    Some((String::from_utf8(field.to_vec()).ok()?, rest))
}

fn take_u64(buf: &[u8]) -> Option<u64> {
    let raw: [u8; 8] = buf.try_into().ok()?;
    Some(u64::from_be_bytes(raw))
}

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

/// Parses every key under the reserved `idx/` prefixes into [`LogicalView`],
/// reporting undecodable keys/values as it goes. Keys outside `idx/` are a
/// consumer's own keyspace — mechanism, not this pass's business.
fn parse_view(kv: &BTreeMap<Vec<u8>, Value>, dir: &Path, report: &mut VerifyReport) -> LogicalView {
    let mut view = LogicalView::default();
    for (key, value) in kv.range(b"idx/".to_vec()..b"idx0".to_vec()) {
        let key = key.as_slice();
        let malformed = |report: &mut VerifyReport| {
            report.error(
                IssueKind::IdxKeyMalformed,
                dir,
                format!(
                    "key {:?} sits in a reserved idx/ keyspace but does not parse against its layout",
                    String::from_utf8_lossy(key)
                ),
            );
        };
        let corrupt = |report: &mut VerifyReport, what: &str, e: &dyn std::fmt::Display| {
            report.error(
                IssueKind::IdxValueCorrupt,
                dir,
                format!("{what} at key {:?}: {e}", String::from_utf8_lossy(key)),
            );
        };

        if key == memory_index::META_KEY {
            match memory_meta::decode(value) {
                Ok(meta) => view.memory_meta = Some(meta),
                Err(e) => corrupt(report, "memory allocator metadata", &e),
            }
        } else if let Some(suffix) = key.strip_prefix(memory_index::RECORD_PREFIX) {
            match take_str(suffix).and_then(|(agent, rest)| Some((agent, String::from_utf8(rest.to_vec()).ok()?))) {
                None => malformed(report),
                Some((agent, id)) => match record::decode(value) {
                    Ok(rec) => {
                        view.records.insert((agent, id), rec);
                    }
                    Err(e) => corrupt(report, "memory record", &e),
                },
            }
        } else if let Some(suffix) = key.strip_prefix(memory_index::VECMAP_PREFIX) {
            match take_u64(suffix) {
                None => malformed(report),
                Some(vec_id) => match vecmap::decode(value) {
                    Ok(entry) => {
                        view.vecmap.insert(vec_id, entry);
                    }
                    Err(e) => corrupt(report, "memory vecmap entry", &e),
                },
            }
        } else if key == vector_index::META_KEY {
            match vector_meta::decode(value) {
                Ok(meta) => view.vector_meta = Some(meta),
                // Healable by design: `PersistentVectorIndex::open` rebuilds
                // the metadata from the stored vectors (ADR-026).
                Err(e) => report.warning(
                    IssueKind::VectorMetaInconsistent,
                    dir,
                    format!("vector index metadata is corrupt (rebuilt from data at the next open): {e}"),
                ),
            }
        } else if let Some(suffix) = key.strip_prefix(vector_index::NODE_PREFIX) {
            match take_u64(suffix) {
                None => malformed(report),
                Some(id) => match node::decode(value) {
                    Ok(n) => {
                        view.nodes.insert(id, n);
                    }
                    Err(e) => corrupt(report, "vector node", &e),
                },
            }
        } else if let Some(suffix) = key.strip_prefix(fts_index::POSTINGS_PREFIX) {
            let parsed = take_str(suffix)
                .and_then(|(agent, rest)| take_str(rest).map(|(term, rest)| (agent, term, rest)))
                .and_then(|(agent, term, rest)| Some((agent, term, take_u64(rest)?)));
            match parsed {
                None => malformed(report),
                Some((agent, term, vec_id)) => match postings::decode(value) {
                    Ok(p) => {
                        view.postings.insert((agent, term, vec_id), p.tf);
                    }
                    Err(e) => corrupt(report, "fts posting", &e),
                },
            }
        } else if let Some(suffix) = key.strip_prefix(fts_index::DOCTERMS_PREFIX) {
            let parsed = take_str(suffix).and_then(|(agent, rest)| Some((agent, take_u64(rest)?)));
            match parsed {
                None => malformed(report),
                Some((agent, vec_id)) => match docterms::decode(value) {
                    Ok(doc) => {
                        view.docterms.insert((agent, vec_id), doc);
                    }
                    Err(e) => corrupt(report, "fts doc-terms", &e),
                },
            }
        } else if let Some(suffix) = key.strip_prefix(fts_index::META_PREFIX) {
            match take_str(suffix).and_then(|(agent, rest)| rest.is_empty().then_some(agent)) {
                None => malformed(report),
                Some(agent) => match stats::decode(value) {
                    Ok(s) => {
                        view.fts_stats.insert(agent, s);
                    }
                    // Healable by design: stats are re-derived from
                    // doc-terms on the next search (ADR-028 §3).
                    Err(e) => report.warning(
                        IssueKind::FtsStatsInconsistent,
                        dir,
                        format!("fts stats record is corrupt (healed from doc-terms on the next search): {e}"),
                    ),
                },
            }
        } else if let Some(suffix) = key.strip_prefix(graph_index::ENTITY_PREFIX) {
            match take_str(suffix).and_then(|(agent, rest)| Some((agent, String::from_utf8(rest.to_vec()).ok()?))) {
                None => malformed(report),
                Some((agent, id)) => match graph_entity::decode(value) {
                    Ok(entity) => {
                        view.entities.insert((agent, id), entity);
                    }
                    Err(e) => corrupt(report, "graph entity", &e),
                },
            }
        } else if let Some(suffix) = key.strip_prefix(graph_index::EDGE_PREFIX) {
            let parsed = take_str(suffix)
                .and_then(|(agent, rest)| take_str(rest).map(|(src, rest)| (agent, src, rest)))
                .and_then(|(agent, src, rest)| take_str(rest).map(|(relation, rest)| (agent, src, relation, rest)))
                .and_then(|(agent, src, relation, rest)| {
                    Some((agent, src, relation, String::from_utf8(rest.to_vec()).ok()?))
                });
            match parsed {
                None => malformed(report),
                Some((agent, src, relation, dst)) => match graph_edge::decode(value) {
                    Ok(_) => view.edges.push((agent, src, relation, dst)),
                    Err(e) => corrupt(report, "graph edge", &e),
                },
            }
        } else {
            malformed(report);
        }
    }
    view
}

/// Record ↔ vecmap ↔ vector-node linkage (ADR-027) plus allocator
/// monotonicity. Only meaningful when the memory keyspace is populated —
/// see the module doc's scope guard.
fn check_memory_links(view: &LogicalView, dir: &Path, report: &mut VerifyReport) {
    let mut vec_id_owner: BTreeMap<u64, &(String, String)> = BTreeMap::new();
    for (key, rec) in &view.records {
        let (agent, id) = key;
        if let Some(previous) = vec_id_owner.insert(rec.vec_id, key) {
            report.error(
                IssueKind::MemoryLinkBroken,
                dir,
                format!(
                    "records ({}, {}) and ({agent}, {id}) both claim vec_id {} — ids are never shared or reused",
                    previous.0, previous.1, rec.vec_id
                ),
            );
        }
        match view.vecmap.get(&rec.vec_id) {
            None => report.error(
                IssueKind::MemoryLinkBroken,
                dir,
                format!(
                    "record ({agent}, {id}) points at vec_id {} but no vecmap entry maps it back",
                    rec.vec_id
                ),
            ),
            Some(entry) if (&entry.agent, &entry.id) != (agent, id) => report.error(
                IssueKind::MemoryLinkBroken,
                dir,
                format!(
                    "record ({agent}, {id}) points at vec_id {} but the vecmap maps it to ({}, {})",
                    rec.vec_id, entry.agent, entry.id
                ),
            ),
            Some(_) => {}
        }
        match view.nodes.get(&rec.vec_id) {
            None => report.error(
                IssueKind::MemoryLinkBroken,
                dir,
                format!(
                    "record ({agent}, {id}) points at vec_id {} but no vector node exists",
                    rec.vec_id
                ),
            ),
            Some(n) if n.deleted => report.error(
                IssueKind::MemoryLinkBroken,
                dir,
                format!(
                    "record ({agent}, {id}) points at vec_id {} whose vector node is tombstoned — \
                     a forget deletes the record and tombstones the node in one batch, never half",
                    rec.vec_id
                ),
            ),
            Some(_) => {}
        }
    }
    for (vec_id, entry) in &view.vecmap {
        match view.records.get(&(entry.agent.clone(), entry.id.clone())) {
            None => report.error(
                IssueKind::MemoryLinkBroken,
                dir,
                format!(
                    "vecmap entry {vec_id} -> ({}, {}) resolves to no record — an orphan mapping",
                    entry.agent, entry.id
                ),
            ),
            Some(rec) if rec.vec_id != *vec_id => report.error(
                IssueKind::MemoryLinkBroken,
                dir,
                format!(
                    "vecmap entry {vec_id} -> ({}, {}) disagrees with that record's own vec_id {}",
                    entry.agent, entry.id, rec.vec_id
                ),
            ),
            Some(_) => {}
        }
    }
    for (id, n) in &view.nodes {
        if !n.deleted && !view.vecmap.contains_key(id) {
            report.error(
                IssueKind::MemoryLinkBroken,
                dir,
                format!("live vector node {id} has no vecmap entry — a search hit on it could never resolve"),
            );
        }
    }

    // Allocator monotonicity (ADR-027 §4): a decodable-but-stale counter is
    // trusted by `open` (healing only fires on absent/corrupt), so the next
    // insert would reuse an id.
    let max_used = view.nodes.keys().chain(view.vecmap.keys()).max().copied();
    match (view.memory_meta, max_used) {
        (Some(meta), Some(max)) if meta.next_vec_id <= max => report.error(
            IssueKind::AllocatorStale,
            dir,
            format!(
                "next_vec_id {} is not above the highest vector id in use ({max}) — the next insert would reuse an id",
                meta.next_vec_id
            ),
        ),
        (None, Some(max)) => report.warning(
            IssueKind::AllocatorStale,
            dir,
            format!(
                "allocator metadata is absent while vector ids up to {max} are in use — \
                 healed from data at the next open (ADR-027 §4)"
            ),
        ),
        _ => {}
    }
}

/// Vector-index internal consistency: neighbor existence, dimensions, and
/// the (healable) metadata gauges. Runs whether or not the memory keyspace
/// exists — these invariants are the index's own.
fn check_vector_index(view: &LogicalView, dir: &Path, report: &mut VerifyReport) {
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

/// FTS forward ↔ inverted agreement and recomputed BM25 stats (ADR-028),
/// plus the cross-structure agent-isolation check against the vecmap.
fn check_fts(view: &LogicalView, memory_populated: bool, dir: &Path, report: &mut VerifyReport) {
    for ((agent, vec_id), doc) in &view.docterms {
        for term in &doc.terms {
            if term.tf == 0 {
                report.error(
                    IssueKind::FtsLinkBroken,
                    dir,
                    format!("doc-terms ({agent}, {vec_id}) lists term {:?} with tf 0", term.term),
                );
                continue;
            }
            match view.postings.get(&(agent.clone(), term.term.clone(), *vec_id)) {
                None => report.error(
                    IssueKind::FtsLinkBroken,
                    dir,
                    format!(
                        "doc-terms ({agent}, {vec_id}) lists term {:?} but no matching posting exists",
                        term.term
                    ),
                ),
                Some(tf) if *tf != term.tf => report.error(
                    IssueKind::FtsLinkBroken,
                    dir,
                    format!(
                        "term {:?} of doc ({agent}, {vec_id}) has tf {} in doc-terms but {tf} in its posting",
                        term.term, term.tf
                    ),
                ),
                Some(_) => {}
            }
        }
        if memory_populated {
            match view.vecmap.get(vec_id) {
                None => report.error(
                    IssueKind::FtsLinkBroken,
                    dir,
                    format!("doc-terms ({agent}, {vec_id}) references a vec_id no vecmap entry maps"),
                ),
                Some(entry) if entry.agent != *agent => report.error(
                    IssueKind::AgentIsolationBreach,
                    dir,
                    format!(
                        "fts document ({agent}, {vec_id}) is indexed under agent {agent:?} but vec_id {vec_id} \
                         belongs to agent {:?}",
                        entry.agent
                    ),
                ),
                Some(_) => {}
            }
        }
    }
    for (agent, term, vec_id) in view.postings.keys() {
        let listed = view
            .docterms
            .get(&(agent.clone(), *vec_id))
            .is_some_and(|doc| doc.terms.iter().any(|t| &t.term == term));
        if !listed {
            report.error(
                IssueKind::FtsLinkBroken,
                dir,
                format!(
                    "posting ({agent}, {term:?}, {vec_id}) has no matching doc-terms entry — \
                     an orphan a delete could never clean up"
                ),
            );
        }
    }

    // Recomputed BM25 stats per agent (ADR-028 §3): `doc_count` and
    // `total_terms` derived from doc-terms are the ground truth.
    let mut recomputed: BTreeMap<&str, FtsStats> = BTreeMap::new();
    for ((agent, _), doc) in &view.docterms {
        let entry = recomputed.entry(agent).or_default();
        entry.doc_count += 1;
        entry.total_terms += doc.terms.iter().map(|t| u64::from(t.tf)).sum::<u64>();
    }
    for (agent, expected) in &recomputed {
        match view.fts_stats.get(*agent) {
            None => report.warning(
                IssueKind::FtsStatsInconsistent,
                dir,
                format!(
                    "agent {agent:?} has {} indexed documents but no stats record — \
                     healed from doc-terms on the next search",
                    expected.doc_count
                ),
            ),
            Some(stored) if stored != expected => report.error(
                IssueKind::FtsStatsInconsistent,
                dir,
                format!(
                    "agent {agent:?} stats record says doc_count {} / total_terms {} but doc-terms \
                     recompute to {} / {} — an intact-but-wrong record silently skews every BM25 score",
                    stored.doc_count, stored.total_terms, expected.doc_count, expected.total_terms
                ),
            ),
            Some(_) => {}
        }
    }
    for (agent, stored) in &view.fts_stats {
        if !recomputed.contains_key(agent.as_str()) && *stored != FtsStats::default() {
            report.error(
                IssueKind::FtsStatsInconsistent,
                dir,
                format!(
                    "agent {agent:?} has a stats record (doc_count {}) but no indexed documents at all",
                    stored.doc_count
                ),
            );
        }
    }
}

/// Graph edge endpoints — warnings only: the engine's graph API never
/// enforced endpoint existence, so a dangling endpoint is a tolerated (if
/// suspicious) state, not a broken invariant.
fn check_graph(view: &LogicalView, dir: &Path, report: &mut VerifyReport) {
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

/// Entry point called by [`super::verify::verify_store`] in `FullLogical`
/// mode, over a physically-verified merged live view. Issues carry `dir` as
/// their path: every finding here spans structures, not one file.
pub(crate) fn check_logical(kv: &BTreeMap<Vec<u8>, Value>, dir: &Path, report: &mut VerifyReport) {
    let view = parse_view(kv, dir, report);
    let memory_populated = !view.records.is_empty() || !view.vecmap.is_empty() || view.memory_meta.is_some();
    if memory_populated {
        check_memory_links(&view, dir, report);
    }
    check_vector_index(&view, dir, report);
    check_fts(&view, memory_populated, dir, report);
    check_graph(&view, dir, report);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::idx::fts::PersistentFts;
    use crate::idx::graph::{GraphEdgeMeta, GraphEntity, PersistentGraph};
    use crate::idx::memory::{NewMemoryRecord, PersistentMemoryIndex};
    use crate::idx::vector::{PersistentVectorIndex, VectorIndexParams};
    use crate::store::verify::{VerifyMode, verify_store};
    use crate::store::{Engine, EngineOptions};

    const DIM: usize = 4;

    fn small_options() -> EngineOptions {
        EngineOptions {
            memtable_flush_threshold: 1000,
            compaction_sst_threshold: 100,
            block_size: 512,
            ..EngineOptions::default()
        }
    }

    fn new_record(content: &'static str) -> NewMemoryRecord<'static> {
        NewMemoryRecord {
            layer: "episodic",
            content,
            source: "user",
            valid_from: 0,
            valid_until: None,
            importance: 1.0,
            last_access: 0,
        }
    }

    /// Builds a fully composed store — memory records with FTS content on
    /// two agents, a forgotten memory (tombstoned vector node), and a graph
    /// with entities and edges — flushed to SSTs plus an unflushed WAL
    /// tail, so `FullLogical` exercises both layers of the merged view.
    fn build_composed_store() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut engine = Engine::open_with_options(dir.path(), small_options()).expect("open");
        let mut vectors =
            PersistentVectorIndex::open(&mut engine, VectorIndexParams::with_dim(DIM)).expect("open vectors");
        let fts = PersistentFts::new();
        let mut memory = PersistentMemoryIndex::open(&engine).expect("open memory");

        for (i, content) in ["le chat mange la souris", "le chien dort", "la souris danse"]
            .iter()
            .enumerate()
        {
            memory
                .put(
                    &mut engine,
                    &mut vectors,
                    &fts,
                    "agent-a",
                    &format!("m{i}"),
                    &new_record(content),
                    vec![0.1 * (i + 1) as f32; DIM],
                )
                .expect("put");
        }
        memory
            .put(
                &mut engine,
                &mut vectors,
                &fts,
                "agent-b",
                "m0",
                &new_record("le hibou observe"),
                vec![0.9; DIM],
            )
            .expect("put agent-b");
        // A forgotten memory: record+vecmap+FTS removed, node tombstoned —
        // a healthy store contains tombstones, verify must not flag them.
        memory
            .forget(&mut engine, &mut vectors, &fts, "agent-a", "m2")
            .expect("forget");

        let graph = PersistentGraph::new();
        for id in ["alice", "acme"] {
            graph
                .upsert_entity(
                    &mut engine,
                    "agent-a",
                    id,
                    GraphEntity {
                        kind: "person".to_string(),
                        label: id.to_string(),
                        valid_from: 0,
                        valid_until: None,
                    },
                )
                .expect("upsert entity");
        }
        graph
            .upsert_edge(
                &mut engine,
                "agent-a",
                "alice",
                "employeur",
                "acme",
                GraphEdgeMeta {
                    weight: 1.0,
                    valid_from: 0,
                    valid_until: None,
                },
            )
            .expect("upsert edge");

        engine.flush().expect("flush");
        // One more memory left in the WAL only: the merged view must
        // overlay it, or its record/vecmap/node links would look broken.
        memory
            .put(
                &mut engine,
                &mut vectors,
                &fts,
                "agent-a",
                "wal-tail",
                &new_record("reste dans le wal"),
                vec![0.5; DIM],
            )
            .expect("put wal tail");
        // Drop without flush/close: the last put stays WAL-only.
        drop(engine);
        dir
    }

    fn full_logical(dir: &Path) -> VerifyReport {
        verify_store(dir, None, VerifyMode::FullLogical).expect("verify")
    }

    fn has_error(report: &VerifyReport, kind: IssueKind) -> bool {
        report.errors.iter().any(|e| e.kind == kind)
    }

    fn has_warning(report: &VerifyReport, kind: IssueKind) -> bool {
        report.warnings.iter().any(|w| w.kind == kind)
    }

    #[test]
    fn composed_store_with_tombstones_and_wal_tail_is_logically_healthy() {
        let dir = build_composed_store();
        let report = full_logical(dir.path());
        assert!(report.healthy, "errors: {:?}", report.errors);
        assert!(report.warnings.is_empty(), "warnings: {:?}", report.warnings);
        assert!(report.blocks_checked > 0);
    }

    #[test]
    fn missing_vecmap_entry_is_a_broken_memory_link() {
        let dir = build_composed_store();
        {
            let mut engine = Engine::open_with_options(dir.path(), small_options()).expect("open");
            let memory = PersistentMemoryIndex::open(&engine).expect("open memory");
            let rec = memory.get(&engine, "agent-a", "m0").expect("get").expect("m0 exists");
            engine
                .delete(memory_index::vecmap_key(rec.vec_id).as_bytes())
                .expect("delete vecmap");
            engine.close().expect("close");
        }
        let report = full_logical(dir.path());
        assert!(!report.healthy);
        assert!(
            has_error(&report, IssueKind::MemoryLinkBroken),
            "errors: {:?}",
            report.errors
        );
    }

    #[test]
    fn orphan_vecmap_entry_is_a_broken_memory_link() {
        let dir = build_composed_store();
        {
            let mut engine = Engine::open_with_options(dir.path(), small_options()).expect("open");
            engine
                .delete(
                    memory_index::record_key("agent-a", "m0")
                        .expect("record key")
                        .as_bytes(),
                )
                .expect("delete record");
            engine.close().expect("close");
        }
        let report = full_logical(dir.path());
        assert!(!report.healthy);
        assert!(
            has_error(&report, IssueKind::MemoryLinkBroken),
            "errors: {:?}",
            report.errors
        );
    }

    #[test]
    fn stale_allocator_is_an_error_not_a_healable_state() {
        let dir = build_composed_store();
        {
            let mut engine = Engine::open_with_options(dir.path(), small_options()).expect("open");
            let stale = memory_meta::encode(&memory_meta::MemoryIndexMeta { next_vec_id: 0 }).expect("encode");
            engine.put(memory_index::META_KEY, &stale).expect("put stale meta");
            engine.close().expect("close");
        }
        let report = full_logical(dir.path());
        assert!(!report.healthy);
        assert!(
            has_error(&report, IssueKind::AllocatorStale),
            "errors: {:?}",
            report.errors
        );
    }

    #[test]
    fn undecodable_idx_value_is_reported_with_its_key() {
        let dir = build_composed_store();
        {
            let mut engine = Engine::open_with_options(dir.path(), small_options()).expect("open");
            engine
                .put(vector_index::node_key(0).as_bytes(), b"not a node block")
                .expect("put garbage node");
            engine.close().expect("close");
        }
        let report = full_logical(dir.path());
        assert!(!report.healthy);
        assert!(
            has_error(&report, IssueKind::IdxValueCorrupt),
            "errors: {:?}",
            report.errors
        );
    }

    #[test]
    fn malformed_key_in_reserved_keyspace_is_an_error() {
        let dir = build_composed_store();
        {
            let mut engine = Engine::open_with_options(dir.path(), small_options()).expect("open");
            // First 4 bytes of "junk" decode to a giant agent length — the
            // bounded parse must reject it, not panic.
            engine.put(b"idx/memory/rec/junk", b"x").expect("put foreign key");
            engine.close().expect("close");
        }
        let report = full_logical(dir.path());
        assert!(!report.healthy);
        assert!(
            has_error(&report, IssueKind::IdxKeyMalformed),
            "errors: {:?}",
            report.errors
        );
    }

    #[test]
    fn missing_docterms_makes_postings_orphans() {
        let dir = build_composed_store();
        {
            let mut engine = Engine::open_with_options(dir.path(), small_options()).expect("open");
            let memory = PersistentMemoryIndex::open(&engine).expect("open memory");
            let rec = memory.get(&engine, "agent-a", "m0").expect("get").expect("m0 exists");
            engine
                .delete(
                    fts_index::docterms_key("agent-a", rec.vec_id)
                        .expect("docterms key")
                        .as_bytes(),
                )
                .expect("delete docterms");
            engine.close().expect("close");
        }
        let report = full_logical(dir.path());
        assert!(!report.healthy);
        assert!(
            has_error(&report, IssueKind::FtsLinkBroken),
            "errors: {:?}",
            report.errors
        );
        // Deleting one doc's terms also desyncs the recomputed stats.
        assert!(
            has_error(&report, IssueKind::FtsStatsInconsistent),
            "errors: {:?}",
            report.errors
        );
    }

    #[test]
    fn intact_but_wrong_bm25_stats_are_an_error() {
        let dir = build_composed_store();
        {
            let mut engine = Engine::open_with_options(dir.path(), small_options()).expect("open");
            let lying = stats::encode(&FtsStats {
                doc_count: 999,
                total_terms: 1,
            })
            .expect("encode");
            engine
                .put(fts_index::meta_key("agent-a").expect("meta key").as_bytes(), &lying)
                .expect("put lying stats");
            engine.close().expect("close");
        }
        let report = full_logical(dir.path());
        assert!(!report.healthy);
        assert!(
            has_error(&report, IssueKind::FtsStatsInconsistent),
            "errors: {:?}",
            report.errors
        );
    }

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

    #[test]
    fn cross_agent_fts_document_is_an_isolation_breach() {
        let dir = build_composed_store();
        {
            let mut engine = Engine::open_with_options(dir.path(), small_options()).expect("open");
            let memory = PersistentMemoryIndex::open(&engine).expect("open memory");
            let rec = memory.get(&engine, "agent-a", "m0").expect("get").expect("m0 exists");
            // Forge agent-b FTS structures over a vec_id owned by agent-a —
            // exactly what structural key isolation must make impossible
            // through the real API.
            let doc = docterms::FtsDocTerms {
                terms: vec![docterms::DocTerm {
                    term: "vole".to_string(),
                    tf: 1,
                }],
            };
            engine
                .put(
                    fts_index::docterms_key("agent-b", rec.vec_id)
                        .expect("docterms key")
                        .as_bytes(),
                    &docterms::encode(&doc).expect("encode"),
                )
                .expect("put forged docterms");
            engine
                .put(
                    fts_index::postings_key("agent-b", "vole", rec.vec_id)
                        .expect("postings key")
                        .as_bytes(),
                    &postings::encode(&postings::FtsPosting { tf: 1 }).expect("encode"),
                )
                .expect("put forged posting");
            engine.close().expect("close");
        }
        let report = full_logical(dir.path());
        assert!(!report.healthy);
        assert!(
            has_error(&report, IssueKind::AgentIsolationBreach),
            "errors: {:?}",
            report.errors
        );
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

    #[test]
    fn standalone_vector_index_without_memory_keyspace_is_healthy() {
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
