//! Fuzz target: `format::sst_block::decode_sst_data_block` with a *valid*
//! trailing crc32.
//!
//! `sst_data_block_decode.rs` throws raw arbitrary bytes at the decoder —
//! useful, but the whole-buffer crc32 gate makes pure random mutation an
//! unlikely way to reach the `entry_count`/per-entry parsing logic. This
//! target builds a header (magic + version + `entry_count`) plus an
//! arbitrary body, appends the *correct* crc32 for that exact buffer, and
//! fuzzes `decode_sst_data_block` on that — so the fuzzer can freely
//! explore malformed `entry_count`/entry bodies without needing to also
//! guess a hash collision. Same rationale, same bug class this exists to
//! catch, as `sst_decode_structured` (which found a real
//! `Vec::with_capacity` overflow panic in the whole-file SST decoder before
//! `entry_count` was bounded against the buffer).
#![no_main]

use std::path::Path;

use arbitrary::Arbitrary;
use basemyai_engine::format::sst_block::{self, SST_DATA_BLOCK_MAGIC, SST_DATA_BLOCK_VERSION};
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
struct RawBlock {
    /// Usually the real version, occasionally something else — exercises
    /// `UnsupportedSstDataBlockVersion` alongside the entry-parsing paths.
    version_is_real: bool,
    version_if_fake: u16,
    /// The block header's `entry_count` field: fully attacker-controlled,
    /// independent of how many bytes `body` actually contains.
    entry_count: u32,
    /// Arbitrary bytes standing in for the entries region.
    body: Vec<u8>,
}

fuzz_target!(|input: RawBlock| {
    let mut buf = Vec::new();
    buf.extend_from_slice(&SST_DATA_BLOCK_MAGIC.to_le_bytes());
    let version = if input.version_is_real {
        SST_DATA_BLOCK_VERSION
    } else {
        input.version_if_fake
    };
    buf.extend_from_slice(&version.to_le_bytes());
    buf.extend_from_slice(&input.entry_count.to_le_bytes());
    buf.extend_from_slice(&input.body);
    let crc = crc32(&buf);
    buf.extend_from_slice(&crc.to_le_bytes());

    let _ = sst_block::decode_sst_data_block(&buf, Path::new("fuzz.sst"));
});
