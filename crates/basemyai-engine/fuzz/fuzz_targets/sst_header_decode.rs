//! Fuzz target: `format::sst_block::decode_sst_header` on arbitrary bytes.
//!
//! `SstHeader` (ADR-039, N8.2) is fixed-length with an exact-length check
//! before any field read — same shape as `vector_meta_decode`, so there are
//! no wire-controlled count fields to bound here. This target guards that
//! the length/crc/magic/version/`block_size != 0` gates stay panic-free as
//! the format evolves, and that every rejection path returns
//! `CorruptSstHeader`/`UnsupportedSstHeaderVersion`, never a panic.
#![no_main]

use std::path::Path;

use basemyai_engine::format::sst_block;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = sst_block::decode_sst_header(data, Path::new("fuzz.sst"));
});
