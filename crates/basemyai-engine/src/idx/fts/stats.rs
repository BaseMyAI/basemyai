//! Per-agent BM25-stats record layout (N5.2, ADR-028 §3/§5).
//!
//! `format.lock` anchor: `FtsStats:1` — bump [`FTS_STATS_VERSION`] and this
//! doc comment together whenever the byte layout below changes.
//!
//! The single record under `key::fts_index::meta_key(agent)`: `doc_count`
//! (documents indexed for this agent) and `total_terms` (sum of every
//! document's length), from which `avgdl = total_terms / doc_count` is
//! derived at query time. Updated inside the *same* atomic batch as every
//! FTS insert/delete (ADR-028 §3), like `MemoryIndexMeta`'s allocator — but
//! **not** healed eagerly on open (there is no "all agents" enumeration at
//! open time, unlike the single global vector-id counter); instead,
//! [`super::persistent::PersistentFts`] heals it lazily, per agent, from
//! that agent's `docterms` when the record is absent or corrupt.
//!
//! Record layout (all integers little-endian):
//!
//! ```text
//! magic:        u32  = FTS_STATS_MAGIC
//! version:      u16  = FTS_STATS_VERSION
//! doc_count:    u64
//! total_terms:  u64
//! crc32:        u32   over every byte above
//! ```

use crate::error::{EngineError, Result};
use crate::format::FormatSpec;
use crate::format::checksum::crc32;

pub const FTS_STATS_MAGIC: u32 = 0x4653_5453; // b"FSTS" (LE bytes "STSF")
pub const FTS_STATS_VERSION: u16 = 1;

/// Canonical wire-format spec hashed into `format.lock` (see
/// [`crate::format::lock`]). Field list and order must mirror the byte
/// layout documented above exactly — update both together, never one
/// without the other.
pub fn spec() -> FormatSpec {
    FormatSpec {
        name: "FtsStats",
        version: FTS_STATS_VERSION,
        fields: &[
            ("magic", "u32"),
            ("version", "u16"),
            ("doc_count", "u64"),
            ("total_terms", "u64"),
            ("crc32", "u32"),
        ],
    }
}

/// Total encoded size: fixed-size record (no variable-length payload).
const STATS_LEN: usize = 4 + 2 + 8 + 8 + 4;
const CRC_LEN: usize = 4;

/// The decoded BM25-stats record for one agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FtsStats {
    pub doc_count: u64,
    pub total_terms: u64,
}

impl FtsStats {
    /// Average document length (BM25's `avgdl`) — `0.0` for an agent with no
    /// indexed documents (search short-circuits before this matters: no
    /// documents means no postings, means no hits).
    #[must_use]
    pub fn avgdl(&self) -> f64 {
        if self.doc_count == 0 {
            0.0
        } else {
            self.total_terms as f64 / self.doc_count as f64
        }
    }
}

/// Encodes the stats record. Infallible in practice (fixed-width fields);
/// returns `Result` for signature symmetry with the sibling codecs.
pub fn encode(stats: &FtsStats) -> Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(STATS_LEN);
    buf.extend_from_slice(&FTS_STATS_MAGIC.to_le_bytes());
    buf.extend_from_slice(&FTS_STATS_VERSION.to_le_bytes());
    buf.extend_from_slice(&stats.doc_count.to_le_bytes());
    buf.extend_from_slice(&stats.total_terms.to_le_bytes());
    let crc = crc32(&buf);
    buf.extend_from_slice(&crc.to_le_bytes());
    Ok(buf)
}

/// Decodes a stats record previously produced by [`encode`].
///
/// Fixed-length record: an exact-length check runs before any field is
/// read (same discipline as `idx::memory::meta`).
pub fn decode(buf: &[u8]) -> Result<FtsStats> {
    let corrupt = |reason: String| EngineError::CorruptFtsStats { reason };

    if buf.len() != STATS_LEN {
        return Err(corrupt(format!(
            "stats record must be exactly {STATS_LEN} bytes, got {}",
            buf.len()
        )));
    }
    let crc_at = STATS_LEN - CRC_LEN;
    let expected_crc = u32::from_le_bytes(buf[crc_at..].try_into().expect("slice is exactly 4 bytes"));
    let actual_crc = crc32(&buf[..crc_at]);
    if actual_crc != expected_crc {
        return Err(corrupt(format!(
            "checksum mismatch (expected {expected_crc:#x}, got {actual_crc:#x})"
        )));
    }

    let magic = u32::from_le_bytes(buf[0..4].try_into().expect("slice is exactly 4 bytes"));
    if magic != FTS_STATS_MAGIC {
        return Err(corrupt("bad magic".to_string()));
    }
    let version = u16::from_le_bytes(buf[4..6].try_into().expect("slice is exactly 2 bytes"));
    if version != FTS_STATS_VERSION {
        return Err(EngineError::UnsupportedFtsStatsVersion {
            expected: FTS_STATS_VERSION,
            found: version,
        });
    }
    let doc_count = u64::from_le_bytes(buf[6..14].try_into().expect("slice is exactly 8 bytes"));
    let total_terms = u64::from_le_bytes(buf[14..22].try_into().expect("slice is exactly 8 bytes"));

    Ok(FtsStats { doc_count, total_terms })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_stats() {
        let stats = FtsStats {
            doc_count: 5,
            total_terms: 42,
        };
        let bytes = encode(&stats).expect("encode ok");
        assert_eq!(decode(&bytes).expect("decode ok"), stats);
    }

    #[test]
    fn avgdl_divides_total_by_count() {
        let stats = FtsStats {
            doc_count: 4,
            total_terms: 20,
        };
        assert!((stats.avgdl() - 5.0).abs() < f64::EPSILON);
    }

    #[test]
    fn avgdl_of_empty_agent_is_zero() {
        assert_eq!(FtsStats::default().avgdl(), 0.0);
    }

    #[test]
    fn truncated_buffer_is_rejected_at_every_cut() {
        let bytes = encode(&FtsStats {
            doc_count: 1,
            total_terms: 1,
        })
        .expect("encode ok");
        for cut in 0..bytes.len() {
            let err = decode(&bytes[..cut]).expect_err("truncated record must not decode");
            assert!(
                matches!(err, EngineError::CorruptFtsStats { .. }),
                "unexpected error at cut={cut}: {err}"
            );
        }
    }

    #[test]
    fn bit_flip_is_corrupt_error() {
        let mut bytes = encode(&FtsStats {
            doc_count: 1,
            total_terms: 1,
        })
        .expect("encode ok");
        bytes[8] ^= 0xFF;
        let err = decode(&bytes).expect_err("checksum should fail");
        assert!(matches!(err, EngineError::CorruptFtsStats { .. }));
    }

    #[test]
    fn unknown_version_is_unsupported_error() {
        let mut bytes = encode(&FtsStats {
            doc_count: 1,
            total_terms: 1,
        })
        .expect("encode ok");
        bytes[4] = 0xFF;
        let crc_at = bytes.len() - CRC_LEN;
        let crc = crc32(&bytes[..crc_at]);
        bytes[crc_at..].copy_from_slice(&crc.to_le_bytes());
        let err = decode(&bytes).expect_err("unknown version must be rejected");
        assert!(matches!(
            err,
            EngineError::UnsupportedFtsStatsVersion { found: 0x00FF, .. }
        ));
    }

    #[test]
    fn oversized_buffer_is_rejected_too() {
        let mut bytes = encode(&FtsStats {
            doc_count: 1,
            total_terms: 1,
        })
        .expect("encode ok");
        bytes.push(0);
        let err = decode(&bytes).expect_err("oversized record must not decode");
        assert!(matches!(err, EngineError::CorruptFtsStats { .. }));
    }
}
