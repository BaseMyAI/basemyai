// SPDX-License-Identifier: BUSL-1.1
//! Memory-record block layout (N5.1, ADR-027 §2).
//!
//! `format.lock` anchor: `MemoryRecord:1` — bump [`MEMORY_RECORD_VERSION`]
//! and this doc comment together whenever the byte layout below changes.
//!
//! One memory = one self-contained KV value under
//! `key::memory_index::record_key(agent, id)` — `agent` and `id` live in the
//! *key* (structural isolation, same discipline as `idx::graph`), so the
//! value only carries the memory's own attributes. `layer` is an **opaque
//! tag** to this crate: the four-layer semantics (episodic, semantic, …)
//! belong to `basemyai`, exactly like `kind`/`label` on a graph entity —
//! mechanism here, sense at the consumer.
//!
//! `vec_id` links the record to its vector-index node
//! ([`crate::idx::vector`]); the reverse direction lives in its own
//! [`super::vecmap`] record. `importance`/`last_access` are carried for
//! parity with the libSQL schema's columns (adaptive forgetting / GC read
//! them there) even though the N5.1 `MemoryStore` contract only ever writes
//! them — reserving them now avoids a format bump later.
//!
//! Block layout (all integers little-endian):
//!
//! ```text
//! magic:            u32  = MEMORY_RECORD_MAGIC
//! version:          u16  = MEMORY_RECORD_VERSION
//! layer_len:        u16   byte length of `layer` (short taxonomy tag)
//! content_len:      u32   byte length of `content` (free text, wide field)
//! source_len:       u16   byte length of `source` (short provenance tag)
//! valid_from:       i64
//! has_valid_until:  u8    0 or 1; any other value is rejected on decode
//! valid_until:      i64   meaningful only when has_valid_until == 1
//!                         (encoded as 0 otherwise)
//! importance:       f64
//! last_access:      i64
//! vec_id:           u64
//! layer:            [u8; layer_len]    UTF-8
//! content:          [u8; content_len]  UTF-8
//! source:           [u8; source_len]   UTF-8
//! crc32:            u32   over every byte above (magic..source)
//! ```

use crate::error::{EngineError, Result};
use crate::format::FormatSpec;
use crate::format::checksum::crc32;

pub const MEMORY_RECORD_MAGIC: u32 = 0x4D52_4543; // b"MREC" (LE bytes "CERM")
pub const MEMORY_RECORD_VERSION: u16 = 1;

/// Canonical wire-format spec hashed into `format.lock` (see
/// [`crate::format::lock`]). Field list and order must mirror the byte
/// layout documented above exactly — update both together, never one
/// without the other.
pub fn spec() -> FormatSpec {
    FormatSpec {
        name: "MemoryRecord",
        version: MEMORY_RECORD_VERSION,
        fields: &[
            ("magic", "u32"),
            ("version", "u16"),
            ("layer_len", "u16"),
            ("content_len", "u32"),
            ("source_len", "u16"),
            ("valid_from", "i64"),
            ("has_valid_until", "u8"),
            ("valid_until", "i64"),
            ("importance", "f64"),
            ("last_access", "i64"),
            ("vec_id", "u64"),
            ("layer", "bytes[layer_len]"),
            ("content", "bytes[content_len]"),
            ("source", "bytes[source_len]"),
            ("crc32", "u32"),
        ],
    }
}

/// Fixed-size portion of a block, before the variable-length
/// `layer`/`content`/`source`.
const HEADER_LEN: usize = 4 + 2 + 2 + 4 + 2 + 8 + 1 + 8 + 8 + 8 + 8;
const CRC_LEN: usize = 4;

/// A decoded memory-record block, owned. `agent`/`id` are not part of this
/// type — they live in the key that addresses this block.
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryRecord {
    /// Opaque layer tag (the consumer's `MemoryLayer::table()` string).
    pub layer: String,
    pub content: String,
    /// Provenance tag (`"user"`, `"consolidation"`, …).
    pub source: String,
    pub valid_from: i64,
    /// `None` == valid indefinitely (no expiry), matching
    /// `basemyai::temporal::Validity::valid_until`'s convention.
    pub valid_until: Option<i64>,
    pub importance: f64,
    pub last_access: i64,
    /// Id of this memory's node in the vector index
    /// ([`crate::idx::vector`]); allocated once, never reused (ADR-027 §4).
    pub vec_id: u64,
}

/// Encodes one memory-record block.
///
/// Returns `Err(EngineError::CorruptMemoryRecord)` if `layer`/`source`
/// exceed their `u16` wire fields or `content` exceeds its `u32` field — a
/// silent truncation would persist a block that decodes to different content
/// than what was encoded.
pub fn encode(record: &MemoryRecord) -> Result<Vec<u8>> {
    let corrupt = |reason: String| EngineError::CorruptMemoryRecord { reason };
    let layer_len = u16::try_from(record.layer.len()).map_err(|_| {
        corrupt(format!(
            "layer length {} exceeds the u16 wire field",
            record.layer.len()
        ))
    })?;
    let content_len = u32::try_from(record.content.len()).map_err(|_| {
        corrupt(format!(
            "content length {} exceeds the u32 wire field",
            record.content.len()
        ))
    })?;
    let source_len = u16::try_from(record.source.len()).map_err(|_| {
        corrupt(format!(
            "source length {} exceeds the u16 wire field",
            record.source.len()
        ))
    })?;

    let mut buf =
        Vec::with_capacity(HEADER_LEN + record.layer.len() + record.content.len() + record.source.len() + CRC_LEN);
    buf.extend_from_slice(&MEMORY_RECORD_MAGIC.to_le_bytes());
    buf.extend_from_slice(&MEMORY_RECORD_VERSION.to_le_bytes());
    buf.extend_from_slice(&layer_len.to_le_bytes());
    buf.extend_from_slice(&content_len.to_le_bytes());
    buf.extend_from_slice(&source_len.to_le_bytes());
    buf.extend_from_slice(&record.valid_from.to_le_bytes());
    let (has_valid_until, valid_until): (u8, i64) = match record.valid_until {
        Some(v) => (1, v),
        None => (0, 0),
    };
    buf.push(has_valid_until);
    buf.extend_from_slice(&valid_until.to_le_bytes());
    buf.extend_from_slice(&record.importance.to_le_bytes());
    buf.extend_from_slice(&record.last_access.to_le_bytes());
    buf.extend_from_slice(&record.vec_id.to_le_bytes());
    buf.extend_from_slice(record.layer.as_bytes());
    buf.extend_from_slice(record.content.as_bytes());
    buf.extend_from_slice(record.source.as_bytes());
    let crc = crc32(&buf);
    buf.extend_from_slice(&crc.to_le_bytes());
    Ok(buf)
}

/// Decodes a memory-record block previously produced by [`encode`].
///
/// N2/N3 fuzzing lesson applied: `layer_len`/`content_len`/`source_len` are
/// wire-controlled count fields, bounded against the *actual* buffer length
/// via an exact-length equation before any string is materialized — a lying
/// length yields `CorruptMemoryRecord`, never a panic or an oversized
/// allocation.
pub fn decode(buf: &[u8]) -> Result<MemoryRecord> {
    let corrupt = |reason: String| EngineError::CorruptMemoryRecord { reason };

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
    if magic != MEMORY_RECORD_MAGIC {
        return Err(corrupt("bad magic".to_string()));
    }
    let version = u16::from_le_bytes(buf[4..6].try_into().expect("slice is exactly 2 bytes"));
    if version != MEMORY_RECORD_VERSION {
        return Err(EngineError::UnsupportedMemoryRecordVersion {
            expected: MEMORY_RECORD_VERSION,
            found: version,
        });
    }
    let layer_len = u16::from_le_bytes(buf[6..8].try_into().expect("slice is exactly 2 bytes")) as usize;
    let content_len = u32::from_le_bytes(buf[8..12].try_into().expect("slice is exactly 4 bytes")) as usize;
    let source_len = u16::from_le_bytes(buf[12..14].try_into().expect("slice is exactly 2 bytes")) as usize;
    let valid_from = i64::from_le_bytes(buf[14..22].try_into().expect("slice is exactly 8 bytes"));
    let has_valid_until = buf[22];
    if has_valid_until > 1 {
        return Err(corrupt(format!(
            "has_valid_until must be 0 or 1, got {has_valid_until}"
        )));
    }
    let valid_until_raw = i64::from_le_bytes(buf[23..31].try_into().expect("slice is exactly 8 bytes"));
    let importance = f64::from_le_bytes(buf[31..39].try_into().expect("slice is exactly 8 bytes"));
    let last_access = i64::from_le_bytes(buf[39..47].try_into().expect("slice is exactly 8 bytes"));
    let vec_id = u64::from_le_bytes(buf[47..55].try_into().expect("slice is exactly 8 bytes"));

    let expected_len = HEADER_LEN + layer_len + content_len + source_len + CRC_LEN;
    if buf.len() != expected_len {
        return Err(corrupt(format!(
            "declared layer_len {layer_len} + content_len {content_len} + source_len {source_len} \
             imply a {expected_len}-byte block, got {} bytes",
            buf.len()
        )));
    }

    let mut pos = HEADER_LEN;
    let layer = String::from_utf8(buf[pos..pos + layer_len].to_vec())
        .map_err(|_| corrupt("layer is not valid UTF-8".to_string()))?;
    pos += layer_len;
    let content = String::from_utf8(buf[pos..pos + content_len].to_vec())
        .map_err(|_| corrupt("content is not valid UTF-8".to_string()))?;
    pos += content_len;
    let source = String::from_utf8(buf[pos..pos + source_len].to_vec())
        .map_err(|_| corrupt("source is not valid UTF-8".to_string()))?;

    Ok(MemoryRecord {
        layer,
        content,
        source,
        valid_from,
        valid_until: (has_valid_until == 1).then_some(valid_until_raw),
        importance,
        last_access,
        vec_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> MemoryRecord {
        MemoryRecord {
            layer: "episodic".to_string(),
            content: "bonjour".to_string(),
            source: "user".to_string(),
            valid_from: 1000,
            valid_until: None,
            importance: 1.0,
            last_access: 1000,
            vec_id: 42,
        }
    }

    #[test]
    fn roundtrips_a_record() {
        let record = sample();
        let bytes = encode(&record).expect("encode ok");
        assert_eq!(decode(&bytes).expect("decode ok"), record);
    }

    #[test]
    fn roundtrips_a_record_with_expiry() {
        let record = MemoryRecord {
            valid_until: Some(2000),
            ..sample()
        };
        let bytes = encode(&record).expect("encode ok");
        assert_eq!(decode(&bytes).expect("decode ok"), record);
    }

    #[test]
    fn roundtrips_empty_strings_and_utf8_content() {
        let record = MemoryRecord {
            layer: String::new(),
            content: "réunion à 京都 — détails".to_string(),
            source: String::new(),
            ..sample()
        };
        let bytes = encode(&record).expect("encode ok");
        assert_eq!(decode(&bytes).expect("decode ok"), record);
    }

    #[test]
    fn truncated_buffer_is_rejected_at_every_cut() {
        let bytes = encode(&sample()).expect("encode ok");
        for cut in 0..bytes.len() {
            let err = decode(&bytes[..cut]).expect_err("truncated block must not decode");
            assert!(
                matches!(err, EngineError::CorruptMemoryRecord { .. }),
                "unexpected error at cut={cut}: {err}"
            );
        }
    }

    #[test]
    fn bit_flip_is_corrupt_error() {
        let mut bytes = encode(&sample()).expect("encode ok");
        bytes[HEADER_LEN] ^= 0xFF;
        let err = decode(&bytes).expect_err("checksum should fail");
        assert!(matches!(err, EngineError::CorruptMemoryRecord { .. }));
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
            EngineError::UnsupportedMemoryRecordVersion { found: 0x00FF, .. }
        ));
    }

    #[test]
    fn lying_content_len_is_rejected_not_panicking() {
        let mut bytes = encode(&sample()).expect("encode ok");
        bytes[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        let crc_at = bytes.len() - CRC_LEN;
        let crc = crc32(&bytes[..crc_at]);
        bytes[crc_at..].copy_from_slice(&crc.to_le_bytes());
        let err = decode(&bytes).expect_err("lying content_len must be rejected");
        assert!(matches!(err, EngineError::CorruptMemoryRecord { .. }));
    }

    #[test]
    fn lying_layer_and_source_lens_are_rejected_not_panicking() {
        for range in [6..8, 12..14] {
            let mut bytes = encode(&sample()).expect("encode ok");
            bytes[range].copy_from_slice(&u16::MAX.to_le_bytes());
            let crc_at = bytes.len() - CRC_LEN;
            let crc = crc32(&bytes[..crc_at]);
            bytes[crc_at..].copy_from_slice(&crc.to_le_bytes());
            let err = decode(&bytes).expect_err("lying length must be rejected");
            assert!(matches!(err, EngineError::CorruptMemoryRecord { .. }));
        }
    }

    #[test]
    fn invalid_has_valid_until_byte_is_rejected() {
        let mut bytes = encode(&sample()).expect("encode ok");
        bytes[22] = 7;
        let crc_at = bytes.len() - CRC_LEN;
        let crc = crc32(&bytes[..crc_at]);
        bytes[crc_at..].copy_from_slice(&crc.to_le_bytes());
        let err = decode(&bytes).expect_err("invalid has_valid_until must be rejected");
        assert!(matches!(err, EngineError::CorruptMemoryRecord { .. }));
    }

    #[test]
    fn oversized_layer_is_rejected_at_encode_time() {
        let record = MemoryRecord {
            layer: "x".repeat(usize::from(u16::MAX) + 1),
            ..sample()
        };
        let err = encode(&record).expect_err("layer beyond u16 must not silently truncate");
        assert!(matches!(err, EngineError::CorruptMemoryRecord { .. }));
    }
}
