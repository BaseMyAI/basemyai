//! Fuzz target: `idx::graph::entity::decode` on arbitrary/malformed byte
//! streams.
//!
//! Entity blocks are KV values (N4 — one graph entity = one KV record), so
//! this decoder runs on whatever bytes come back from the store: a
//! corrupted disk, a buggy compaction, or by definition anything a fuzzer
//! throws at it. It must never panic and must only ever return `Ok(entity)`
//! or a clean `EngineError::CorruptGraphEntity`/`UnsupportedGraphEntityVersion`.
//!
//! Same rationale as `vector_node_decode` (the N2 fuzzing lesson): `kind_len`
//! and `label_len` are wire-controlled count fields and are bounded against
//! the actual buffer length before any allocation — this target guards that
//! property against regression. The crc32 gate sits before the structural
//! checks, so like the other `*_decode` targets most random mutations die
//! there; a `*_decode_structured` sibling (recomputed crc32) would be needed
//! to fuzz past that gate the way `vector_node_decode_structured` does for
//! the tombstone `flags` byte — not added here since `GraphEntity` has no
//! comparable "meaning behind the crc gate" byte yet (just the
//! `has_valid_until` boolean, itself covered by a unit test).
#![no_main]

use basemyai_engine::idx::graph::entity;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = entity::decode(data);
});
