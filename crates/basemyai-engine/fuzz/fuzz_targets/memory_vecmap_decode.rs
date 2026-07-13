//! Fuzz target: `idx::memory::vecmap::decode` on arbitrary/malformed byte
//! streams — the `vec_id -> (agent, id)` reverse mapping (ADR-027 §2/§4).
//!
//! N2/N3 fuzzing lesson applied: `agent_len`/`id_len` are `u32`
//! wire-controlled count fields (widened to match the key encoders' own
//! length-prefix width, per the module doc), bounded against the buffer's
//! actual length before any string is materialized — a lying length must
//! yield `EngineError::CorruptMemoryVecMap`, never a panic or an oversized
//! allocation (same discipline as `memory_record_decode`/
//! `fts_docterms_decode`).
#![no_main]

use basemyai_engine::idx::memory::vecmap;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = vecmap::decode(data);
});
