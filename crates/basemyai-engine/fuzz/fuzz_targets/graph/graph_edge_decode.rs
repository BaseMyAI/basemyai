//! Fuzz target: `idx::graph::edge::decode` on arbitrary/malformed byte
//! streams. Same rationale as `graph_entity_decode`/`vector_meta_decode`:
//! `GraphEdge` is a fixed-length record, so the exact-length check ahead of
//! any field read is what this target guards — it must never panic and must
//! only ever return `Ok(edge)` or a clean
//! `EngineError::CorruptGraphEdge`/`UnsupportedGraphEdgeVersion`.
#![no_main]

use basemyai_engine::idx::graph::edge;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = edge::decode(data);
});
