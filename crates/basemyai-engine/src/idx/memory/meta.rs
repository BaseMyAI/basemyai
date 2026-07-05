//! Memory-index allocator-metadata record layout (N5.1, ADR-027 §4).
//!
//! `format.lock` anchor: `MemoryIndexMeta:1` — bump
//! [`MEMORY_INDEX_META_VERSION`] and this doc comment together whenever the
//! byte layout below changes.
//!
//! The single record under `key::memory_index::meta_key()`: the **monotonic**
//! `next_vec_id` allocator. It is bumped inside the *same* atomic batch as
//! every memory put (ADR-027 §3/§4), so it can never lag behind the vector
//! nodes it allocates for — which is exactly what makes healing from the
//! data safe when this record is absent or corrupt (see
//! [`super::persistent::PersistentMemoryIndex::open`]). Ids are never
//! reused: a decremented or reset counter could resurrect a purged id into a
//! phantom `DuplicateVectorId`.
//!
//! Record layout (all integers little-endian):
//!
//! ```text
//! magic:        u32  = MEMORY_INDEX_META_MAGIC
//! version:      u16  = MEMORY_INDEX_META_VERSION
//! next_vec_id:  u64
//! crc32:        u32   over every byte above
//! ```

use crate::error::{EngineError, Result};
use crate::format::FormatSpec;
use crate::format::checksum::crc32;

pub const MEMORY_INDEX_META_MAGIC: u32 = 0x4D4D_4554; // b"MMET" (LE bytes "TEMM")
pub const MEMORY_INDEX_META_VERSION: u16 = 1;

/// Canonical wire-format spec hashed into `format.lock` (see
/// [`crate::format::lock`]). Field list and order must mirror the byte
/// layout documented above exactly — update both together, never one
/// without the other.
pub fn spec() -> FormatSpec {
    FormatSpec {
        name: "MemoryIndexMeta",
        version: MEMORY_INDEX_META_VERSION,
        fields: &[
            ("magic", "u32"),
            ("version", "u16"),
            ("next_vec_id", "u64"),
            ("crc32", "u32"),
        ],
    }
}

/// Total encoded size: fixed-size record (no variable-length payload).
const META_LEN: usize = 4 + 2 + 8 + 4;
const CRC_LEN: usize = 4;

/// The decoded allocator-metadata record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryIndexMeta {
    /// Next vector id to allocate — strictly monotonic, never reused.
    pub next_vec_id: u64,
}

/// Encodes the metadata record. Infallible in practice (fixed-width fields);
/// returns `Result` for signature symmetry with the sibling codecs.
pub fn encode(meta: &MemoryIndexMeta) -> Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(META_LEN);
    buf.extend_from_slice(&MEMORY_INDEX_META_MAGIC.to_le_bytes());
    buf.extend_from_slice(&MEMORY_INDEX_META_VERSION.to_le_bytes());
    buf.extend_from_slice(&meta.next_vec_id.to_le_bytes());
    let crc = crc32(&buf);
    buf.extend_from_slice(&crc.to_le_bytes());
    Ok(buf)
}

/// Decodes a metadata record previously produced by [`encode`].
///
/// Fixed-length record: an exact-length check runs before any field is
/// read (same discipline as `idx::vector::meta`).
pub fn decode(buf: &[u8]) -> Result<MemoryIndexMeta> {
    let corrupt = |reason: String| EngineError::CorruptMemoryIndexMeta { reason };

    if buf.len() != META_LEN {
        return Err(corrupt(format!(
            "metadata record must be exactly {META_LEN} bytes, got {}",
            buf.len()
        )));
    }
    let crc_at = META_LEN - CRC_LEN;
    let expected_crc = u32::from_le_bytes(buf[crc_at..].try_into().expect("slice is exactly 4 bytes"));
    let actual_crc = crc32(&buf[..crc_at]);
    if actual_crc != expected_crc {
        return Err(corrupt(format!(
            "checksum mismatch (expected {expected_crc:#x}, got {actual_crc:#x})"
        )));
    }

    let magic = u32::from_le_bytes(buf[0..4].try_into().expect("slice is exactly 4 bytes"));
    if magic != MEMORY_INDEX_META_MAGIC {
        return Err(corrupt("bad magic".to_string()));
    }
    let version = u16::from_le_bytes(buf[4..6].try_into().expect("slice is exactly 2 bytes"));
    if version != MEMORY_INDEX_META_VERSION {
        return Err(EngineError::UnsupportedMemoryIndexMetaVersion {
            expected: MEMORY_INDEX_META_VERSION,
            found: version,
        });
    }
    let next_vec_id = u64::from_le_bytes(buf[6..14].try_into().expect("slice is exactly 8 bytes"));

    Ok(MemoryIndexMeta { next_vec_id })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_the_counter() {
        let meta = MemoryIndexMeta { next_vec_id: 42 };
        let bytes = encode(&meta).expect("encode ok");
        assert_eq!(decode(&bytes).expect("decode ok"), meta);
    }

    #[test]
    fn truncated_buffer_is_rejected_at_every_cut() {
        let bytes = encode(&MemoryIndexMeta { next_vec_id: 7 }).expect("encode ok");
        for cut in 0..bytes.len() {
            let err = decode(&bytes[..cut]).expect_err("truncated record must not decode");
            assert!(
                matches!(err, EngineError::CorruptMemoryIndexMeta { .. }),
                "unexpected error at cut={cut}: {err}"
            );
        }
    }

    #[test]
    fn bit_flip_is_corrupt_error() {
        let mut bytes = encode(&MemoryIndexMeta { next_vec_id: 7 }).expect("encode ok");
        bytes[8] ^= 0xFF;
        let err = decode(&bytes).expect_err("checksum should fail");
        assert!(matches!(err, EngineError::CorruptMemoryIndexMeta { .. }));
    }

    #[test]
    fn unknown_version_is_unsupported_error() {
        let mut bytes = encode(&MemoryIndexMeta { next_vec_id: 7 }).expect("encode ok");
        bytes[4] = 0xFF;
        let crc_at = bytes.len() - CRC_LEN;
        let crc = crc32(&bytes[..crc_at]);
        bytes[crc_at..].copy_from_slice(&crc.to_le_bytes());
        let err = decode(&bytes).expect_err("unknown version must be rejected");
        assert!(matches!(
            err,
            EngineError::UnsupportedMemoryIndexMetaVersion { found: 0x00FF, .. }
        ));
    }
}
