// SPDX-License-Identifier: BUSL-1.1
//! Posting-record layout (N5.2, ADR-028 §3).
//!
//! `format.lock` anchor: `FtsPosting:1` — bump [`FTS_POSTING_VERSION`] and
//! this doc comment together whenever the byte layout below changes.
//!
//! One posting = one KV value under
//! `key::fts_index::postings_key(agent, term, vec_id)` — `agent`, `term`
//! and `vec_id` all live in the *key* (structural isolation + the term
//! this posting belongs to), so the value only carries the one thing the
//! key can't: how many times `term` occurs in that document.
//!
//! Record layout (all integers little-endian):
//!
//! ```text
//! magic:    u32  = FTS_POSTING_MAGIC
//! version:  u16  = FTS_POSTING_VERSION
//! tf:       u32   term frequency: occurrences of this term in this document
//! crc32:    u32   over every byte above
//! ```

use crate::error::{EngineError, Result};
use crate::format::FormatSpec;
use crate::format::checksum::crc32;

pub const FTS_POSTING_MAGIC: u32 = 0x4650_5354; // b"FPST" (LE bytes "TSPF")
pub const FTS_POSTING_VERSION: u16 = 1;

/// Canonical wire-format spec hashed into `format.lock` (see
/// [`crate::format::lock`]). Field list and order must mirror the byte
/// layout documented above exactly — update both together, never one
/// without the other.
pub fn spec() -> FormatSpec {
    FormatSpec {
        name: "FtsPosting",
        version: FTS_POSTING_VERSION,
        fields: &[("magic", "u32"), ("version", "u16"), ("tf", "u32"), ("crc32", "u32")],
    }
}

/// Total encoded size: fixed-size record (no variable-length payload).
const POSTING_LEN: usize = 4 + 2 + 4 + 4;
const CRC_LEN: usize = 4;

/// A decoded posting: the term frequency of one `(agent, term, vec_id)` —
/// all three of which live in the key that addresses this record, not here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FtsPosting {
    /// Occurrences of the posting's term in the posting's document.
    pub tf: u32,
}

/// Encodes one posting record. Infallible in practice (fixed-width field);
/// returns `Result` for signature symmetry with the sibling codecs.
pub fn encode(posting: &FtsPosting) -> Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(POSTING_LEN);
    buf.extend_from_slice(&FTS_POSTING_MAGIC.to_le_bytes());
    buf.extend_from_slice(&FTS_POSTING_VERSION.to_le_bytes());
    buf.extend_from_slice(&posting.tf.to_le_bytes());
    let crc = crc32(&buf);
    buf.extend_from_slice(&crc.to_le_bytes());
    Ok(buf)
}

/// Decodes a posting record previously produced by [`encode`].
///
/// Fixed-length record: an exact-length check runs before any field is
/// read (same discipline as `idx::memory::meta`).
pub fn decode(buf: &[u8]) -> Result<FtsPosting> {
    let corrupt = |reason: String| EngineError::CorruptFtsPosting { reason };

    if buf.len() != POSTING_LEN {
        return Err(corrupt(format!(
            "posting record must be exactly {POSTING_LEN} bytes, got {}",
            buf.len()
        )));
    }
    let crc_at = POSTING_LEN - CRC_LEN;
    let expected_crc = u32::from_le_bytes(buf[crc_at..].try_into().expect("slice is exactly 4 bytes"));
    let actual_crc = crc32(&buf[..crc_at]);
    if actual_crc != expected_crc {
        return Err(corrupt(format!(
            "checksum mismatch (expected {expected_crc:#x}, got {actual_crc:#x})"
        )));
    }

    let magic = u32::from_le_bytes(buf[0..4].try_into().expect("slice is exactly 4 bytes"));
    if magic != FTS_POSTING_MAGIC {
        return Err(corrupt("bad magic".to_string()));
    }
    let version = u16::from_le_bytes(buf[4..6].try_into().expect("slice is exactly 2 bytes"));
    if version != FTS_POSTING_VERSION {
        return Err(EngineError::UnsupportedFtsPostingVersion {
            expected: FTS_POSTING_VERSION,
            found: version,
        });
    }
    let tf = u32::from_le_bytes(buf[6..10].try_into().expect("slice is exactly 4 bytes"));

    Ok(FtsPosting { tf })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_a_posting() {
        let posting = FtsPosting { tf: 3 };
        let bytes = encode(&posting).expect("encode ok");
        assert_eq!(decode(&bytes).expect("decode ok"), posting);
    }

    #[test]
    fn truncated_buffer_is_rejected_at_every_cut() {
        let bytes = encode(&FtsPosting { tf: 7 }).expect("encode ok");
        for cut in 0..bytes.len() {
            let err = decode(&bytes[..cut]).expect_err("truncated record must not decode");
            assert!(
                matches!(err, EngineError::CorruptFtsPosting { .. }),
                "unexpected error at cut={cut}: {err}"
            );
        }
    }

    #[test]
    fn bit_flip_is_corrupt_error() {
        let mut bytes = encode(&FtsPosting { tf: 7 }).expect("encode ok");
        bytes[8] ^= 0xFF;
        let err = decode(&bytes).expect_err("checksum should fail");
        assert!(matches!(err, EngineError::CorruptFtsPosting { .. }));
    }

    #[test]
    fn unknown_version_is_unsupported_error() {
        let mut bytes = encode(&FtsPosting { tf: 7 }).expect("encode ok");
        bytes[4] = 0xFF;
        let crc_at = bytes.len() - CRC_LEN;
        let crc = crc32(&bytes[..crc_at]);
        bytes[crc_at..].copy_from_slice(&crc.to_le_bytes());
        let err = decode(&bytes).expect_err("unknown version must be rejected");
        assert!(matches!(
            err,
            EngineError::UnsupportedFtsPostingVersion { found: 0x00FF, .. }
        ));
    }

    #[test]
    fn oversized_buffer_is_rejected_too() {
        let mut bytes = encode(&FtsPosting { tf: 7 }).expect("encode ok");
        bytes.push(0);
        let err = decode(&bytes).expect_err("oversized record must not decode");
        assert!(matches!(err, EngineError::CorruptFtsPosting { .. }));
    }
}
