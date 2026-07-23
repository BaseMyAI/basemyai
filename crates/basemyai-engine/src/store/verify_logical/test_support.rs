// SPDX-License-Identifier: BUSL-1.1
//! Shared test fixtures for `verify_logical`'s split submodules — a single
//! composed store exercising memory/vector/FTS/graph together, and the
//! `full_logical`/`has_error`/`has_warning` helpers every submodule's own
//! tests build on.

use std::path::Path;

use crate::idx::fts::PersistentFts;
use crate::idx::graph::{GraphEdgeMeta, GraphEntity, PersistentGraph};
use crate::idx::memory::{NewMemoryRecord, PersistentMemoryIndex};
use crate::idx::vector::{PersistentVectorIndex, VectorIndexParams};
use crate::store::verify::{IssueKind, VerifyMode, VerifyReport, verify_store};
use crate::store::{Engine, EngineOptions};

pub(super) const DIM: usize = 4;

pub(super) fn small_options() -> EngineOptions {
    EngineOptions {
        memtable_flush_threshold: 1000,
        compaction_sst_threshold: 100,
        block_size: 512,
        ..EngineOptions::default()
    }
}

pub(super) fn new_record(content: &'static str) -> NewMemoryRecord<'static> {
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
pub(super) fn build_composed_store() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut engine = Engine::open_with_options(dir.path(), small_options()).expect("open");
    let mut vectors = PersistentVectorIndex::open(&mut engine, VectorIndexParams::with_dim(DIM)).expect("open vectors");
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

pub(super) fn full_logical(dir: &Path) -> VerifyReport {
    verify_store(dir, None, VerifyMode::FullLogical).expect("verify")
}

pub(super) fn has_error(report: &VerifyReport, kind: IssueKind) -> bool {
    report.errors.iter().any(|e| e.kind == kind)
}

pub(super) fn has_warning(report: &VerifyReport, kind: IssueKind) -> bool {
    report.warnings.iter().any(|w| w.kind == kind)
}
