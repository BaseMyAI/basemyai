//! Fuzz target: `idx::vector::node::decode` on arbitrary/malformed byte
//! streams.
//!
//! Node blocks are KV values (ADR-026 — one graph node = one KV record), so
//! once persistence lands this decoder will run on whatever bytes come back
//! from the store: a corrupted disk, a buggy compaction, or by definition
//! anything a fuzzer throws at it. It must never panic and must only ever
//! return `Ok(node)` or a clean
//! `EngineError::CorruptVectorNode`/`UnsupportedVectorNodeVersion`.
//!
//! Same rationale as `sst_decode` (the N2 fuzzing lesson): `dim` and
//! `neighbor_count` are wire-controlled count fields and are bounded against
//! the actual buffer length before any allocation — this target guards that
//! property against regression. Note the crc32 gate sits before the
//! structural checks, so like `sst_decode` most random mutations die there;
//! `vector_node_decode_structured` recomputes the crc32 (like
//! `sst_decode_structured`) so the fuzzer can reach past that gate — the v2
//! `flags` byte (tombstone + reserved bits) lives behind it.
#![no_main]

use basemyai_engine::idx::vector::node;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = node::decode(data);
});
