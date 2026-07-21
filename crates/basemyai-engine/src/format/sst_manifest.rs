// SPDX-License-Identifier: BUSL-1.1
//! `manifest.meta` — the durable list of live SSTs for a generation
//! (ENG-DUR-001, `docs/audits/2026-07-engine-architecture-safety-audit.md`;
//! design: `docs/adr/ADR-043-native-version-set-snapshots-and-concurrent-compaction.md`
//! §1). One file per generation directory (the root for generation 0, a
//! `gen-N/` directory for a rotated store) — before this format, the set of
//! live SSTs was never published as an independent fact on disk;
//! `Engine::open` simply trusted whatever `*.sst` files a directory listing
//! happened to contain. A live SST silently deleted (user error, antivirus,
//! failed backup, a crash mid-cleanup) was therefore invisible: `open`
//! succeeded, the data was just gone. This format closes that: an id this
//! file lists but the directory doesn't contain is a typed error
//! ([`crate::error::EngineError::MissingLiveSst`]), not a silent miss.
//!
//! ```text
//! magic:               u32  = SST_MANIFEST_MAGIC
//! version:             u16  = SST_MANIFEST_VERSION
//! manifest_generation: u64  // incremented on every publish
//! live_sst_ids_count:  u32
//! live_sst_ids:        [u64; live_sst_ids_count]  // oldest to newest
//! crc32:               u32  over every byte above
//! ```

use std::path::Path;

use super::checksum::crc32;
use crate::error::{EngineError, Result};

pub const SST_MANIFEST_MAGIC: u32 = 0x4D53_5354; // "TSSM" LE
pub const SST_MANIFEST_VERSION: u16 = 1;
pub const SST_MANIFEST_FILENAME: &str = "manifest.meta";

const HEADER_LEN: usize = 4 + 2 + 8 + 4; // magic, version, manifest_generation, count

/// The durable live-SST list for one generation directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SstManifest {
    pub manifest_generation: u64,
    /// Oldest to newest, same invariant `Engine::ssts` already keeps.
    pub live_sst_ids: Vec<u64>,
}

pub fn spec() -> super::FormatSpec {
    super::FormatSpec {
        name: "SstManifest",
        version: SST_MANIFEST_VERSION,
        fields: &[
            ("magic", "u32"),
            ("version", "u16"),
            ("manifest_generation", "u64"),
            ("live_sst_ids_count", "u32"),
            ("live_sst_ids", "[u64]"),
            ("crc32", "u32"),
        ],
    }
}

#[must_use]
pub fn encode(manifest: &SstManifest) -> Vec<u8> {
    let mut buf = Vec::with_capacity(HEADER_LEN + manifest.live_sst_ids.len() * 8 + 4);
    buf.extend_from_slice(&SST_MANIFEST_MAGIC.to_le_bytes());
    buf.extend_from_slice(&SST_MANIFEST_VERSION.to_le_bytes());
    buf.extend_from_slice(&manifest.manifest_generation.to_le_bytes());
    buf.extend_from_slice(&(manifest.live_sst_ids.len() as u32).to_le_bytes());
    for id in &manifest.live_sst_ids {
        buf.extend_from_slice(&id.to_le_bytes());
    }
    buf.extend_from_slice(&crc32(&buf).to_le_bytes());
    buf
}

/// Decodes a `manifest.meta` file body. Structural corruption (truncation,
/// bad magic, bad checksum) is [`EngineError::CorruptSstManifest`] — never a
/// panic. An unrecognized `version` is
/// [`EngineError::UnsupportedFormatVersion`], same convention as
/// `generation_meta::decode`.
pub fn decode(buf: &[u8], path: &Path) -> Result<SstManifest> {
    let corrupt = |reason: String| EngineError::CorruptSstManifest {
        path: path.to_path_buf(),
        reason,
    };
    if buf.len() < HEADER_LEN + 4 {
        return Err(corrupt(format!(
            "manifest.meta must be at least {} bytes, got {}",
            HEADER_LEN + 4,
            buf.len()
        )));
    }
    let magic = u32::from_le_bytes(buf[0..4].try_into().expect("slice is exactly 4 bytes"));
    if magic != SST_MANIFEST_MAGIC {
        return Err(corrupt("bad magic".to_string()));
    }
    let version = u16::from_le_bytes(buf[4..6].try_into().expect("slice is exactly 2 bytes"));
    if version != SST_MANIFEST_VERSION {
        return Err(EngineError::UnsupportedFormatVersion {
            path: path.to_path_buf(),
            expected: SST_MANIFEST_VERSION,
            found: version,
        });
    }
    let manifest_generation = u64::from_le_bytes(buf[6..14].try_into().expect("slice is exactly 8 bytes"));
    let count = u32::from_le_bytes(buf[14..18].try_into().expect("slice is exactly 4 bytes")) as usize;
    let body_len = HEADER_LEN + count * 8;
    let total_len = body_len + 4;
    if buf.len() != total_len {
        return Err(corrupt(format!(
            "manifest.meta must be exactly {total_len} bytes for {count} ids, got {}",
            buf.len()
        )));
    }
    let expected_crc = u32::from_le_bytes(buf[body_len..].try_into().expect("slice is exactly 4 bytes"));
    let actual_crc = crc32(&buf[..body_len]);
    if actual_crc != expected_crc {
        return Err(corrupt(format!(
            "checksum mismatch (expected {expected_crc:#x}, got {actual_crc:#x})"
        )));
    }
    let mut live_sst_ids = Vec::with_capacity(count);
    for i in 0..count {
        let start = HEADER_LEN + i * 8;
        live_sst_ids.push(u64::from_le_bytes(
            buf[start..start + 8].try_into().expect("slice is exactly 8 bytes"),
        ));
    }
    Ok(SstManifest {
        manifest_generation,
        live_sst_ids,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn path() -> PathBuf {
        PathBuf::from("test.manifest.meta")
    }

    fn sample() -> SstManifest {
        SstManifest {
            manifest_generation: 7,
            live_sst_ids: vec![0, 1, 3, 100],
        }
    }

    #[test]
    fn roundtrips() {
        let manifest = sample();
        assert_eq!(decode(&encode(&manifest), &path()).expect("decode"), manifest);
    }

    #[test]
    fn roundtrips_empty() {
        let manifest = SstManifest {
            manifest_generation: 0,
            live_sst_ids: vec![],
        };
        assert_eq!(decode(&encode(&manifest), &path()).expect("decode"), manifest);
    }

    #[test]
    fn every_truncation_is_corrupt() {
        let bytes = encode(&sample());
        for end in 0..bytes.len() {
            let err = decode(&bytes[..end], &path()).expect_err("truncated manifest.meta is corrupt");
            assert!(
                matches!(err, EngineError::CorruptSstManifest { .. }),
                "end={end}: {err}"
            );
        }
    }

    #[test]
    fn bit_flip_is_corrupt_error() {
        let mut bytes = encode(&sample());
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        let err = decode(&bytes, &path()).expect_err("bit flip should fail");
        assert!(matches!(err, EngineError::CorruptSstManifest { .. }));
    }

    #[test]
    fn crc_valid_unknown_version_is_unsupported() {
        let mut bytes = encode(&sample());
        bytes[4..6].copy_from_slice(&2_u16.to_le_bytes());
        let body_len = bytes.len() - 4;
        let crc = crc32(&bytes[..body_len]);
        bytes[body_len..].copy_from_slice(&crc.to_le_bytes());
        assert!(matches!(
            decode(&bytes, &path()),
            Err(EngineError::UnsupportedFormatVersion { found: 2, .. })
        ));
    }
}
