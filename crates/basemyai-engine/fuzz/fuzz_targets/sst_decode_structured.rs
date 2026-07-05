//! Fuzz target: `format::sst::decode` with a *valid* trailing crc32.
//!
//! `sst_decode.rs` throws raw arbitrary bytes at `decode` — useful, but
//! `decode` checks the whole-buffer crc32 before it parses `entry_count` or
//! any entry, so purely random mutation essentially never gets past that
//! gate (need a 1-in-2^32 coincidence). That undersells the actual attack
//! surface: crc32 is *not* cryptographic, so a real attacker crafting a
//! malicious `*.sst` file computes the matching checksum trivially — the
//! checksum only defends against accidental bit-rot, not deliberate
//! corruption. This target builds a header (magic/version/entry_count) plus
//! an arbitrary body, appends the *correct* crc32 for that exact buffer (the
//! same algorithm `format::checksum::crc32` uses), and fuzzes `decode` on
//! that — so the fuzzer can freely explore malformed `entry_count`/entry
//! bodies without needing to also guess a hash collision.
//!
//! This is exactly how the manual-review finding below was confirmed
//! (reachable without a fuzzer, but this target lets libFuzzer keep
//! searching for siblings of it):
//!
//! `entry_count` (an 8-byte, fully attacker-controlled `u64` in the file
//! header) is passed straight to `Vec::with_capacity(entry_count as usize)`
//! (src/format/sst.rs, in `decode`) *before* any check against the buffer's
//! actual remaining length. A crafted 18-byte file — magic + version +
//! `entry_count = u64::MAX` + a correct trailing crc32 — panics with
//! "capacity overflow" instead of returning `EngineError::CorruptSst`. Any
//! caller that loads an untrusted or adversarially-corrupted `.sst` file
//! (`store::sst::SstFile::load`) can be crashed this way.

#![no_main]

use std::path::Path;

use arbitrary::Arbitrary;
use basemyai_engine::format::sst::{self, SST_FORMAT_VERSION, SST_MAGIC};
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
struct RawSst {
    /// Usually the real version, occasionally something else — exercises
    /// `UnsupportedFormatVersion` alongside the entry-parsing paths.
    version_is_real: bool,
    version_if_fake: u16,
    /// The header's `entry_count` field: fully attacker-controlled,
    /// independent of how many bytes `body` actually contains.
    entry_count: u64,
    /// Arbitrary bytes standing in for the entries region.
    body: Vec<u8>,
}

fuzz_target!(|input: RawSst| {
    let mut buf = Vec::new();
    buf.extend_from_slice(&SST_MAGIC.to_le_bytes());
    let version = if input.version_is_real {
        SST_FORMAT_VERSION
    } else {
        input.version_if_fake
    };
    buf.extend_from_slice(&version.to_le_bytes());
    buf.extend_from_slice(&input.entry_count.to_le_bytes());
    buf.extend_from_slice(&input.body);
    let crc = crc32(&buf);
    buf.extend_from_slice(&crc.to_le_bytes());

    let path = Path::new("fuzz.sst");
    let _ = sst::decode(&buf, path);
});
