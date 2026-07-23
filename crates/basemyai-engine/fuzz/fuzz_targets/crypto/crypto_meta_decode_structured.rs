//! Fuzz target: `crypto.meta` decoder with a valid trailing CRC-32.
//!
//! The raw `crypto_meta_decode` target is still useful for the outer
//! checksum gate. This companion builds a syntactically positioned header
//! with the correct CRC, then makes the version, generation, wrapped length,
//! KDF tag and KDF body independently hostile. It consequently exercises CryptoMeta:1
//! compatibility and all CryptoMeta:2 post-checksum rejection paths (unknown
//! KDF, malformed Argon2id fields, and length confusion).
#![no_main]

use std::path::Path;

use arbitrary::Arbitrary;
use basemyai_engine::format::crypto;
use libfuzzer_sys::fuzz_target;

const CRYPTO_META_MAGIC: u32 = 0x424B_4559;
const CRYPTO_META_V1_VERSION: u16 = 1;
const CRYPTO_META_V2_VERSION: u16 = 2;

// Same IEEE 802.3 CRC-32 as the private format helper. This harness is a
// separate crate, so it cannot reach `format::checksum::crc32` directly.
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
struct RawCryptoMeta {
    version_kind: u8,
    other_version: u16,
    generation_id: u64,
    salt: [u8; 16],
    nonce: [u8; 24],
    wrapped_len: u32,
    body: Vec<u8>,
}

fuzz_target!(|input: RawCryptoMeta| {
    let version = match input.version_kind % 3 {
        0 => CRYPTO_META_V1_VERSION,
        1 => CRYPTO_META_V2_VERSION,
        _ => input.other_version,
    };
    let mut bytes = Vec::with_capacity(4 + 2 + 8 + 16 + 24 + 4 + input.body.len() + 4);
    bytes.extend_from_slice(&CRYPTO_META_MAGIC.to_le_bytes());
    bytes.extend_from_slice(&version.to_le_bytes());
    if version == CRYPTO_META_V2_VERSION {
        bytes.extend_from_slice(&input.generation_id.to_le_bytes());
    }
    bytes.extend_from_slice(&input.salt);
    bytes.extend_from_slice(&input.nonce);
    bytes.extend_from_slice(&input.wrapped_len.to_le_bytes());
    bytes.extend_from_slice(&input.body);
    bytes.extend_from_slice(&crc32(&bytes).to_le_bytes());

    crypto::fuzz_decode_crypto_meta(&bytes, Path::new("fuzz-crypto.meta"));
});
