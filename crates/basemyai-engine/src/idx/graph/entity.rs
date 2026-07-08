// SPDX-License-Identifier: BUSL-1.1
//! Graph-entity node block layout (N4, `docs/TODO-NATIVE-ENGINE.md`).
//!
//! `format.lock` anchor: `GraphEntity:1` — bump [`GRAPH_ENTITY_VERSION`] and
//! this doc comment together whenever the byte layout below changes.
//!
//! One entity = one self-contained KV value under
//! `key::graph_index::entity_key(agent, id)` — mirrors `idx::vector::node`'s
//! discipline exactly (one logical thing, one block, no sidecar). `agent`
//! and `id` are already fully encoded in the *key*, so the value only needs
//! the entity's own attributes.
//!
//! Block layout (all integers little-endian; the key's own length prefixes,
//! documented in `key::graph_index`, are big-endian — unrelated encoding,
//! don't confuse the two):
//!
//! ```text
//! magic:            u32  = GRAPH_ENTITY_MAGIC
//! version:          u16  = GRAPH_ENTITY_VERSION
//! kind_len:         u16   byte length of `kind`
//! label_len:        u32   byte length of `label` (labels are free text,
//!                         wider field than `kind`'s short taxonomy string)
//! valid_from:       i64
//! has_valid_until:  u8    0 or 1; any other value is rejected on decode
//! valid_until:      i64   meaningful only when has_valid_until == 1
//!                         (encoded as 0 otherwise)
//! kind:             [u8; kind_len]   UTF-8
//! label:            [u8; label_len]  UTF-8
//! crc32:            u32   over every byte above (magic..label)
//! ```

use crate::error::{EngineError, Result};
use crate::format::FormatSpec;
use crate::format::checksum::crc32;

pub const GRAPH_ENTITY_MAGIC: u32 = 0x4745_4E54; // b"GENT" (LE bytes "TNEG")
pub const GRAPH_ENTITY_VERSION: u16 = 1;

/// Canonical wire-format spec hashed into `format.lock` (see
/// [`crate::format::lock`]). Field list and order must mirror the byte
/// layout documented above exactly — update both together, never one
/// without the other.
pub fn spec() -> FormatSpec {
    FormatSpec {
        name: "GraphEntity",
        version: GRAPH_ENTITY_VERSION,
        fields: &[
            ("magic", "u32"),
            ("version", "u16"),
            ("kind_len", "u16"),
            ("label_len", "u32"),
            ("valid_from", "i64"),
            ("has_valid_until", "u8"),
            ("valid_until", "i64"),
            ("kind", "bytes[kind_len]"),
            ("label", "bytes[label_len]"),
            ("crc32", "u32"),
        ],
    }
}

/// Fixed-size portion of a block, before the variable-length `kind`/`label`.
const HEADER_LEN: usize = 4 + 2 + 2 + 4 + 8 + 1 + 8;
const CRC_LEN: usize = 4;

/// A decoded graph-entity block, owned. `agent`/`id` are not part of this
/// type — they live in the key that addresses this block.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphEntity {
    pub kind: String,
    pub label: String,
    pub valid_from: i64,
    /// `None` == valid indefinitely (no expiry), matching
    /// `basemyai::temporal::Validity::valid_until`'s convention.
    pub valid_until: Option<i64>,
}

/// Encodes one entity block.
///
/// Returns `Err(EngineError::CorruptGraphEntity)` if `kind.len()` exceeds
/// `u16::MAX` or `label.len()` exceeds `u32::MAX` — the wire fields are
/// fixed-width and a silent truncation would persist a block that decodes to
/// different content than what was encoded.
pub fn encode(entity: &GraphEntity) -> Result<Vec<u8>> {
    let corrupt = |reason: String| EngineError::CorruptGraphEntity { reason };
    let kind_len = u16::try_from(entity.kind.len())
        .map_err(|_| corrupt(format!("kind length {} exceeds the u16 wire field", entity.kind.len())))?;
    let label_len = u32::try_from(entity.label.len()).map_err(|_| {
        corrupt(format!(
            "label length {} exceeds the u32 wire field",
            entity.label.len()
        ))
    })?;

    let mut buf = Vec::with_capacity(HEADER_LEN + entity.kind.len() + entity.label.len() + CRC_LEN);
    buf.extend_from_slice(&GRAPH_ENTITY_MAGIC.to_le_bytes());
    buf.extend_from_slice(&GRAPH_ENTITY_VERSION.to_le_bytes());
    buf.extend_from_slice(&kind_len.to_le_bytes());
    buf.extend_from_slice(&label_len.to_le_bytes());
    buf.extend_from_slice(&entity.valid_from.to_le_bytes());
    let (has_valid_until, valid_until): (u8, i64) = match entity.valid_until {
        Some(v) => (1, v),
        None => (0, 0),
    };
    buf.push(has_valid_until);
    buf.extend_from_slice(&valid_until.to_le_bytes());
    buf.extend_from_slice(entity.kind.as_bytes());
    buf.extend_from_slice(entity.label.as_bytes());
    let crc = crc32(&buf);
    buf.extend_from_slice(&crc.to_le_bytes());
    Ok(buf)
}

/// Decodes an entity block previously produced by [`encode`].
///
/// N2/N3 fuzzing lesson applied: `kind_len`/`label_len` are wire-controlled
/// count fields, bounded against the *actual* buffer length via an
/// exact-length equation before any string is materialized — a lying length
/// yields `CorruptGraphEntity`, never a panic or oversized allocation.
pub fn decode(buf: &[u8]) -> Result<GraphEntity> {
    let corrupt = |reason: String| EngineError::CorruptGraphEntity { reason };

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
    if magic != GRAPH_ENTITY_MAGIC {
        return Err(corrupt("bad magic".to_string()));
    }
    let version = u16::from_le_bytes(buf[4..6].try_into().expect("slice is exactly 2 bytes"));
    if version != GRAPH_ENTITY_VERSION {
        return Err(EngineError::UnsupportedGraphEntityVersion {
            expected: GRAPH_ENTITY_VERSION,
            found: version,
        });
    }
    let kind_len = u16::from_le_bytes(buf[6..8].try_into().expect("slice is exactly 2 bytes")) as usize;
    let label_len = u32::from_le_bytes(buf[8..12].try_into().expect("slice is exactly 4 bytes")) as usize;
    let valid_from = i64::from_le_bytes(buf[12..20].try_into().expect("slice is exactly 8 bytes"));
    let has_valid_until = buf[20];
    if has_valid_until > 1 {
        return Err(corrupt(format!(
            "has_valid_until must be 0 or 1, got {has_valid_until}"
        )));
    }
    let valid_until_raw = i64::from_le_bytes(buf[21..29].try_into().expect("slice is exactly 8 bytes"));

    let expected_len = HEADER_LEN + kind_len + label_len + CRC_LEN;
    if buf.len() != expected_len {
        return Err(corrupt(format!(
            "declared kind_len {kind_len} + label_len {label_len} imply a {expected_len}-byte block, \
             got {} bytes",
            buf.len()
        )));
    }

    let mut pos = HEADER_LEN;
    let kind = String::from_utf8(buf[pos..pos + kind_len].to_vec())
        .map_err(|_| corrupt("kind is not valid UTF-8".to_string()))?;
    pos += kind_len;
    let label = String::from_utf8(buf[pos..pos + label_len].to_vec())
        .map_err(|_| corrupt("label is not valid UTF-8".to_string()))?;

    Ok(GraphEntity {
        kind,
        label,
        valid_from,
        valid_until: (has_valid_until == 1).then_some(valid_until_raw),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> GraphEntity {
        GraphEntity {
            kind: "person".to_string(),
            label: "Alice".to_string(),
            valid_from: 1000,
            valid_until: None,
        }
    }

    #[test]
    fn roundtrips_an_entity() {
        let entity = sample();
        let bytes = encode(&entity).expect("encode ok");
        assert_eq!(decode(&bytes).expect("decode ok"), entity);
    }

    #[test]
    fn roundtrips_an_entity_with_expiry() {
        let entity = GraphEntity {
            valid_until: Some(2000),
            ..sample()
        };
        let bytes = encode(&entity).expect("encode ok");
        let decoded = decode(&bytes).expect("decode ok");
        assert_eq!(decoded.valid_until, Some(2000));
        assert_eq!(decoded, entity);
    }

    #[test]
    fn roundtrips_empty_kind_and_label() {
        let entity = GraphEntity {
            kind: String::new(),
            label: String::new(),
            valid_from: 0,
            valid_until: None,
        };
        let bytes = encode(&entity).expect("encode ok");
        assert_eq!(decode(&bytes).expect("decode ok"), entity);
    }

    #[test]
    fn roundtrips_utf8_label() {
        let entity = GraphEntity {
            label: "Ålice — 京都".to_string(),
            ..sample()
        };
        let bytes = encode(&entity).expect("encode ok");
        assert_eq!(decode(&bytes).expect("decode ok"), entity);
    }

    #[test]
    fn truncated_buffer_is_rejected_at_every_cut() {
        let bytes = encode(&sample()).expect("encode ok");
        for cut in 0..bytes.len() {
            let err = decode(&bytes[..cut]).expect_err("truncated block must not decode");
            assert!(
                matches!(err, EngineError::CorruptGraphEntity { .. }),
                "unexpected error at cut={cut}: {err}"
            );
        }
    }

    #[test]
    fn bit_flip_is_corrupt_error() {
        let mut bytes = encode(&sample()).expect("encode ok");
        bytes[HEADER_LEN] ^= 0xFF;
        let err = decode(&bytes).expect_err("checksum should fail");
        assert!(matches!(err, EngineError::CorruptGraphEntity { .. }));
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
            EngineError::UnsupportedGraphEntityVersion { found: 0x00FF, .. }
        ));
    }

    #[test]
    fn invalid_has_valid_until_byte_is_rejected() {
        let mut bytes = encode(&sample()).expect("encode ok");
        bytes[20] = 7;
        let crc_at = bytes.len() - CRC_LEN;
        let crc = crc32(&bytes[..crc_at]);
        bytes[crc_at..].copy_from_slice(&crc.to_le_bytes());
        let err = decode(&bytes).expect_err("invalid has_valid_until must be rejected");
        assert!(matches!(err, EngineError::CorruptGraphEntity { .. }));
    }

    #[test]
    fn lying_label_len_is_rejected_not_panicking() {
        let mut bytes = encode(&sample()).expect("encode ok");
        bytes[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        let crc_at = bytes.len() - CRC_LEN;
        let crc = crc32(&bytes[..crc_at]);
        bytes[crc_at..].copy_from_slice(&crc.to_le_bytes());
        let err = decode(&bytes).expect_err("lying label_len must be rejected");
        assert!(matches!(err, EngineError::CorruptGraphEntity { .. }));
    }

    #[test]
    fn lying_kind_len_is_rejected_not_panicking() {
        let mut bytes = encode(&sample()).expect("encode ok");
        bytes[6..8].copy_from_slice(&u16::MAX.to_le_bytes());
        let crc_at = bytes.len() - CRC_LEN;
        let crc = crc32(&bytes[..crc_at]);
        bytes[crc_at..].copy_from_slice(&crc.to_le_bytes());
        let err = decode(&bytes).expect_err("lying kind_len must be rejected");
        assert!(matches!(err, EngineError::CorruptGraphEntity { .. }));
    }

    #[test]
    fn invalid_utf8_kind_is_rejected() {
        let entity = sample();
        let mut bytes = encode(&entity).expect("encode ok");
        // Overwrite `kind`'s bytes ("person") with an invalid UTF-8 sequence
        // of the same length, then fix the crc.
        let kind_start = HEADER_LEN;
        bytes[kind_start] = 0xFF;
        let crc_at = bytes.len() - CRC_LEN;
        let crc = crc32(&bytes[..crc_at]);
        bytes[crc_at..].copy_from_slice(&crc.to_le_bytes());
        let err = decode(&bytes).expect_err("invalid UTF-8 kind must be rejected");
        assert!(matches!(err, EngineError::CorruptGraphEntity { .. }));
    }

    #[test]
    fn oversized_kind_is_rejected_at_encode_time() {
        let entity = GraphEntity {
            kind: "x".repeat(usize::from(u16::MAX) + 1),
            ..sample()
        };
        let err = encode(&entity).expect_err("kind beyond u16 must not silently truncate");
        assert!(matches!(err, EngineError::CorruptGraphEntity { .. }));
    }
}
