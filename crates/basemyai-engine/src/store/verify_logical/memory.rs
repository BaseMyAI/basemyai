// SPDX-License-Identifier: BUSL-1.1
//! Record ↔ vecmap ↔ vector-node linkage (ADR-027) plus allocator
//! monotonicity — the memory-keyspace half of [`super::check_logical`].
//! Only run when the memory keyspace is populated; see the module doc on
//! [`super`] for the scope guard.

use std::collections::BTreeMap;
use std::path::Path;

use crate::store::verify::{IssueKind, VerifyReport};

use super::LogicalView;

pub(super) fn check_memory_links(view: &LogicalView, dir: &Path, report: &mut VerifyReport) {
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

#[cfg(test)]
mod tests {
    use super::super::test_support::{build_composed_store, full_logical, has_error, small_options};
    use crate::idx::memory::{PersistentMemoryIndex, meta as memory_meta};
    use crate::key::memory_index;
    use crate::store::Engine;
    use crate::store::verify::IssueKind;

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
}
