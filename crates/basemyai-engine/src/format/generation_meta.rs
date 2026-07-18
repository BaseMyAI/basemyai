// SPDX-License-Identifier: BUSL-1.1
//! `generation.meta` — atomic active-generation pointer for a full DEK
//! rotation (ADR-042 §3). It lives at the store root and contains no key
//! material: the generation number is authenticated independently by the
//! active generation's `CryptoMeta:2` DEK wrap AAD.
//!
//! ```text
//! magic:              u32 = GENERATION_META_MAGIC
//! version:            u16 = GENERATION_META_VERSION
//! current_generation: u64
//! crc32:              u32 over every byte above
//! ```

use std::path::Path;

use super::checksum::crc32;
use crate::error::{EngineError, Result};

pub const GENERATION_META_MAGIC: u32 = 0x4247_454E; // "NEGB" LE
pub const GENERATION_META_VERSION: u16 = 1;
pub const GENERATION_META_FILENAME: &str = "generation.meta";
const BODY_LEN: usize = 4 + 2 + 8;
const TOTAL_LEN: usize = BODY_LEN + 4;

/// The sole root-level publication record for an active store generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenerationMeta {
    pub current_generation: u64,
}

pub fn spec() -> super::FormatSpec {
    super::FormatSpec {
        name: "GenerationMeta",
        version: GENERATION_META_VERSION,
        fields: &[
            ("magic", "u32"),
            ("version", "u16"),
            ("current_generation", "u64"),
            ("crc32", "u32"),
        ],
    }
}

#[must_use]
pub fn encode(meta: &GenerationMeta) -> Vec<u8> {
    let mut buf = Vec::with_capacity(TOTAL_LEN);
    buf.extend_from_slice(&GENERATION_META_MAGIC.to_le_bytes());
    buf.extend_from_slice(&GENERATION_META_VERSION.to_le_bytes());
    buf.extend_from_slice(&meta.current_generation.to_le_bytes());
    buf.extend_from_slice(&crc32(&buf).to_le_bytes());
    buf
}

/// Decodes only a complete pointer. A missing pointer remains a separate
/// legacy-root layout case for the engine opener to decide.
pub fn decode(buf: &[u8], path: &Path) -> Result<GenerationMeta> {
    let corrupt = |reason: String| EngineError::CorruptGenerationMeta {
        path: path.to_path_buf(),
        reason,
    };
    if buf.len() != TOTAL_LEN {
        return Err(corrupt(format!(
            "generation.meta must be exactly {TOTAL_LEN} bytes, got {}",
            buf.len()
        )));
    }
    let expected_crc = u32::from_le_bytes(buf[BODY_LEN..].try_into().expect("slice is exactly 4 bytes"));
    let actual_crc = crc32(&buf[..BODY_LEN]);
    if actual_crc != expected_crc {
        return Err(corrupt(format!(
            "checksum mismatch (expected {expected_crc:#x}, got {actual_crc:#x})"
        )));
    }
    let magic = u32::from_le_bytes(buf[..4].try_into().expect("slice is exactly 4 bytes"));
    if magic != GENERATION_META_MAGIC {
        return Err(corrupt("bad magic".to_string()));
    }
    let version = u16::from_le_bytes(buf[4..6].try_into().expect("slice is exactly 2 bytes"));
    if version != GENERATION_META_VERSION {
        return Err(EngineError::UnsupportedFormatVersion {
            path: path.to_path_buf(),
            expected: GENERATION_META_VERSION,
            found: version,
        });
    }
    Ok(GenerationMeta {
        current_generation: u64::from_le_bytes(buf[6..BODY_LEN].try_into().expect("slice is exactly 8 bytes")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn path() -> PathBuf {
        PathBuf::from("test.generation.meta")
    }

    #[test]
    fn roundtrips() {
        let meta = GenerationMeta { current_generation: 42 };
        assert_eq!(decode(&encode(&meta), &path()).expect("decode"), meta);
    }

    #[test]
    fn every_truncation_is_corrupt() {
        let bytes = encode(&GenerationMeta { current_generation: 0 });
        for end in 0..bytes.len() {
            assert!(matches!(
                decode(&bytes[..end], &path()),
                Err(EngineError::CorruptGenerationMeta { .. })
            ));
        }
    }

    #[test]
    fn crc_valid_unknown_version_is_unsupported() {
        let mut bytes = encode(&GenerationMeta { current_generation: 0 });
        bytes[4..6].copy_from_slice(&2_u16.to_le_bytes());
        let crc = crc32(&bytes[..BODY_LEN]);
        bytes[BODY_LEN..].copy_from_slice(&crc.to_le_bytes());
        assert!(matches!(
            decode(&bytes, &path()),
            Err(EngineError::UnsupportedFormatVersion { found: 2, .. })
        ));
    }
}
