// SPDX-License-Identifier: BUSL-1.1
//! Single parse pass over the store's merged live view: decodes every key
//! under the reserved `idx/` prefixes into a [`super::LogicalView`],
//! reporting undecodable keys/values along the way. Keys outside `idx/` are
//! a consumer's own keyspace — mechanism, not this pass's business.

use std::collections::BTreeMap;
use std::path::Path;

use crate::idx::fts::{docterms, postings, stats};
use crate::idx::graph::{edge as graph_edge, entity as graph_entity};
use crate::idx::memory::{meta as memory_meta, record, vecmap};
use crate::idx::vector::{meta as vector_meta, node};
use crate::key::{fts_index, graph_index, memory_index, vector_index};
use crate::store::Value;
use crate::store::verify::{IssueKind, VerifyReport};

use super::LogicalView;

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

/// Parses every key under the reserved `idx/` prefixes into [`LogicalView`],
/// reporting undecodable keys/values as it goes. Keys outside `idx/` are a
/// consumer's own keyspace — mechanism, not this pass's business.
pub(super) fn parse_view(kv: &BTreeMap<Vec<u8>, Value>, dir: &Path, report: &mut VerifyReport) -> LogicalView {
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

#[cfg(test)]
mod tests {
    use super::super::test_support::{build_composed_store, full_logical, has_error, small_options};
    use crate::key::vector_index;
    use crate::store::Engine;
    use crate::store::verify::IssueKind;

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
}
