// SPDX-License-Identifier: BUSL-1.1
//! `store.meta` — the store-generation marker (ADR-039 §7, `StoreMeta:1`).
//!
//! A tiny standalone file, one per store directory, whose sole purpose is
//! answering "does this build's reader understand this store's on-disk
//! layout" before touching any WAL or SST bytes. Stores created before this
//! marker existed (block-based-SST-format stores predate it entirely) have
//! no `store.meta` at all — that absence, together with other store
//! artifacts being present, is itself the signal an old-generation store is
//! being opened. This module only encodes/decodes the file's bytes; the
//! open-time policy that reacts to "absent" vs "wrong version" vs "missing
//! entirely" (`EngineError::UnsupportedStoreFormat`, ADR-039 §7) is N8.9's
//! job, not this codec's.
//!
//! ```text
//! magic:            u32  = STORE_META_MAGIC
//! store_format_version: u16  = STORE_FORMAT_VERSION (currently 2 — the
//!                               block-based-SST generation this ADR
//!                               introduces; there is no generation-1
//!                               store.meta, see above)
//! crc32:            u32  over every byte above (magic..store_format_version)
//! ```

use std::path::Path;

use super::checksum::crc32;
use crate::error::{EngineError, Result};

pub const STORE_META_MAGIC: u32 = 0x424D_5354; // "TSMB" LE

/// The store-generation this build creates and expects to find. Bump this
/// (and the doc comment above) whenever the on-disk store layout changes in
/// a way that makes older stores unreadable.
pub const STORE_FORMAT_VERSION: u16 = 2;

const STORE_META_LEN: usize = 4 + 2; // magic, store_format_version
const STORE_META_TOTAL_LEN: usize = STORE_META_LEN + 4; // + crc32

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreMeta {
    pub store_format_version: u16,
}

pub fn spec() -> super::FormatSpec {
    super::FormatSpec {
        name: "StoreMeta",
        version: 1,
        fields: &[("magic", "u32"), ("store_format_version", "u16"), ("crc32", "u32")],
    }
}

#[must_use]
pub fn encode(meta: &StoreMeta) -> Vec<u8> {
    let mut buf = Vec::with_capacity(STORE_META_TOTAL_LEN);
    buf.extend_from_slice(&STORE_META_MAGIC.to_le_bytes());
    buf.extend_from_slice(&meta.store_format_version.to_le_bytes());
    let crc = crc32(&buf);
    buf.extend_from_slice(&crc.to_le_bytes());
    buf
}

/// Decodes a `store.meta` file body. Structural corruption (truncation, bad
/// magic, bad checksum) is [`EngineError::CorruptStoreMeta`] — never a
/// panic. This does **not** check `store_format_version` against
/// [`STORE_FORMAT_VERSION`]: an unexpected value is a legitimate, callable
/// decode result, for the open-time caller (N8.9) to turn into
/// `UnsupportedStoreFormat` with the right context (path, expected, found).
pub fn decode(buf: &[u8], path: &Path) -> Result<StoreMeta> {
    let corrupt = |reason: String| EngineError::CorruptStoreMeta {
        path: path.to_path_buf(),
        reason,
    };

    if buf.len() != STORE_META_TOTAL_LEN {
        return Err(corrupt(format!(
            "store.meta must be exactly {STORE_META_TOTAL_LEN} bytes, got {}",
            buf.len()
        )));
    }
    let crc_at = STORE_META_LEN;
    let expected_crc = u32::from_le_bytes(buf[crc_at..].try_into().expect("slice is exactly 4 bytes"));
    let actual_crc = crc32(&buf[..crc_at]);
    if actual_crc != expected_crc {
        return Err(corrupt(format!(
            "checksum mismatch (expected {expected_crc:#x}, got {actual_crc:#x})"
        )));
    }

    let magic = u32::from_le_bytes(buf[0..4].try_into().expect("slice is exactly 4 bytes"));
    if magic != STORE_META_MAGIC {
        return Err(corrupt("bad magic".to_string()));
    }
    let store_format_version = u16::from_le_bytes(buf[4..6].try_into().expect("slice is exactly 2 bytes"));

    Ok(StoreMeta { store_format_version })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn path() -> PathBuf {
        PathBuf::from("store.meta")
    }

    fn sample() -> StoreMeta {
        StoreMeta {
            store_format_version: STORE_FORMAT_VERSION,
        }
    }

    #[test]
    fn roundtrips() {
        let meta = sample();
        let bytes = encode(&meta);
        let decoded = decode(&bytes, &path()).expect("decode ok");
        assert_eq!(decoded, meta);
    }

    #[test]
    fn bit_flip_is_corrupt_error() {
        let mut bytes = encode(&sample());
        bytes[4] ^= 0xFF;
        let err = decode(&bytes, &path()).expect_err("bit flip should fail");
        assert!(matches!(err, EngineError::CorruptStoreMeta { .. }));
    }

    #[test]
    fn truncation_is_corrupt_error_at_every_cut() {
        let bytes = encode(&sample());
        for cut in 0..bytes.len() {
            let err = decode(&bytes[..cut], &path()).expect_err("truncated store.meta is corrupt");
            assert!(matches!(err, EngineError::CorruptStoreMeta { .. }), "cut={cut}: {err}");
        }
    }

    #[test]
    fn unexpected_version_still_decodes_for_the_caller_to_judge() {
        let meta = StoreMeta {
            store_format_version: 999,
        };
        let bytes = encode(&meta);
        let decoded = decode(&bytes, &path()).expect("decode ok even for an unexpected version");
        assert_eq!(decoded, meta);
    }
}
