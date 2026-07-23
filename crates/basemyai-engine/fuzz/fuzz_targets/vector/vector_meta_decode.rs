//! Fuzz target: `idx::vector::meta::decode` on arbitrary/malformed byte
//! streams.
//!
//! The index metadata record is a KV value (`key::vector_index::meta_key()`,
//! ADR-026) that `PersistentVectorIndex::open` decodes on every open of a
//! store containing a vector index — and treats a decode *error* as the
//! trigger for the rebuild escape hatch. That makes the failure contract
//! doubly load-bearing: `decode` must never panic, and must only ever return
//! `Ok(meta)` or a clean
//! `EngineError::CorruptVectorIndexMeta`/`UnsupportedVectorIndexMetaVersion`
//! (which `open` maps to a rebuild, never a crash).
//!
//! The record is fixed-length with an exact-length check before any field
//! read, so unlike `sst_decode` there are no wire-controlled count fields to
//! bound — this target mostly guards that the length/crc/magic/version
//! gates stay panic-free as the format evolves.
#![no_main]

use basemyai_engine::idx::vector::meta;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = meta::decode(data);
});
