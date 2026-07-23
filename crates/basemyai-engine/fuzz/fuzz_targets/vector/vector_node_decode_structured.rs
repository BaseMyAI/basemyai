//! Fuzz target: `idx::vector::node::decode` with a *valid* trailing crc32
//! (v2 block layout — flags/tombstone byte, N3 deletes step).
//!
//! `vector_node_decode.rs` throws raw arbitrary bytes at `decode` — useful,
//! but the whole-buffer crc32 is checked before any structural field is
//! parsed, so purely random mutation essentially never gets past that gate.
//! Like `sst_decode_structured`, this target builds a header (magic,
//! version, flags, dim, neighbor_count) plus an arbitrary body, appends the
//! *correct* crc32 for that exact buffer, and fuzzes `decode` on that — so
//! the fuzzer freely explores the post-checksum surface: the `flags`
//! reserved-bits rejection (new in v2), lying `dim`/`neighbor_count` vs the
//! exact-length equation, and version gating. `decode` must never panic and
//! must only ever return `Ok(node)` or a clean
//! `CorruptVectorNode`/`UnsupportedVectorNodeVersion`.
//!
//! Same execution constraint as every target here (see fuzz/README.md,
//! N2): libFuzzer does not link on native Windows — run under WSL.

#![no_main]

use arbitrary::Arbitrary;
use basemyai_engine::idx::vector::node::{self, VECTOR_NODE_MAGIC, VECTOR_NODE_VERSION};
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
struct RawNode {
    /// Usually the real version, occasionally something else — exercises
    /// `UnsupportedVectorNodeVersion` alongside the structural paths.
    version_is_real: bool,
    version_if_fake: u16,
    /// Fully attacker-controlled flags byte — bit 0 is the tombstone,
    /// bits 1-7 are reserved and must be rejected by the decoder.
    flags: u8,
    /// The header's `dim` and `neighbor_count` fields: fully controlled,
    /// independent of how many bytes `body` actually contains.
    dim: u16,
    neighbor_count: u16,
    /// Arbitrary bytes standing in for the vector + neighbors region.
    body: Vec<u8>,
}

fuzz_target!(|input: RawNode| {
    let mut buf = Vec::new();
    buf.extend_from_slice(&VECTOR_NODE_MAGIC.to_le_bytes());
    let version = if input.version_is_real {
        VECTOR_NODE_VERSION
    } else {
        input.version_if_fake
    };
    buf.extend_from_slice(&version.to_le_bytes());
    buf.push(input.flags);
    buf.extend_from_slice(&input.dim.to_le_bytes());
    buf.extend_from_slice(&input.neighbor_count.to_le_bytes());
    buf.extend_from_slice(&input.body);
    let crc = crc32(&buf);
    buf.extend_from_slice(&crc.to_le_bytes());

    let _ = node::decode(&buf);
});
