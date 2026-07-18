// SPDX-License-Identifier: BUSL-1.1
//! `store.meta` — the store-generation marker (ADR-039 §7, `StoreMeta:2`).
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
//! store_id:         [u8; 16] UUIDv7, absent in legacy `StoreMeta:1`
//! crc32:            u32  over every byte above (magic..store_id)
//!
//! Legacy `StoreMeta:1` is exactly 10 bytes and remains readable. The
//! additive `StoreMeta:2` form is 26 bytes; the open path stamps its
//! `store_id` atomically while it holds the engine's exclusive lock.
//! ```

use std::path::Path;

use super::checksum::crc32;
use crate::error::{EngineError, Result};
use uuid::Uuid;

pub const STORE_META_MAGIC: u32 = 0x424D_5354; // "TSMB" LE

/// The store-generation this build creates and expects to find. Bump this
/// (and the doc comment above) whenever the on-disk store layout changes in
/// a way that makes older stores unreadable.
pub const STORE_FORMAT_VERSION: u16 = 2;

const STORE_META_LEN: usize = 4 + 2; // magic, store_format_version
const STORE_META_V1_TOTAL_LEN: usize = STORE_META_LEN + 4; // + crc32
const STORE_META_V2_LEN: usize = STORE_META_LEN + 16; // + store_id
const STORE_META_V2_TOTAL_LEN: usize = STORE_META_V2_LEN + 4; // + crc32

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreMeta {
    pub store_format_version: u16,
    /// Stable per-store identity. `None` means a legacy `StoreMeta:1` record
    /// which must be upgraded by the writable open path before use.
    pub store_id: Option<Uuid>,
}

pub fn store_meta_v1_spec() -> super::FormatSpec {
    super::FormatSpec {
        name: "StoreMeta",
        version: 1,
        fields: &[("magic", "u32"), ("store_format_version", "u16"), ("crc32", "u32")],
    }
}

pub fn store_meta_v2_spec() -> super::FormatSpec {
    super::FormatSpec {
        name: "StoreMetaV2",
        version: 2,
        fields: &[
            ("magic", "u32"),
            ("store_format_version", "u16"),
            ("store_id", "[u8; 16]"),
            ("crc32", "u32"),
        ],
    }
}

#[must_use]
pub fn encode(meta: &StoreMeta) -> Vec<u8> {
    let mut buf = Vec::with_capacity(if meta.store_id.is_some() {
        STORE_META_V2_TOTAL_LEN
    } else {
        STORE_META_V1_TOTAL_LEN
    });
    buf.extend_from_slice(&STORE_META_MAGIC.to_le_bytes());
    buf.extend_from_slice(&meta.store_format_version.to_le_bytes());
    if let Some(store_id) = meta.store_id {
        buf.extend_from_slice(store_id.as_bytes());
    }
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

    let (body_len, has_store_id) = match buf.len() {
        STORE_META_V1_TOTAL_LEN => (STORE_META_LEN, false),
        STORE_META_V2_TOTAL_LEN => (STORE_META_V2_LEN, true),
        len => {
            return Err(corrupt(format!(
                "store.meta must be exactly {STORE_META_V1_TOTAL_LEN} (v1) or {STORE_META_V2_TOTAL_LEN} (v2) bytes, got {len}"
            )));
        }
    };
    let crc_at = body_len;
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
    let store_id = if has_store_id {
        Some(Uuid::from_bytes(
            buf[STORE_META_LEN..STORE_META_V2_LEN]
                .try_into()
                .expect("slice is exactly 16 bytes"),
        ))
    } else {
        None
    };

    Ok(StoreMeta {
        store_format_version,
        store_id,
    })
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
            store_id: Some(Uuid::now_v7()),
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
            store_id: Some(Uuid::now_v7()),
        };
        let bytes = encode(&meta);
        let decoded = decode(&bytes, &path()).expect("decode ok even for an unexpected version");
        assert_eq!(decoded, meta);
    }

    #[test]
    fn legacy_v1_is_still_decoded_without_a_store_id() {
        let legacy = StoreMeta {
            store_format_version: STORE_FORMAT_VERSION,
            store_id: None,
        };
        let decoded = decode(&encode(&legacy), &path()).expect("decode legacy StoreMeta:1");
        assert_eq!(decoded, legacy);
    }
}
