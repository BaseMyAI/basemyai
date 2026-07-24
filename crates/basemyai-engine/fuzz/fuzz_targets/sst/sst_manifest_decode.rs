//! Fuzz target: `format::sst_manifest::decode` on arbitrary bytes.
//!
//! FMT-FUZZ-GAP (BaseMyAI adversarial audit, 2026-07-22): `SstManifest`
//! (ADR-043 §1) was the one length-prefixed wire format among the engine's
//! wire formats with no dedicated fuzz target. `decode`'s
//! `HEADER_LEN + count * 8` arithmetic is gated by an exact `buf.len() ==
//! total_len` equality check *before* any per-id indexing, so an inflated
//! `live_sst_ids_count` cannot itself drive an oversized allocation
//! independent of the real (already fully buffered) input length — this
//! target exists to keep that property true under continuous fuzzing, not
//! because a live overflow was found.
#![no_main]

use std::path::Path;

use basemyai_engine::format::sst_manifest;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = sst_manifest::decode(data, Path::new("manifest.meta"));
});
