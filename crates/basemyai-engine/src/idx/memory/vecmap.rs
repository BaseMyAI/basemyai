// SPDX-License-Identifier: BUSL-1.1
//! Vector-id reverse-mapping record layout (N5.1, ADR-027 §2/§4).
//!
//! `format.lock` anchor: `MemoryVecMap:1` — bump [`MEMORY_VECMAP_VERSION`]
//! and this doc comment together whenever the byte layout below changes.
//!
//! One mapping = one KV value under `key::memory_index::vecmap_key(vec_id)`,
//! resolving a vector-index id (`u64`, what a search returns) back to the
//! `(agent, id)` pair that owns the memory. The forward direction (`vec_id`)
//! travels inside the [`super::record`] block; this record exists because a
//! search hit arrives as a bare `u64` and the record key needs `agent` + `id`
//! to be reconstructed.
//!
//! Record layout (all integers little-endian):
//!
//! ```text
//! magic:      u32  = MEMORY_VECMAP_MAGIC
//! version:    u16  = MEMORY_VECMAP_VERSION
//! agent_len:  u32   byte length of `agent`
//! id_len:     u32   byte length of `id`
//! agent:      [u8; agent_len]  UTF-8
//! id:         [u8; id_len]     UTF-8
//! crc32:      u32   over every byte above (magic..id)
//! ```
//!
//! `agent_len`/`id_len` are `u32` to match the width the key encoders'
//! length prefixes accept (`key::memory_index`) — anything encodable in a
//! key must be encodable here.

use crate::error::{EngineError, Result};
use crate::format::FormatSpec;
use crate::format::checksum::crc32;

pub const MEMORY_VECMAP_MAGIC: u32 = 0x4D56_4D50; // b"MVMP" (LE bytes "PMVM")
pub const MEMORY_VECMAP_VERSION: u16 = 1;

/// Canonical wire-format spec hashed into `format.lock` (see
/// [`crate::format::lock`]). Field list and order must mirror the byte
/// layout documented above exactly — update both together, never one
/// without the other.
pub fn spec() -> FormatSpec {
    FormatSpec {
        name: "MemoryVecMap",
        version: MEMORY_VECMAP_VERSION,
        fields: &[
            ("magic", "u32"),
            ("version", "u16"),
            ("agent_len", "u32"),
            ("id_len", "u32"),
            ("agent", "bytes[agent_len]"),
            ("id", "bytes[id_len]"),
            ("crc32", "u32"),
        ],
    }
}

/// Fixed-size portion of a record, before the variable-length `agent`/`id`.
const HEADER_LEN: usize = 4 + 2 + 4 + 4;
const CRC_LEN: usize = 4;

/// A decoded reverse mapping, owned: the `(agent, id)` pair owning the
/// vector id this record is keyed by (the `vec_id` itself lives in the key,
/// see `key::memory_index::vecmap_key`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VecMapEntry {
    pub agent: String,
    pub id: String,
}

/// Encodes one mapping record.
///
/// Returns `Err(EngineError::CorruptMemoryVecMap)` if `agent`/`id` exceed
/// their `u32` wire fields — silent truncation would resolve a search hit to
/// the wrong memory.
pub fn encode(entry: &VecMapEntry) -> Result<Vec<u8>> {
    let corrupt = |reason: String| EngineError::CorruptMemoryVecMap { reason };
    let agent_len = u32::try_from(entry.agent.len())
        .map_err(|_| corrupt(format!("agent length {} exceeds the u32 wire field", entry.agent.len())))?;
    let id_len = u32::try_from(entry.id.len())
        .map_err(|_| corrupt(format!("id length {} exceeds the u32 wire field", entry.id.len())))?;

    let mut buf = Vec::with_capacity(HEADER_LEN + entry.agent.len() + entry.id.len() + CRC_LEN);
    buf.extend_from_slice(&MEMORY_VECMAP_MAGIC.to_le_bytes());
    buf.extend_from_slice(&MEMORY_VECMAP_VERSION.to_le_bytes());
    buf.extend_from_slice(&agent_len.to_le_bytes());
    buf.extend_from_slice(&id_len.to_le_bytes());
    buf.extend_from_slice(entry.agent.as_bytes());
    buf.extend_from_slice(entry.id.as_bytes());
    let crc = crc32(&buf);
    buf.extend_from_slice(&crc.to_le_bytes());
    Ok(buf)
}

/// Decodes a mapping record previously produced by [`encode`].
///
/// N2/N3 fuzzing lesson applied: `agent_len`/`id_len` are wire-controlled
/// counts, bounded against the actual buffer length via an exact-length
/// equation before any string is materialized.
pub fn decode(buf: &[u8]) -> Result<VecMapEntry> {
    let corrupt = |reason: String| EngineError::CorruptMemoryVecMap { reason };

    if buf.len() < HEADER_LEN + CRC_LEN {
        return Err(corrupt("record shorter than fixed header + trailing crc32".to_string()));
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
    if magic != MEMORY_VECMAP_MAGIC {
        return Err(corrupt("bad magic".to_string()));
    }
    let version = u16::from_le_bytes(buf[4..6].try_into().expect("slice is exactly 2 bytes"));
    if version != MEMORY_VECMAP_VERSION {
        return Err(EngineError::UnsupportedMemoryVecMapVersion {
            expected: MEMORY_VECMAP_VERSION,
            found: version,
        });
    }
    let agent_len = u32::from_le_bytes(buf[6..10].try_into().expect("slice is exactly 4 bytes")) as usize;
    let id_len = u32::from_le_bytes(buf[10..14].try_into().expect("slice is exactly 4 bytes")) as usize;

    let expected_len = HEADER_LEN
        .checked_add(agent_len)
        .and_then(|n| n.checked_add(id_len))
        .and_then(|n| n.checked_add(CRC_LEN));
    if expected_len != Some(buf.len()) {
        return Err(corrupt(format!(
            "declared agent_len {agent_len} + id_len {id_len} do not match the {}-byte record",
            buf.len()
        )));
    }

    let mut pos = HEADER_LEN;
    let agent = String::from_utf8(buf[pos..pos + agent_len].to_vec())
        .map_err(|_| corrupt("agent is not valid UTF-8".to_string()))?;
    pos += agent_len;
    let id =
        String::from_utf8(buf[pos..pos + id_len].to_vec()).map_err(|_| corrupt("id is not valid UTF-8".to_string()))?;

    Ok(VecMapEntry { agent, id })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> VecMapEntry {
        VecMapEntry {
            agent: "agent-a".to_string(),
            id: "m1".to_string(),
        }
    }

    #[test]
    fn roundtrips_an_entry() {
        let entry = sample();
        let bytes = encode(&entry).expect("encode ok");
        assert_eq!(decode(&bytes).expect("decode ok"), entry);
    }

    #[test]
    fn roundtrips_empty_and_utf8_fields() {
        let entry = VecMapEntry {
            agent: String::new(),
            id: "identité — 京都".to_string(),
        };
        let bytes = encode(&entry).expect("encode ok");
        assert_eq!(decode(&bytes).expect("decode ok"), entry);
    }

    #[test]
    fn truncated_buffer_is_rejected_at_every_cut() {
        let bytes = encode(&sample()).expect("encode ok");
        for cut in 0..bytes.len() {
            let err = decode(&bytes[..cut]).expect_err("truncated record must not decode");
            assert!(
                matches!(err, EngineError::CorruptMemoryVecMap { .. }),
                "unexpected error at cut={cut}: {err}"
            );
        }
    }

    #[test]
    fn bit_flip_is_corrupt_error() {
        let mut bytes = encode(&sample()).expect("encode ok");
        bytes[HEADER_LEN] ^= 0xFF;
        let err = decode(&bytes).expect_err("checksum should fail");
        assert!(matches!(err, EngineError::CorruptMemoryVecMap { .. }));
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
            EngineError::UnsupportedMemoryVecMapVersion { found: 0x00FF, .. }
        ));
    }

    #[test]
    fn lying_lengths_are_rejected_not_panicking() {
        for range in [6..10, 10..14] {
            let mut bytes = encode(&sample()).expect("encode ok");
            bytes[range].copy_from_slice(&u32::MAX.to_le_bytes());
            let crc_at = bytes.len() - CRC_LEN;
            let crc = crc32(&bytes[..crc_at]);
            bytes[crc_at..].copy_from_slice(&crc.to_le_bytes());
            let err = decode(&bytes).expect_err("lying length must be rejected");
            assert!(matches!(err, EngineError::CorruptMemoryVecMap { .. }));
        }
    }
}
