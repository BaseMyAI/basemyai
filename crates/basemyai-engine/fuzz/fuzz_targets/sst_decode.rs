//! Fuzz target: `format::sst::decode` on arbitrary/malformed byte streams.
//!
//! This is the SST load path's decoder (`store::sst::SstFile::load` calls it
//! on whatever bytes are on disk for a `*.sst` file, which could be
//! corrupted by a bad disk, a partial/interrupted write that dodged the
//! crash-safe rename, or by definition anything a fuzzer throws at it). It
//! must never panic and must only ever return `Ok(entries)` or a clean
//! `EngineError::CorruptSst`/`UnsupportedFormatVersion`.
//!
//! This target exists specifically because manual review turned up a real
//! panic-safety gap here: `entry_count` is an attacker/corruption-controlled
//! `u64` read straight from the file header and passed to
//! `Vec::with_capacity(entry_count as usize)` *before* any bound-check
//! against the actual buffer length (src/format/sst.rs, `decode`). A crafted
//! 19-byte file with `entry_count = u64::MAX` panics with "capacity
//! overflow" instead of returning `EngineError::CorruptSst`. Once that's
//! fixed, this target should stop finding that particular crash — keep it
//! running to catch a regression.
#![no_main]

use std::path::Path;

use basemyai_engine::format::sst;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let path = Path::new("fuzz.sst");
    let _ = sst::decode(data, path);
});
