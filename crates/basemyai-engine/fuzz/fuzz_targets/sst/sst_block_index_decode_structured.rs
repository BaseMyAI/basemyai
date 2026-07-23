//! Fuzz target: `format::sst_block::decode_sst_block_index` with a *valid*
//! trailing crc32.
//!
//! Same pattern and rationale as `sst_data_block_decode_structured`: builds
//! a header (magic + version + `block_count`) plus an arbitrary body,
//! appends the *correct* crc32 for that exact buffer, so the fuzzer can
//! freely explore malformed `block_count`/per-entry
//! `first_key_len`/`last_key_len`/`offset`/`len`/`entry_count` parsing
//! without needing to also guess a hash collision.
#![no_main]

use std::path::Path;

use arbitrary::Arbitrary;
use basemyai_engine::format::sst_block::{self, SST_BLOCK_INDEX_MAGIC, SST_BLOCK_INDEX_VERSION};
use libfuzzer_sys::fuzz_target;

// Same algorithm as `basemyai_engine::format::checksum::crc32` (CRC-32,
// IEEE 802.3, reflected) — that helper is `pub(crate)`-only, so this fuzz
// harness (a separate crate) reimplements it rather than reaching in.
fn crc32(bytes: &[u8]) -> u32 {
    const POLY: u32 = 0xEDB8_8320;
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in bytes {
        let mut c = crc ^ u32::from(byte);
        for _ in 0..8 {
            c = if c & 1 != 0 { POLY ^ (c >> 1) } else { c >> 1 };
        }
        crc = c;
    }
    !crc
}

#[derive(Debug, Arbitrary)]
struct RawIndex {
    /// Usually the real version, occasionally something else — exercises
    /// `UnsupportedSstBlockIndexVersion` alongside the entry-parsing paths.
    version_is_real: bool,
    version_if_fake: u16,
    /// The header's `block_count` field: fully attacker-controlled,
    /// independent of how many bytes `body` actually contains.
    block_count: u32,
    /// Arbitrary bytes standing in for the entries region.
    body: Vec<u8>,
}

fuzz_target!(|input: RawIndex| {
    let mut buf = Vec::new();
    buf.extend_from_slice(&SST_BLOCK_INDEX_MAGIC.to_le_bytes());
    let version = if input.version_is_real {
        SST_BLOCK_INDEX_VERSION
    } else {
        input.version_if_fake
    };
    buf.extend_from_slice(&version.to_le_bytes());
    buf.extend_from_slice(&input.block_count.to_le_bytes());
    buf.extend_from_slice(&input.body);
    let crc = crc32(&buf);
    buf.extend_from_slice(&crc.to_le_bytes());

    let _ = sst_block::decode_sst_block_index(&buf, Path::new("fuzz.sst"));
});
