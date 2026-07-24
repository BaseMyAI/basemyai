//! Fuzz target: `format::generation_meta::decode` on arbitrary bytes.
//!
//! FMT-FUZZ-GAP (BaseMyAI adversarial audit, 2026-07-22): `GenerationMeta`
//! (ADR-042 §3, N12) was the other length-prefixed wire format with no
//! dedicated fuzz target. It has a fixed `TOTAL_LEN` with no variable-length
//! fields at all, so it has no analogous count-driven allocation surface —
//! this target still exists to keep the fixed-length/magic/crc gates
//! panic-free under continuous fuzzing.
#![no_main]

use std::path::Path;

use basemyai_engine::format::generation_meta;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = generation_meta::decode(data, Path::new("generation.meta"));
});
