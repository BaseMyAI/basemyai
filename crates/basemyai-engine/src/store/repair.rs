// SPDX-License-Identifier: BUSL-1.1
//! Safe derived-index repair (ADR-040 §3, N9.5).
//!
//! This module deliberately never rewrites memory records or graph records:
//! they are primary data. It can rebuild only the structures whose source of
//! truth is already in the store: reverse mappings/allocator, FTS, and the
//! DiskANN graph around still-present vectors. Missing vectors are reported
//! for the consumer to re-embed; the engine has no model by design.

use std::collections::BTreeSet;

use crate::error::{EngineError, Result};
use crate::idx::fts::PersistentFts;
use crate::idx::memory::{MemoryRecord, VecMapEntry, meta as memory_meta, record, vecmap};
use crate::idx::vector::{PersistentVectorIndex, VectorIndexParams, meta as vector_meta, node};
use crate::key::{fts_index, memory_index, vector_index};

use super::{Batch, Engine, IntegrityIssue, IssueKind, VerifyReport};

/// One operation a repair plan proposes. The plan is data only: producing it
/// has no write path and is therefore suitable for `repair --dry-run`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairAction {
    RebuildMemoryMappings,
    RebuildFts,
    RebuildVectorGraph,
    ReembedMissingVectors,
}

/// Deterministic, read-only interpretation of a completed logical audit.
#[derive(Debug, Default)]
pub struct RepairPlan {
    pub actions: Vec<RepairAction>,
    /// Problems for which this engine must not promise an automatic repair.
    pub primary_data_at_risk: Vec<IntegrityIssue>,
    pub warnings: Vec<IntegrityIssue>,
}

impl RepairPlan {
    #[must_use]
    pub fn can_apply_derived_only(&self) -> bool {
        self.primary_data_at_risk.is_empty()
    }
}

/// Builds the `repair --dry-run` plan from a [`VerifyReport`] without opening
/// or mutating the store. Unknown and physical anomalies stay conservative:
/// they are primary-data risks until a later repair phase can prove otherwise.
#[must_use]
pub fn plan_repair(report: &VerifyReport) -> RepairPlan {
    let mut plan = RepairPlan::default();
    for issue in &report.warnings {
        plan.warnings.push(issue.clone());
        match issue.kind {
            IssueKind::VectorMetaInconsistent => add_action(&mut plan, RepairAction::RebuildVectorGraph),
            IssueKind::FtsStatsInconsistent => add_action(&mut plan, RepairAction::RebuildFts),
            _ => {}
        }
    }
    for issue in &report.errors {
        match issue.kind {
            IssueKind::AllocatorStale => add_action(&mut plan, RepairAction::RebuildMemoryMappings),
            IssueKind::FtsLinkBroken | IssueKind::FtsStatsInconsistent => {
                add_action(&mut plan, RepairAction::RebuildFts)
            }
            IssueKind::VectorNeighborMissing | IssueKind::VectorDimMismatch | IssueKind::VectorMetaInconsistent => {
                add_action(&mut plan, RepairAction::RebuildVectorGraph);
            }
            IssueKind::MemoryLinkBroken => {
                add_action(&mut plan, RepairAction::RebuildMemoryMappings);
                add_action(&mut plan, RepairAction::ReembedMissingVectors);
            }
            IssueKind::IdxValueCorrupt if issue.detail.contains("fts ") => {
                add_action(&mut plan, RepairAction::RebuildFts)
            }
            IssueKind::IdxValueCorrupt if issue.detail.contains("vector index metadata") => {
                add_action(&mut plan, RepairAction::RebuildVectorGraph);
            }
            _ => plan.primary_data_at_risk.push(issue.clone()),
        }
    }
    plan
}

fn add_action(plan: &mut RepairPlan, action: RepairAction) {
    if !plan.actions.contains(&action) {
        plan.actions.push(action);
    }
}

/// Result of applying the always-safe part of `rebuild-indexes`.
#[derive(Debug, Default)]
pub struct RebuildReport {
    pub memory_mappings_rebuilt: u64,
    pub fts_documents_rebuilt: u64,
    pub vector_graph_rebuilt: bool,
    /// Memory records whose `vec_id` has no surviving vector block. They are
    /// intentionally retained; the consumer must supply embeddings again.
    pub reembedding_required: Vec<(String, String)>,
}

/// Rebuilds only derived structures from primary memory records and surviving
/// vector payloads. It is resumable: a crash can leave a derived index empty
/// or partial, never modifies a primary record, and a later call completes it.
pub fn rebuild_indexes(engine: &mut Engine) -> Result<RebuildReport> {
    let records = load_records(engine)?;
    let mut report = RebuildReport::default();

    let live_nodes = live_node_ids(engine)?;
    for (agent, id, stored) in &records {
        if !live_nodes.contains(&stored.vec_id) {
            report.reembedding_required.push((agent.clone(), id.clone()));
        }
    }

    rebuild_memory_mappings(engine, &records)?;
    report.memory_mappings_rebuilt = records.len() as u64;

    rebuild_fts(engine, &records)?;
    report.fts_documents_rebuilt = records
        .iter()
        .filter(|(_, _, record)| !record.content.is_empty())
        .count() as u64;

    if let Some(params) = vector_params(engine)? {
        let mut vectors = PersistentVectorIndex::open(engine, params)?;
        vectors.rebuild(engine)?;
        report.vector_graph_rebuilt = true;
    }
    Ok(report)
}

fn load_records(engine: &Engine) -> Result<Vec<(String, String, MemoryRecord)>> {
    let mut out = Vec::new();
    for (key, value) in engine.scan_prefix(memory_index::RECORD_PREFIX)? {
        let (agent, id) = parse_record_key(key.as_bytes()).ok_or_else(|| EngineError::CorruptMemoryRecord {
            reason: format!(
                "malformed record key in the idx/memory/rec/ keyspace: {:?}",
                key.as_bytes()
            ),
        })?;
        out.push((agent, id, record::decode(&value)?));
    }
    Ok(out)
}

fn parse_record_key(key: &[u8]) -> Option<(String, String)> {
    let suffix = key.strip_prefix(memory_index::RECORD_PREFIX)?;
    let raw: [u8; 4] = suffix.get(..4)?.try_into().ok()?;
    let len = u32::from_be_bytes(raw) as usize;
    let rest = suffix.get(4..)?;
    let agent = String::from_utf8(rest.get(..len)?.to_vec()).ok()?;
    let id = String::from_utf8(rest.get(len..)?.to_vec()).ok()?;
    Some((agent, id))
}

fn rebuild_memory_mappings(engine: &mut Engine, records: &[(String, String, MemoryRecord)]) -> Result<()> {
    let mut ids = BTreeSet::new();
    let mut batch = Batch::new();
    for (key, _) in engine.scan_prefix(memory_index::VECMAP_PREFIX)? {
        batch.delete(key.as_bytes());
    }
    batch.delete(memory_index::META_KEY);
    for (agent, id, stored) in records {
        if !ids.insert(stored.vec_id) {
            return Err(EngineError::CorruptMemoryRecord {
                reason: format!("multiple primary memory records claim vec_id {}", stored.vec_id),
            });
        }
        let mapping = VecMapEntry {
            agent: agent.clone(),
            id: id.clone(),
        };
        batch.put(
            memory_index::vecmap_key(stored.vec_id).as_bytes(),
            &vecmap::encode(&mapping)?,
        );
    }
    let next_vec_id = ids.last().map_or(0, |id| id.saturating_add(1));
    batch.put(
        memory_index::META_KEY,
        &memory_meta::encode(&memory_meta::MemoryIndexMeta { next_vec_id })?,
    );
    engine.apply_batch(&batch)
}

fn rebuild_fts(engine: &mut Engine, records: &[(String, String, MemoryRecord)]) -> Result<()> {
    let mut purge = Batch::new();
    for (key, _) in engine.scan_prefix(fts_index::INDEX_PREFIX)? {
        purge.delete(key.as_bytes());
    }
    if !purge.is_empty() {
        engine.apply_batch(&purge)?;
    }
    let fts = PersistentFts::new();
    for (agent, _, stored) in records {
        let mut batch = Batch::new();
        fts.stage_insert(engine, agent, stored.vec_id, &stored.content, &mut batch)?;
        if !batch.is_empty() {
            engine.apply_batch(&batch)?;
        }
    }
    Ok(())
}

fn live_node_ids(engine: &Engine) -> Result<BTreeSet<u64>> {
    let mut ids = BTreeSet::new();
    for (key, value) in engine.scan_prefix(vector_index::NODE_PREFIX)? {
        let Some(id) = vector_index::node_id(key.as_bytes()) else {
            return Err(EngineError::CorruptVectorNode {
                reason: "malformed vector node key during rebuild".to_string(),
            });
        };
        if !node::decode(&value)?.deleted {
            ids.insert(id);
        }
    }
    Ok(ids)
}

fn vector_params(engine: &Engine) -> Result<Option<VectorIndexParams>> {
    if let Some(bytes) = engine.get(vector_index::META_KEY)?
        && let Ok(meta) = vector_meta::decode(&bytes)
    {
        return Ok(Some(meta.params));
    }
    let Some((_, value)) = engine.scan_prefix(vector_index::NODE_PREFIX)?.into_iter().next() else {
        return Ok(None);
    };
    Ok(Some(VectorIndexParams::with_dim(node::decode(&value)?.vector.len())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::idx::memory::{NewMemoryRecord, PersistentMemoryIndex};
    use crate::idx::vector::VectorIndexParams;
    use crate::store::{VerifyMode, verify_store};
    use tempfile::tempdir;

    fn new_record(content: &str) -> NewMemoryRecord<'_> {
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

    #[test]
    fn dry_plan_is_read_only_and_proposes_derived_repairs() {
        let dir = tempdir().expect("tempdir");
        let mut engine = Engine::open(dir.path()).expect("open");
        let mut vectors = PersistentVectorIndex::open(&mut engine, VectorIndexParams::with_dim(2)).expect("vectors");
        let mut memory = PersistentMemoryIndex::open(&engine).expect("memory");
        memory
            .put(
                &mut engine,
                &mut vectors,
                &PersistentFts::new(),
                "a",
                "one",
                &new_record("hello world"),
                vec![1.0, 0.0],
            )
            .expect("put");
        engine
            .delete(fts_index::meta_key("a").expect("key").as_bytes())
            .expect("delete stats");
        let before = std::fs::read_dir(dir.path()).expect("read dir").count();
        let audit = verify_store(dir.path(), None, VerifyMode::FullLogical).expect("verify");
        let plan = plan_repair(&audit);
        assert!(plan.actions.contains(&RepairAction::RebuildFts));
        assert_eq!(std::fs::read_dir(dir.path()).expect("read dir").count(), before);
    }

    #[test]
    fn rebuild_keeps_primary_records_and_restores_logical_health() {
        let dir = tempdir().expect("tempdir");
        let mut engine = Engine::open(dir.path()).expect("open");
        let mut vectors = PersistentVectorIndex::open(&mut engine, VectorIndexParams::with_dim(2)).expect("vectors");
        let mut memory = PersistentMemoryIndex::open(&engine).expect("memory");
        memory
            .put(
                &mut engine,
                &mut vectors,
                &PersistentFts::new(),
                "a",
                "one",
                &new_record("hello world"),
                vec![1.0, 0.0],
            )
            .expect("put");
        let primary_before = engine.scan_prefix(memory_index::RECORD_PREFIX).expect("records");
        engine
            .delete(memory_index::vecmap_key(0).as_bytes())
            .expect("delete map");
        engine
            .delete(fts_index::meta_key("a").expect("key").as_bytes())
            .expect("delete stats");
        let rebuilt = rebuild_indexes(&mut engine).expect("rebuild");
        assert_eq!(rebuilt.memory_mappings_rebuilt, 1);
        assert!(rebuilt.vector_graph_rebuilt);
        assert!(rebuilt.reembedding_required.is_empty());
        assert_eq!(
            engine.scan_prefix(memory_index::RECORD_PREFIX).expect("records"),
            primary_before
        );
        let audit = verify_store(dir.path(), None, VerifyMode::FullLogical).expect("verify");
        assert!(audit.healthy, "{:#?}", audit.errors);
    }
}
