//! Fuzz target: `format::sst_block::decode_sst_block_index` on arbitrary
//! bytes.
//!
//! Raw arbitrary bytes into the decoder — useful for the length/magic/
//! version gates, but the whole-buffer crc32 gate (checked before
//! `block_count` or any entry is parsed) makes pure random mutation an
//! unlikely way to reach the per-entry length-bounding logic. See
//! `sst_block_index_decode_structured` for the crc-bypassing sibling.
#![no_main]

use std::path::Path;

use basemyai_engine::format::sst_block;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = sst_block::decode_sst_block_index(data, Path::new("fuzz.sst"));
});
