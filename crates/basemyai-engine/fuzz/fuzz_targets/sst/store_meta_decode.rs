//! Fuzz target: `format::store_meta::decode` on arbitrary bytes.
//!
//! `StoreMeta` (ADR-039 §7, N8.2; extended by ADR-042) accepts only its
//! exact legacy-v1 or current-v2 length before any field read. Note
//! `decode` deliberately does not reject an unexpected
//! `store_format_version` (that policy belongs to the store-open path,
//! N8.9) — this target only guards that the length/magic/crc gates stay
//! panic-free.
#![no_main]

use std::path::Path;

use basemyai_engine::format::store_meta;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = store_meta::decode(data, Path::new("store.meta"));
});
