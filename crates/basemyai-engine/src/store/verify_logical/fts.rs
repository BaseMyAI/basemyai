// SPDX-License-Identifier: BUSL-1.1
//! FTS forward ↔ inverted agreement and recomputed BM25 stats (ADR-028),
//! plus the cross-structure agent-isolation check against the vecmap — the
//! FTS-keyspace half of [`super::check_logical`].

use std::collections::BTreeMap;
use std::path::Path;

use crate::idx::fts::FtsStats;
use crate::store::verify::{IssueKind, VerifyReport};

use super::LogicalView;

pub(super) fn check_fts(view: &LogicalView, memory_populated: bool, dir: &Path, report: &mut VerifyReport) {
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

#[cfg(test)]
mod tests {
    use super::super::test_support::{build_composed_store, full_logical, has_error, small_options};
    use crate::idx::fts::{FtsStats, docterms, postings, stats};
    use crate::idx::memory::PersistentMemoryIndex;
    use crate::key::fts_index;
    use crate::store::Engine;
    use crate::store::verify::IssueKind;

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
}
