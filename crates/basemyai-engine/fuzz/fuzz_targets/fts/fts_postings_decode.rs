//! Fuzz target: `idx::fts::postings::decode` on arbitrary/malformed byte
//! streams.
//!
//! `FtsPosting` is fixed-length (magic/version/`tf`/crc32) with an
//! exact-length check before any field read — same shape as
//! `vector_meta_decode`/`memory_index_meta_decode`, so there are no
//! wire-controlled count fields to bound here. This target guards that the
//! length/crc/magic/version gates stay panic-free as the format evolves.
#![no_main]

use basemyai_engine::idx::fts::postings;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = postings::decode(data);
});
