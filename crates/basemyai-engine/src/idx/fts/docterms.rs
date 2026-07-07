// SPDX-License-Identifier: BUSL-1.1
//! Doc-terms (forward index) block layout (N5.2, ADR-028 §3).
//!
//! `format.lock` anchor: `FtsDocTerms:1` — bump [`FTS_DOCTERMS_VERSION`] and
//! this doc comment together whenever the byte layout below changes.
//!
//! One block = one KV value under `key::fts_index::docterms_key(agent, vec_id)`
//! — every distinct term of one document, with its term frequency. This is
//! what makes `stage_delete` precise (it knows exactly which `postings`
//! entries to remove without scanning anything) and what document length
//! (`Σ tf`, used by BM25's length normalization) is derived from — no
//! dependency on `idx::memory` (ADR-028 §3: the FTS index is self-sufficient).
//!
//! Block layout (all integers little-endian):
//!
//! ```text
//! magic:        u32  = FTS_DOCTERMS_MAGIC
//! version:      u16  = FTS_DOCTERMS_VERSION
//! count:        u32   number of (term, tf) entries
//! entries[count], each:
//!   term_len:   u16
//!   tf:         u32
//!   term:       [u8; term_len]  UTF-8
//! crc32:        u32   over every byte above (magic..last entry)
//! ```

use crate::error::{EngineError, Result};
use crate::format::FormatSpec;
use crate::format::checksum::crc32;

pub const FTS_DOCTERMS_MAGIC: u32 = 0x4644_5443; // b"FDTC" (LE bytes "CTDF")
pub const FTS_DOCTERMS_VERSION: u16 = 1;

/// Canonical wire-format spec hashed into `format.lock` (see
/// [`crate::format::lock`]). Field list and order must mirror the byte
/// layout documented above exactly — update both together, never one
/// without the other. `entries[].*` fields describe one repeated entry, not
/// a fixed-count set of fields (same convention as `SstFile`).
pub fn spec() -> FormatSpec {
    FormatSpec {
        name: "FtsDocTerms",
        version: FTS_DOCTERMS_VERSION,
        fields: &[
            ("magic", "u32"),
            ("version", "u16"),
            ("count", "u32"),
            ("entries[].term_len", "u16"),
            ("entries[].tf", "u32"),
            ("entries[].term", "bytes(term_len)"),
            ("crc32", "u32"),
        ],
    }
}

const HEADER_LEN: usize = 4 + 2 + 4;
const CRC_LEN: usize = 4;
/// Smallest an entry could possibly be (empty term) — the bound used to
/// reject a lying `count` before it drives a `Vec::with_capacity` call
/// (same discipline as `format::sst`'s `entry_count`).
const MIN_ENTRY_LEN: usize = 2 + 4;

/// One `(term, tf)` pair of a decoded doc-terms block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocTerm {
    pub term: String,
    /// Occurrences of `term` in this document.
    pub tf: u32,
}

/// A decoded doc-terms block, owned. `agent`/`vec_id` are not part of this
/// type — they live in the key that addresses this block.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FtsDocTerms {
    pub terms: Vec<DocTerm>,
}

/// Encodes one doc-terms block.
///
/// Returns `Err(EngineError::CorruptFtsDocTerms)` if the entry count or any
/// `term` exceeds its wire field — a silent truncation would desync
/// `stage_delete`'s postings cleanup from what was actually indexed.
pub fn encode(doc: &FtsDocTerms) -> Result<Vec<u8>> {
    let corrupt = |reason: String| EngineError::CorruptFtsDocTerms { reason };
    let count = u32::try_from(doc.terms.len())
        .map_err(|_| corrupt(format!("entry count {} exceeds the u32 wire field", doc.terms.len())))?;

    let mut buf = Vec::with_capacity(HEADER_LEN + doc.terms.len() * MIN_ENTRY_LEN + CRC_LEN);
    buf.extend_from_slice(&FTS_DOCTERMS_MAGIC.to_le_bytes());
    buf.extend_from_slice(&FTS_DOCTERMS_VERSION.to_le_bytes());
    buf.extend_from_slice(&count.to_le_bytes());
    for entry in &doc.terms {
        let term_len = u16::try_from(entry.term.len())
            .map_err(|_| corrupt(format!("term length {} exceeds the u16 wire field", entry.term.len())))?;
        buf.extend_from_slice(&term_len.to_le_bytes());
        buf.extend_from_slice(&entry.tf.to_le_bytes());
        buf.extend_from_slice(entry.term.as_bytes());
    }
    let crc = crc32(&buf);
    buf.extend_from_slice(&crc.to_le_bytes());
    Ok(buf)
}

/// Decodes a doc-terms block previously produced by [`encode`].
///
/// N2/N3 fuzzing lesson applied: `count` is bounded against the smallest an
/// entry could possibly be (`MIN_ENTRY_LEN`) before it drives any
/// allocation, and every `term_len` is bounded against the buffer's actual
/// remaining length before a string is materialized — a lying count or
/// length yields `CorruptFtsDocTerms`, never a panic or an oversized
/// allocation (same discipline as `format::sst::decode`).
pub fn decode(buf: &[u8]) -> Result<FtsDocTerms> {
    let corrupt = |reason: String| EngineError::CorruptFtsDocTerms { reason };

    if buf.len() < HEADER_LEN + CRC_LEN {
        return Err(corrupt("block shorter than fixed header + trailing crc32".to_string()));
    }
    let crc_at = buf.len() - CRC_LEN;
    let expected_crc = u32::from_le_bytes(buf[crc_at..].try_into().expect("slice is exactly 4 bytes"));
    let actual_crc = crc32(&buf[..crc_at]);
    if actual_crc != expected_crc {
        return Err(corrupt(format!(
            "checksum mismatch (expected {expected_crc:#x}, got {actual_crc:#x})"
        )));
    }

    let magic = u32::from_le_bytes(buf[0..4].try_into().expect("slice is exactly 4 bytes"));
    if magic != FTS_DOCTERMS_MAGIC {
        return Err(corrupt("bad magic".to_string()));
    }
    let version = u16::from_le_bytes(buf[4..6].try_into().expect("slice is exactly 2 bytes"));
    if version != FTS_DOCTERMS_VERSION {
        return Err(EngineError::UnsupportedFtsDocTermsVersion {
            expected: FTS_DOCTERMS_VERSION,
            found: version,
        });
    }
    let count = u32::from_le_bytes(buf[6..10].try_into().expect("slice is exactly 4 bytes"));

    let remaining = crc_at.saturating_sub(HEADER_LEN);
    let max_possible_entries = remaining / MIN_ENTRY_LEN;
    if count as u128 > max_possible_entries as u128 {
        return Err(corrupt(format!(
            "count {count} exceeds what the buffer could possibly hold ({max_possible_entries})"
        )));
    }

    let mut pos = HEADER_LEN;
    let mut terms = Vec::with_capacity(count as usize);
    for _ in 0..count {
        if pos + MIN_ENTRY_LEN > crc_at {
            return Err(corrupt("truncated entry header".to_string()));
        }
        let term_len = u16::from_le_bytes(buf[pos..pos + 2].try_into().expect("slice is exactly 2 bytes")) as usize;
        pos += 2;
        let tf = u32::from_le_bytes(buf[pos..pos + 4].try_into().expect("slice is exactly 4 bytes"));
        pos += 4;
        if pos + term_len > crc_at {
            return Err(corrupt("truncated entry term".to_string()));
        }
        let term = String::from_utf8(buf[pos..pos + term_len].to_vec())
            .map_err(|_| corrupt("term is not valid UTF-8".to_string()))?;
        pos += term_len;
        terms.push(DocTerm { term, tf });
    }
    if pos != crc_at {
        return Err(corrupt(format!(
            "{} trailing bytes after the declared {count} entries",
            crc_at - pos
        )));
    }

    Ok(FtsDocTerms { terms })
}

/// Total token count of the document (`Σ tf`) — BM25's `|D|`.
#[must_use]
pub fn doc_length(doc: &FtsDocTerms) -> u64 {
    doc.terms.iter().map(|t| u64::from(t.tf)).sum()
}

/// Tokenizes `content` ([`super::tokenizer::tokenize`]) and aggregates term
/// frequencies into an [`FtsDocTerms`]. A `BTreeMap` intermediate keeps the
/// resulting `terms` in deterministic term order — not a correctness
/// requirement, just a friendlier, reproducible encoding to test against.
#[must_use]
pub fn from_content(content: &str) -> FtsDocTerms {
    let mut counts: std::collections::BTreeMap<String, u32> = std::collections::BTreeMap::new();
    for token in super::tokenizer::tokenize(content) {
        *counts.entry(token).or_insert(0) += 1;
    }
    FtsDocTerms {
        terms: counts.into_iter().map(|(term, tf)| DocTerm { term, tf }).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> FtsDocTerms {
        FtsDocTerms {
            terms: vec![
                DocTerm {
                    term: "chat".to_string(),
                    tf: 2,
                },
                DocTerm {
                    term: "chien".to_string(),
                    tf: 1,
                },
            ],
        }
    }

    #[test]
    fn roundtrips_a_doc() {
        let doc = sample();
        let bytes = encode(&doc).expect("encode ok");
        assert_eq!(decode(&bytes).expect("decode ok"), doc);
    }

    #[test]
    fn roundtrips_an_empty_doc() {
        let doc = FtsDocTerms::default();
        let bytes = encode(&doc).expect("encode ok");
        assert_eq!(decode(&bytes).expect("decode ok"), doc);
    }

    #[test]
    fn doc_length_sums_term_frequencies() {
        assert_eq!(doc_length(&sample()), 3);
        assert_eq!(doc_length(&FtsDocTerms::default()), 0);
    }

    #[test]
    fn from_content_aggregates_term_frequencies_deterministically() {
        let doc = from_content("chat chien chat, chat!");
        assert_eq!(
            doc.terms,
            vec![
                DocTerm {
                    term: "chat".to_string(),
                    tf: 3
                },
                DocTerm {
                    term: "chien".to_string(),
                    tf: 1
                },
            ]
        );
        assert_eq!(doc_length(&doc), 4);
    }

    #[test]
    fn from_content_of_empty_text_is_empty() {
        assert_eq!(from_content(""), FtsDocTerms::default());
    }

    #[test]
    fn truncated_buffer_is_rejected_at_every_cut() {
        let bytes = encode(&sample()).expect("encode ok");
        for cut in 0..bytes.len() {
            let err = decode(&bytes[..cut]).expect_err("truncated block must not decode");
            assert!(
                matches!(err, EngineError::CorruptFtsDocTerms { .. }),
                "unexpected error at cut={cut}: {err}"
            );
        }
    }

    #[test]
    fn bit_flip_is_corrupt_error() {
        let mut bytes = encode(&sample()).expect("encode ok");
        bytes[HEADER_LEN] ^= 0xFF;
        let err = decode(&bytes).expect_err("checksum should fail");
        assert!(matches!(err, EngineError::CorruptFtsDocTerms { .. }));
    }

    #[test]
    fn unknown_version_is_unsupported_error() {
        let mut bytes = encode(&sample()).expect("encode ok");
        bytes[4] = 0xFF;
        let crc_at = bytes.len() - CRC_LEN;
        let crc = crc32(&bytes[..crc_at]);
        bytes[crc_at..].copy_from_slice(&crc.to_le_bytes());
        let err = decode(&bytes).expect_err("unknown version must be rejected");
        assert!(matches!(
            err,
            EngineError::UnsupportedFtsDocTermsVersion { found: 0x00FF, .. }
        ));
    }

    #[test]
    fn huge_count_is_rejected_not_panicking() {
        let mut bytes = encode(&FtsDocTerms::default()).expect("encode ok");
        bytes[6..10].copy_from_slice(&u32::MAX.to_le_bytes());
        let crc_at = bytes.len() - CRC_LEN;
        let crc = crc32(&bytes[..crc_at]);
        bytes[crc_at..].copy_from_slice(&crc.to_le_bytes());
        let err = decode(&bytes).expect_err("huge count must be rejected");
        assert!(matches!(err, EngineError::CorruptFtsDocTerms { .. }));
    }

    #[test]
    fn lying_term_len_is_rejected_not_panicking() {
        let mut bytes = encode(&sample()).expect("encode ok");
        // First entry's term_len, right after the header.
        bytes[HEADER_LEN..HEADER_LEN + 2].copy_from_slice(&u16::MAX.to_le_bytes());
        let crc_at = bytes.len() - CRC_LEN;
        let crc = crc32(&bytes[..crc_at]);
        bytes[crc_at..].copy_from_slice(&crc.to_le_bytes());
        let err = decode(&bytes).expect_err("lying term_len must be rejected");
        assert!(matches!(err, EngineError::CorruptFtsDocTerms { .. }));
    }

    #[test]
    fn trailing_garbage_after_declared_entries_is_rejected() {
        let mut bytes = encode(&FtsDocTerms {
            terms: vec![DocTerm {
                term: "x".to_string(),
                tf: 1,
            }],
        })
        .expect("encode ok");
        // Understate the count so a live entry becomes "trailing garbage".
        bytes[6..10].copy_from_slice(&0u32.to_le_bytes());
        let crc_at = bytes.len() - CRC_LEN;
        let crc = crc32(&bytes[..crc_at]);
        bytes[crc_at..].copy_from_slice(&crc.to_le_bytes());
        let err = decode(&bytes).expect_err("trailing bytes must be rejected");
        assert!(matches!(err, EngineError::CorruptFtsDocTerms { .. }));
    }

    #[test]
    fn oversized_term_is_rejected_at_encode_time() {
        let doc = FtsDocTerms {
            terms: vec![DocTerm {
                term: "x".repeat(usize::from(u16::MAX) + 1),
                tf: 1,
            }],
        };
        let err = encode(&doc).expect_err("term beyond u16 must not silently truncate");
        assert!(matches!(err, EngineError::CorruptFtsDocTerms { .. }));
    }
}
