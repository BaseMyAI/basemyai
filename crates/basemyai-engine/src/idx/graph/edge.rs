//! Graph-edge relation block layout (N4, `docs/TODO-NATIVE-ENGINE.md`).
//!
//! `format.lock` anchor: `GraphEdge:1` — bump [`GRAPH_EDGE_VERSION`] and this
//! doc comment together whenever the byte layout below changes.
//!
//! One outgoing edge `(agent, src) --relation--> dst` = one KV record under
//! `key::graph_index::edge_key(agent, src, relation, dst)`. `relation` and
//! `dst` are already fully encoded in the *key* (see that module's doc for
//! why: it's what makes "every outgoing edge of a node" a single prefix
//! scan), so this value only carries the edge's own attributes — weight and
//! validity window — a small, fixed-size record with no variable-length
//! payload at all.
//!
//! Record layout (all integers/floats little-endian):
//!
//! ```text
//! magic:            u32  = GRAPH_EDGE_MAGIC
//! version:          u16  = GRAPH_EDGE_VERSION
//! weight:           f64
//! valid_from:       i64
//! has_valid_until:  u8    0 or 1; any other value is rejected on decode
//! valid_until:      i64   meaningful only when has_valid_until == 1
//!                         (encoded as 0 otherwise)
//! crc32:            u32   over every byte above (magic..valid_until)
//! ```

use crate::error::{EngineError, Result};
use crate::format::FormatSpec;
use crate::format::checksum::crc32;

pub const GRAPH_EDGE_MAGIC: u32 = 0x4745_4447; // b"GEDG" (LE bytes "GDEG")
pub const GRAPH_EDGE_VERSION: u16 = 1;

/// Canonical wire-format spec hashed into `format.lock` (see
/// [`crate::format::lock`]). Field list and order must mirror the byte
/// layout documented above exactly — update both together, never one
/// without the other.
pub fn spec() -> FormatSpec {
    FormatSpec {
        name: "GraphEdge",
        version: GRAPH_EDGE_VERSION,
        fields: &[
            ("magic", "u32"),
            ("version", "u16"),
            ("weight", "f64"),
            ("valid_from", "i64"),
            ("has_valid_until", "u8"),
            ("valid_until", "i64"),
            ("crc32", "u32"),
        ],
    }
}

/// Total encoded size: fixed-size record (no variable-length payload).
const RECORD_LEN: usize = 4 + 2 + 8 + 8 + 1 + 8 + 4;
const CRC_LEN: usize = 4;

/// A decoded graph-edge record, owned. `src`/`relation`/`dst`/`agent` are
/// not part of this type — they live in the key that addresses this record.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GraphEdgeMeta {
    pub weight: f64,
    pub valid_from: i64,
    /// `None` == valid indefinitely (no expiry).
    pub valid_until: Option<i64>,
}

/// Encodes one edge record. Infallible: every field is already fixed-width.
#[must_use]
pub fn encode(edge: &GraphEdgeMeta) -> Vec<u8> {
    let mut buf = Vec::with_capacity(RECORD_LEN);
    buf.extend_from_slice(&GRAPH_EDGE_MAGIC.to_le_bytes());
    buf.extend_from_slice(&GRAPH_EDGE_VERSION.to_le_bytes());
    buf.extend_from_slice(&edge.weight.to_le_bytes());
    buf.extend_from_slice(&edge.valid_from.to_le_bytes());
    let (has_valid_until, valid_until): (u8, i64) = match edge.valid_until {
        Some(v) => (1, v),
        None => (0, 0),
    };
    buf.push(has_valid_until);
    buf.extend_from_slice(&valid_until.to_le_bytes());
    let crc = crc32(&buf);
    buf.extend_from_slice(&crc.to_le_bytes());
    buf
}

/// Decodes an edge record previously produced by [`encode`].
///
/// Fixed-length record: an exact-length check runs before any field is
/// read, so nothing on the wire can drive an out-of-bounds read (same
/// discipline as `idx::vector::meta`).
pub fn decode(buf: &[u8]) -> Result<GraphEdgeMeta> {
    let corrupt = |reason: String| EngineError::CorruptGraphEdge { reason };

    if buf.len() != RECORD_LEN {
        return Err(corrupt(format!(
            "edge record must be exactly {RECORD_LEN} bytes, got {}",
            buf.len()
        )));
    }
    let crc_at = RECORD_LEN - CRC_LEN;
    let expected_crc = u32::from_le_bytes(buf[crc_at..].try_into().expect("slice is exactly 4 bytes"));
    let actual_crc = crc32(&buf[..crc_at]);
    if actual_crc != expected_crc {
        return Err(corrupt(format!(
            "checksum mismatch (expected {expected_crc:#x}, got {actual_crc:#x})"
        )));
    }

    let magic = u32::from_le_bytes(buf[0..4].try_into().expect("slice is exactly 4 bytes"));
    if magic != GRAPH_EDGE_MAGIC {
        return Err(corrupt("bad magic".to_string()));
    }
    let version = u16::from_le_bytes(buf[4..6].try_into().expect("slice is exactly 2 bytes"));
    if version != GRAPH_EDGE_VERSION {
        return Err(EngineError::UnsupportedGraphEdgeVersion {
            expected: GRAPH_EDGE_VERSION,
            found: version,
        });
    }
    let weight = f64::from_le_bytes(buf[6..14].try_into().expect("slice is exactly 8 bytes"));
    let valid_from = i64::from_le_bytes(buf[14..22].try_into().expect("slice is exactly 8 bytes"));
    let has_valid_until = buf[22];
    if has_valid_until > 1 {
        return Err(corrupt(format!(
            "has_valid_until must be 0 or 1, got {has_valid_until}"
        )));
    }
    let valid_until_raw = i64::from_le_bytes(buf[23..31].try_into().expect("slice is exactly 8 bytes"));

    Ok(GraphEdgeMeta {
        weight,
        valid_from,
        valid_until: (has_valid_until == 1).then_some(valid_until_raw),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> GraphEdgeMeta {
        GraphEdgeMeta {
            weight: 1.5,
            valid_from: 1000,
            valid_until: None,
        }
    }

    #[test]
    fn roundtrips_an_edge() {
        let edge = sample();
        let bytes = encode(&edge);
        assert_eq!(bytes.len(), RECORD_LEN);
        assert_eq!(decode(&bytes).expect("decode ok"), edge);
    }

    #[test]
    fn roundtrips_an_edge_with_expiry() {
        let edge = GraphEdgeMeta {
            valid_until: Some(2000),
            ..sample()
        };
        let bytes = encode(&edge);
        assert_eq!(decode(&bytes).expect("decode ok"), edge);
    }

    #[test]
    fn truncated_buffer_is_rejected_at_every_cut() {
        let bytes = encode(&sample());
        for cut in 0..bytes.len() {
            let err = decode(&bytes[..cut]).expect_err("truncated record must not decode");
            assert!(
                matches!(err, EngineError::CorruptGraphEdge { .. }),
                "unexpected error at cut={cut}: {err}"
            );
        }
    }

    #[test]
    fn bit_flip_is_corrupt_error() {
        let mut bytes = encode(&sample());
        bytes[10] ^= 0xFF; // flip a byte of the weight field
        let err = decode(&bytes).expect_err("checksum should fail");
        assert!(matches!(err, EngineError::CorruptGraphEdge { .. }));
    }

    #[test]
    fn unknown_version_is_unsupported_error() {
        let mut bytes = encode(&sample());
        bytes[4] = 0xFF;
        let crc_at = RECORD_LEN - CRC_LEN;
        let crc = crc32(&bytes[..crc_at]);
        bytes[crc_at..].copy_from_slice(&crc.to_le_bytes());
        let err = decode(&bytes).expect_err("unknown version must be rejected");
        assert!(matches!(
            err,
            EngineError::UnsupportedGraphEdgeVersion { found: 0x00FF, .. }
        ));
    }

    #[test]
    fn invalid_has_valid_until_byte_is_rejected() {
        let mut bytes = encode(&sample());
        bytes[22] = 5;
        let crc_at = RECORD_LEN - CRC_LEN;
        let crc = crc32(&bytes[..crc_at]);
        bytes[crc_at..].copy_from_slice(&crc.to_le_bytes());
        let err = decode(&bytes).expect_err("invalid has_valid_until must be rejected");
        assert!(matches!(err, EngineError::CorruptGraphEdge { .. }));
    }

    #[test]
    fn oversized_buffer_is_rejected() {
        let mut bytes = encode(&sample());
        bytes.push(0);
        let err = decode(&bytes).expect_err("oversized record must be rejected");
        assert!(matches!(err, EngineError::CorruptGraphEdge { .. }));
    }
}
