//! Fuzz target: `format::sst_block::decode_sst_bloom_filter` on arbitrary
//! bytes.
//!
//! `SstBloomFilter` (ADR-039 §6, N8.2) has one wire-controlled length field
//! (`bits_len`) that must be bounded against the buffer *and*
//! cross-checked against `ceil(num_bits / 8)` before it drives a slice —
//! same class of bug the `sst_decode`/`crypto_meta` targets exist to catch
//! (a lying length field feeding an allocation or slice before validation).
//! The whole-buffer crc32 gate makes pure random mutation an unlikely way
//! to reach that logic — see `sst_data_block_decode_structured` for the
//! crc-bypassing sibling pattern; this raw target is still useful for the
//! length/magic/version gates ahead of the crc check.
#![no_main]

use std::path::Path;

use basemyai_engine::format::sst_block;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = sst_block::decode_sst_bloom_filter(data, Path::new("fuzz.sst"));
});
