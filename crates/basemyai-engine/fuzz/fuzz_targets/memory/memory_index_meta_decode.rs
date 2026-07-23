//! Fuzz target: `idx::memory::meta::decode` on arbitrary/malformed byte
//! streams — the persisted `next_vec_id` allocator (ADR-027 §4).
//!
//! Fixed-length record (magic/version/`next_vec_id`/crc32), same shape as
//! `fts_postings_decode`/`fts_stats_decode`. Worth its own target despite
//! the small surface: a decode bug here that *silently* under-reports would
//! be one of the worst possible outcomes in this crate (a stale allocator
//! risks reusing a live `vec_id`, ADR-027 §4) — this guards that any
//! adversarial input is cleanly `EngineError::CorruptMemoryIndexMeta`/
//! `UnsupportedMemoryIndexMetaVersion`, never a panic and never a
//! misleadingly-decoded value.
#![no_main]

use basemyai_engine::idx::memory::meta;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = meta::decode(data);
});
