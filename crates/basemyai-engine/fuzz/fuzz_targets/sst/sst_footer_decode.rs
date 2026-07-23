//! Fuzz target: `format::sst_block::decode_sst_footer` on arbitrary bytes.
//!
//! `SstFooter` (ADR-039, N8.2) is fixed-length (`SST_FOOTER_LEN`) with an
//! exact-length check before any field read — same shape as
//! `vector_meta_decode`. This target guards the length/leading-magic/crc/
//! trailing-`footer_magic`/version gates stay panic-free, and that every
//! rejection returns `CorruptSstFooter`/`UnsupportedSstFooterVersion`,
//! never a panic.
#![no_main]

use std::path::Path;

use basemyai_engine::format::sst_block;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = sst_block::decode_sst_footer(data, Path::new("fuzz.sst"));
});
