//! Fuzz target: `format::sst_block::decode_sst_data_block` on arbitrary
//! bytes.
//!
//! Raw arbitrary bytes into the decoder — useful for the length/magic/
//! version gates, but the whole-buffer crc32 is checked before
//! `entry_count` or any entry is parsed, so pure random mutation essentially
//! never gets past that gate (needs a 1-in-2^32 coincidence). See
//! `sst_data_block_decode_structured` for the crc-bypassing sibling that
//! actually exercises the per-entry length-bounding logic — this is the
//! block-based-SST-format sibling of `sst_decode`/`sst_decode_structured`.
#![no_main]

use std::path::Path;

use basemyai_engine::format::sst_block;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = sst_block::decode_sst_data_block(data, Path::new("fuzz.sst"));
});
