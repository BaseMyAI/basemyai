// SPDX-License-Identifier: BUSL-1.1
//! `wal_epoch.meta` — the WAL anti-replay episode counter (ADR-044 §2,
//! CRYPTO-1). Lives next to `wal.log` (one per store generation directory)
//! and is bumped exactly once per successful [`crate::store::wal::Wal::reset`]
//! (WAL truncation after a flush) — never by an ordinary compaction, which
//! never touches the WAL.
//!
//! ```text
//! magic:      u32 = WAL_EPOCH_MAGIC
//! version:    u16 = WAL_EPOCH_VERSION
//! wal_epoch:  u64
//! crc32:      u32 over every byte above
//! ```
//!
//! Published durably (tmp + fsync + rename + parent-dir fsync, same idiom as
//! [`super::generation_meta`]) **before** the truncation it precedes — a
//! crash between the two leaves either the old epoch next to an untruncated
//! WAL (still consistent with the epoch it announces) or the new epoch next
//! to an already-empty WAL (also consistent) — never a WAL that claims an
//! episode no durable file confirms. See `store::wal` for the publish call
//! site and `format::crypto::wal_envelope_aad_v2` for how this value binds
//! into every encrypted WAL envelope's AAD.

use std::path::Path;

use super::checksum::crc32;
use crate::error::{EngineError, Result};

pub const WAL_EPOCH_MAGIC: u32 = 0x4257_4550; // "PEWB" LE ("BWEP" domain tag)
pub const WAL_EPOCH_VERSION: u16 = 1;
pub const WAL_EPOCH_FILENAME: &str = "wal_epoch.meta";

const BODY_LEN: usize = 4 + 2 + 8;
const TOTAL_LEN: usize = BODY_LEN + 4;

/// The sole per-generation publication record for the current WAL episode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalEpoch {
    pub wal_epoch: u64,
}

pub fn spec() -> super::FormatSpec {
    super::FormatSpec {
        name: "WalEpoch",
        version: WAL_EPOCH_VERSION,
        fields: &[
            ("magic", "u32"),
            ("version", "u16"),
            ("wal_epoch", "u64"),
            ("crc32", "u32"),
        ],
    }
}

#[must_use]
pub fn encode(meta: &WalEpoch) -> Vec<u8> {
    let mut buf = Vec::with_capacity(TOTAL_LEN);
    buf.extend_from_slice(&WAL_EPOCH_MAGIC.to_le_bytes());
    buf.extend_from_slice(&WAL_EPOCH_VERSION.to_le_bytes());
    buf.extend_from_slice(&meta.wal_epoch.to_le_bytes());
    buf.extend_from_slice(&crc32(&buf).to_le_bytes());
    buf
}

/// Decodes only a complete `wal_epoch.meta`. A missing file is a separate
/// case the caller (`store::wal::Wal::open_for_append`) decides — genuinely
/// fresh store (create at epoch 0) vs. a pre-ADR-044 store with WAL bytes
/// already on disk (refuse typed, ADR-044 §7).
pub fn decode(buf: &[u8], path: &Path) -> Result<WalEpoch> {
    let corrupt = |reason: String| EngineError::CorruptWalEpoch {
        path: path.to_path_buf(),
        reason,
    };
    if buf.len() != TOTAL_LEN {
        return Err(corrupt(format!(
            "wal_epoch.meta must be exactly {TOTAL_LEN} bytes, got {}",
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
    if magic != WAL_EPOCH_MAGIC {
        return Err(corrupt("bad magic".to_string()));
    }
    let version = u16::from_le_bytes(buf[4..6].try_into().expect("slice is exactly 2 bytes"));
    if version != WAL_EPOCH_VERSION {
        return Err(EngineError::UnsupportedFormatVersion {
            path: path.to_path_buf(),
            expected: WAL_EPOCH_VERSION,
            found: version,
        });
    }
    Ok(WalEpoch {
        wal_epoch: u64::from_le_bytes(buf[6..BODY_LEN].try_into().expect("slice is exactly 8 bytes")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn path() -> PathBuf {
        PathBuf::from("test.wal_epoch.meta")
    }

    #[test]
    fn roundtrips() {
        let meta = WalEpoch { wal_epoch: 42 };
        assert_eq!(decode(&encode(&meta), &path()).expect("decode"), meta);
    }

    #[test]
    fn every_truncation_is_corrupt() {
        let bytes = encode(&WalEpoch { wal_epoch: 0 });
        for end in 0..bytes.len() {
            assert!(matches!(
                decode(&bytes[..end], &path()),
                Err(EngineError::CorruptWalEpoch { .. })
            ));
        }
    }

    #[test]
    fn bit_flip_is_corrupt() {
        let mut bytes = encode(&WalEpoch { wal_epoch: 7 });
        bytes[8] ^= 0xFF;
        assert!(matches!(
            decode(&bytes, &path()),
            Err(EngineError::CorruptWalEpoch { .. })
        ));
    }

    #[test]
    fn crc_valid_unknown_version_is_unsupported() {
        let mut bytes = encode(&WalEpoch { wal_epoch: 0 });
        bytes[4..6].copy_from_slice(&2_u16.to_le_bytes());
        let crc = crc32(&bytes[..BODY_LEN]);
        bytes[BODY_LEN..].copy_from_slice(&crc.to_le_bytes());
        assert!(matches!(
            decode(&bytes, &path()),
            Err(EngineError::UnsupportedFormatVersion { found: 2, .. })
        ));
    }
}
