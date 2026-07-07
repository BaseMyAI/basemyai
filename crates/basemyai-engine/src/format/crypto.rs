// SPDX-License-Identifier: BUSL-1.1
//! On-disk layouts of the three encryption-at-rest artifacts (ADR-030):
//! the `crypto.meta` key-wrap file, the per-record WAL envelope and the
//! whole-file SST envelope.
//!
//! This module only does *encoding*: bytes in, bytes out. No cryptography
//! happens here — sealing/opening (AEAD) and key derivation live in
//! [`crate::crypto`]; file I/O stays in `crate::store::{wal,sst}` and
//! `crate::crypto`. Same split as [`super::wal`]/[`super::sst`] vs
//! `store::{wal,sst}`.
//!
//! ## `crypto.meta` (`CryptoMeta:1` in `format.lock`)
//!
//! The single per-store key-wrap record (ADR-030 §2): the user key never
//! encrypts data — it derives a KEK that seals a random 32-byte DEK, and
//! this file holds that seal. Layout (integers little-endian):
//!
//! ```text
//! magic:       u32   = CRYPTO_META_MAGIC
//! version:     u16   = CRYPTO_META_VERSION
//! salt:        [u8; 16]     per-store random KEK-derivation salt
//! wrap_nonce:  [u8; 24]     XChaCha20 nonce of the DEK seal
//! wrapped_len: u32          length of the sealed DEK (32 + 16-byte tag)
//! wrapped_dek: [u8; wrapped_len]
//! crc32:       u32   over every byte above (magic..wrapped_dek)
//! ```
//!
//! The trailing crc32 is deliberately *in addition to* the Poly1305 tag
//! inside `wrapped_dek`: it lets `decode` distinguish a structurally
//! corrupt file (`CorruptCryptoMeta`) from an intact file whose seal fails
//! to open under the supplied key (`WrongEncryptionKey`, raised by the
//! caller in [`crate::crypto`]) — two very different diagnoses for a user.
//!
//! ## WAL envelope (`WalEnvelope:1`)
//!
//! In an encrypted store, each plain WAL record ([`super::wal`]'s bytes,
//! batches included — a batch is already a single outer record) is sealed
//! into one envelope (ADR-030 §3):
//!
//! ```text
//! magic:      u32   = WAL_ENVELOPE_MAGIC
//! version:    u16   = WAL_ENVELOPE_VERSION
//! nonce:      [u8; 24]
//! ct_len:     u32
//! ciphertext: [u8; ct_len]   sealed plain WAL-record bytes (+16-byte tag)
//! crc32 — none: the Poly1305 tag authenticates strictly more than a crc32
//! would, and by envelope-decode time the key is already verified against
//! crypto.meta, so an AEAD failure is unambiguously corruption.
//! ```
//!
//! `decode_envelope` mirrors `wal::decode`'s torn-tail contract exactly:
//! `Ok(None)` for an incomplete trailing envelope (expected crash shape,
//! replay stops silently), `Err` only for a fully-buffered envelope that is
//! structurally impossible.
//!
//! ## SST envelope (`SstEnvelope:1`)
//!
//! The whole plain SST body ([`super::sst`]'s bytes) sealed as one unit —
//! adequate because the store reads SSTs whole (ADR-025, no block reads):
//!
//! ```text
//! magic:      u32   = SST_ENVELOPE_MAGIC
//! version:    u16   = SST_ENVELOPE_VERSION
//! nonce:      [u8; 24]
//! ciphertext: rest of file    sealed plain SST-file bytes (+16-byte tag)
//! ```

use std::path::Path;

use super::checksum::crc32;
use crate::error::{EngineError, Result};

pub const CRYPTO_META_MAGIC: u32 = 0x424B_4559; // b"BKEY" (LE bytes: "YEKB")
pub const CRYPTO_META_VERSION: u16 = 1;

pub const WAL_ENVELOPE_MAGIC: u32 = 0x4257_4C45; // b"BWLE"
pub const WAL_ENVELOPE_VERSION: u16 = 1;

pub const SST_ENVELOPE_MAGIC: u32 = 0x4253_5345; // b"BSSE"
pub const SST_ENVELOPE_VERSION: u16 = 1;

/// Per-store KEK-derivation salt length (ADR-030 §1).
pub const SALT_LEN: usize = 16;
/// XChaCha20-Poly1305 nonce length.
pub const NONCE_LEN: usize = 24;

/// Canonical wire-format spec of `crypto.meta`, hashed into `format.lock`.
pub fn crypto_meta_spec() -> super::FormatSpec {
    super::FormatSpec {
        name: "CryptoMeta",
        version: CRYPTO_META_VERSION,
        fields: &[
            ("magic", "u32"),
            ("version", "u16"),
            ("salt", "bytes(16)"),
            ("wrap_nonce", "bytes(24)"),
            ("wrapped_len", "u32"),
            ("wrapped_dek", "bytes(wrapped_len)"),
            ("crc32", "u32"),
        ],
    }
}

/// Canonical wire-format spec of the WAL envelope, hashed into `format.lock`.
pub fn wal_envelope_spec() -> super::FormatSpec {
    super::FormatSpec {
        name: "WalEnvelope",
        version: WAL_ENVELOPE_VERSION,
        fields: &[
            ("magic", "u32"),
            ("version", "u16"),
            ("nonce", "bytes(24)"),
            ("ct_len", "u32"),
            ("ciphertext", "bytes(ct_len)"),
        ],
    }
}

/// Canonical wire-format spec of the SST envelope, hashed into `format.lock`.
pub fn sst_envelope_spec() -> super::FormatSpec {
    super::FormatSpec {
        name: "SstEnvelope",
        version: SST_ENVELOPE_VERSION,
        fields: &[
            ("magic", "u32"),
            ("version", "u16"),
            ("nonce", "bytes(24)"),
            ("ciphertext", "bytes(eof)"),
        ],
    }
}

/// Decoded `crypto.meta` contents (the seal itself — opening it is
/// [`crate::crypto`]'s job).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CryptoMeta {
    pub salt: [u8; SALT_LEN],
    pub wrap_nonce: [u8; NONCE_LEN],
    pub wrapped_dek: Vec<u8>,
}

impl CryptoMeta {
    /// The additional authenticated data binding the DEK seal to this
    /// file's header (magic, version, salt) — re-derived identically at
    /// encode and decode time, so a header spliced from another store
    /// fails the AEAD open even if the sealed bytes are intact.
    #[must_use]
    pub fn wrap_aad(&self) -> Vec<u8> {
        let mut aad = Vec::with_capacity(4 + 2 + SALT_LEN);
        aad.extend_from_slice(&CRYPTO_META_MAGIC.to_le_bytes());
        aad.extend_from_slice(&CRYPTO_META_VERSION.to_le_bytes());
        aad.extend_from_slice(&self.salt);
        aad
    }
}

const CRYPTO_META_HEADER_LEN: usize = 4 + 2 + SALT_LEN + NONCE_LEN + 4;
const CRC_LEN: usize = 4;

/// Encodes a `crypto.meta` file body.
#[must_use]
pub fn encode_crypto_meta(meta: &CryptoMeta) -> Vec<u8> {
    let mut buf = Vec::with_capacity(CRYPTO_META_HEADER_LEN + meta.wrapped_dek.len() + CRC_LEN);
    buf.extend_from_slice(&CRYPTO_META_MAGIC.to_le_bytes());
    buf.extend_from_slice(&CRYPTO_META_VERSION.to_le_bytes());
    buf.extend_from_slice(&meta.salt);
    buf.extend_from_slice(&meta.wrap_nonce);
    buf.extend_from_slice(&(meta.wrapped_dek.len() as u32).to_le_bytes());
    buf.extend_from_slice(&meta.wrapped_dek);
    let crc = crc32(&buf);
    buf.extend_from_slice(&crc.to_le_bytes());
    buf
}

/// Decodes a `crypto.meta` file body. Any structural problem (truncation,
/// bad magic, bad checksum, wire-controlled length exceeding the buffer) is
/// [`EngineError::CorruptCryptoMeta`] — never a panic (N2/N3 fuzzing
/// discipline: lengths are bounded against the real buffer before any
/// allocation or slice).
pub fn decode_crypto_meta(buf: &[u8], path: &Path) -> Result<CryptoMeta> {
    let corrupt = |reason: String| EngineError::CorruptCryptoMeta {
        path: path.to_path_buf(),
        reason,
    };

    if buf.len() < CRYPTO_META_HEADER_LEN + CRC_LEN {
        return Err(corrupt("file shorter than fixed header + trailing crc32".to_string()));
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
    if magic != CRYPTO_META_MAGIC {
        return Err(corrupt("bad magic".to_string()));
    }
    let version = u16::from_le_bytes(buf[4..6].try_into().expect("slice is exactly 2 bytes"));
    if version != CRYPTO_META_VERSION {
        return Err(EngineError::UnsupportedFormatVersion {
            path: path.to_path_buf(),
            expected: CRYPTO_META_VERSION,
            found: version,
        });
    }
    let mut pos = 6;
    let salt: [u8; SALT_LEN] = buf[pos..pos + SALT_LEN].try_into().expect("slice is exactly SALT_LEN");
    pos += SALT_LEN;
    let wrap_nonce: [u8; NONCE_LEN] = buf[pos..pos + NONCE_LEN]
        .try_into()
        .expect("slice is exactly NONCE_LEN");
    pos += NONCE_LEN;
    let wrapped_len = u32::from_le_bytes(buf[pos..pos + 4].try_into().expect("slice is exactly 4 bytes")) as usize;
    pos += 4;
    if wrapped_len != crc_at - pos {
        return Err(corrupt(format!(
            "wrapped_len {wrapped_len} does not match the {} bytes actually present",
            crc_at - pos
        )));
    }
    let wrapped_dek = buf[pos..crc_at].to_vec();
    Ok(CryptoMeta {
        salt,
        wrap_nonce,
        wrapped_dek,
    })
}

const WAL_ENVELOPE_HEADER_LEN: usize = 4 + 2 + NONCE_LEN + 4;

/// One decoded WAL envelope: `(nonce, ciphertext, consumed_len)`.
pub type WalEnvelopeRef<'a> = ([u8; NONCE_LEN], &'a [u8], usize);

/// Encodes one WAL envelope around already-sealed ciphertext.
#[must_use]
pub fn encode_wal_envelope(nonce: &[u8; NONCE_LEN], ciphertext: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(WAL_ENVELOPE_HEADER_LEN + ciphertext.len());
    buf.extend_from_slice(&WAL_ENVELOPE_MAGIC.to_le_bytes());
    buf.extend_from_slice(&WAL_ENVELOPE_VERSION.to_le_bytes());
    buf.extend_from_slice(nonce);
    buf.extend_from_slice(&(ciphertext.len() as u32).to_le_bytes());
    buf.extend_from_slice(ciphertext);
    buf
}

/// The AAD every WAL-envelope seal is bound to (magic + version).
#[must_use]
pub fn wal_envelope_aad() -> [u8; 6] {
    let mut aad = [0u8; 6];
    aad[0..4].copy_from_slice(&WAL_ENVELOPE_MAGIC.to_le_bytes());
    aad[4..6].copy_from_slice(&WAL_ENVELOPE_VERSION.to_le_bytes());
    aad
}

/// Decodes exactly one WAL envelope from the front of `buf`.
///
/// Same contract as [`super::wal::decode`]: `Ok(Some((nonce, ciphertext,
/// consumed)))` on a complete envelope, `Ok(None)` if `buf` does not yet
/// hold a full envelope (torn trailing write — the replay loop stops
/// silently), `Err` only for a version this build does not understand.
/// There is no checksum at this layer — the Poly1305 tag inside
/// `ciphertext` is verified by the caller when opening the seal.
pub fn decode_wal_envelope<'a>(buf: &'a [u8], path: &Path) -> Result<Option<WalEnvelopeRef<'a>>> {
    if buf.len() < WAL_ENVELOPE_HEADER_LEN {
        return Ok(None);
    }
    let magic = u32::from_le_bytes(buf[0..4].try_into().expect("slice is exactly 4 bytes"));
    if magic != WAL_ENVELOPE_MAGIC {
        return Ok(None);
    }
    let version = u16::from_le_bytes(buf[4..6].try_into().expect("slice is exactly 2 bytes"));
    if version != WAL_ENVELOPE_VERSION {
        return Err(EngineError::UnsupportedFormatVersion {
            path: path.to_path_buf(),
            expected: WAL_ENVELOPE_VERSION,
            found: version,
        });
    }
    let mut pos = 6;
    let nonce: [u8; NONCE_LEN] = buf[pos..pos + NONCE_LEN]
        .try_into()
        .expect("slice is exactly NONCE_LEN");
    pos += NONCE_LEN;
    let ct_len = u32::from_le_bytes(buf[pos..pos + 4].try_into().expect("slice is exactly 4 bytes")) as usize;
    pos += 4;
    let Some(end) = pos.checked_add(ct_len) else {
        return Ok(None);
    };
    if buf.len() < end {
        return Ok(None);
    }
    Ok(Some((nonce, &buf[pos..end], end)))
}

/// Encodes a whole-file SST envelope around already-sealed ciphertext.
#[must_use]
pub fn encode_sst_envelope(nonce: &[u8; NONCE_LEN], ciphertext: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(4 + 2 + NONCE_LEN + ciphertext.len());
    buf.extend_from_slice(&SST_ENVELOPE_MAGIC.to_le_bytes());
    buf.extend_from_slice(&SST_ENVELOPE_VERSION.to_le_bytes());
    buf.extend_from_slice(nonce);
    buf.extend_from_slice(ciphertext);
    buf
}

/// The AAD every SST-envelope seal is bound to (magic + version).
#[must_use]
pub fn sst_envelope_aad() -> [u8; 6] {
    let mut aad = [0u8; 6];
    aad[0..4].copy_from_slice(&SST_ENVELOPE_MAGIC.to_le_bytes());
    aad[4..6].copy_from_slice(&SST_ENVELOPE_VERSION.to_le_bytes());
    aad
}

/// Decodes a whole-file SST envelope: `(nonce, ciphertext)`. Unlike the WAL
/// envelope there is no torn-tail tolerance — an SST is only ever read after
/// its crash-safe rename, so any structural problem is genuine corruption
/// ([`EngineError::CorruptSst`], same variant its plaintext sibling uses).
pub fn decode_sst_envelope<'a>(buf: &'a [u8], path: &Path) -> Result<([u8; NONCE_LEN], &'a [u8])> {
    let corrupt = |reason: &str| EngineError::CorruptSst {
        path: path.to_path_buf(),
        reason: reason.to_string(),
    };
    if buf.len() < 4 + 2 + NONCE_LEN {
        return Err(corrupt("file shorter than the fixed envelope header"));
    }
    let magic = u32::from_le_bytes(buf[0..4].try_into().expect("slice is exactly 4 bytes"));
    if magic != SST_ENVELOPE_MAGIC {
        return Err(corrupt("bad envelope magic"));
    }
    let version = u16::from_le_bytes(buf[4..6].try_into().expect("slice is exactly 2 bytes"));
    if version != SST_ENVELOPE_VERSION {
        return Err(EngineError::UnsupportedFormatVersion {
            path: path.to_path_buf(),
            expected: SST_ENVELOPE_VERSION,
            found: version,
        });
    }
    let nonce: [u8; NONCE_LEN] = buf[6..6 + NONCE_LEN].try_into().expect("slice is exactly NONCE_LEN");
    Ok((nonce, &buf[6 + NONCE_LEN..]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn path() -> PathBuf {
        PathBuf::from("test.crypto")
    }

    fn sample_meta() -> CryptoMeta {
        CryptoMeta {
            salt: [7u8; SALT_LEN],
            wrap_nonce: [9u8; NONCE_LEN],
            wrapped_dek: vec![0xAB; 48],
        }
    }

    #[test]
    fn crypto_meta_roundtrips() {
        let meta = sample_meta();
        let bytes = encode_crypto_meta(&meta);
        let decoded = decode_crypto_meta(&bytes, &path()).expect("decode ok");
        assert_eq!(decoded, meta);
    }

    #[test]
    fn crypto_meta_bit_flip_is_corrupt_error() {
        let mut bytes = encode_crypto_meta(&sample_meta());
        bytes[10] ^= 0xFF;
        let err = decode_crypto_meta(&bytes, &path()).expect_err("checksum should fail");
        assert!(matches!(err, EngineError::CorruptCryptoMeta { .. }));
    }

    #[test]
    fn crypto_meta_truncation_is_corrupt_error() {
        let bytes = encode_crypto_meta(&sample_meta());
        for cut in 0..bytes.len() {
            let err = decode_crypto_meta(&bytes[..cut], &path()).expect_err("truncated file is corrupt");
            assert!(
                matches!(err, EngineError::CorruptCryptoMeta { .. }),
                "unexpected error at cut={cut}: {err}"
            );
        }
    }

    #[test]
    fn crypto_meta_lying_wrapped_len_is_corrupt_error() {
        // A wrapped_len claiming more bytes than actually present, with the
        // crc32 recomputed so the checksum gate doesn't short-circuit first.
        let meta = sample_meta();
        let mut bytes = encode_crypto_meta(&meta);
        let len_at = 4 + 2 + SALT_LEN + NONCE_LEN;
        bytes[len_at..len_at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        let crc_at = bytes.len() - 4;
        let crc = crate::format::checksum::crc32(&bytes[..crc_at]);
        bytes[crc_at..].copy_from_slice(&crc.to_le_bytes());
        let err = decode_crypto_meta(&bytes, &path()).expect_err("lying length is corrupt");
        assert!(matches!(err, EngineError::CorruptCryptoMeta { .. }));
    }

    #[test]
    fn crypto_meta_aad_binds_header_and_salt() {
        let a = sample_meta();
        let mut b = sample_meta();
        b.salt = [8u8; SALT_LEN];
        assert_ne!(a.wrap_aad(), b.wrap_aad());
    }

    #[test]
    fn wal_envelope_roundtrips() {
        let nonce = [3u8; NONCE_LEN];
        let ct = b"sealed bytes".to_vec();
        let bytes = encode_wal_envelope(&nonce, &ct);
        let (got_nonce, got_ct, consumed) = decode_wal_envelope(&bytes, &path())
            .expect("decode ok")
            .expect("full envelope");
        assert_eq!(consumed, bytes.len());
        assert_eq!(got_nonce, nonce);
        assert_eq!(got_ct, ct.as_slice());
    }

    #[test]
    fn wal_envelope_torn_tail_is_none_not_error() {
        let bytes = encode_wal_envelope(&[3u8; NONCE_LEN], b"sealed bytes");
        for cut in 1..bytes.len() {
            let result = decode_wal_envelope(&bytes[..cut], &path()).expect("torn tail is not an error");
            assert!(result.is_none(), "expected None at cut={cut}");
        }
    }

    #[test]
    fn wal_envelope_lying_ct_len_is_none_not_panic() {
        // ct_len claiming u32::MAX bytes in a short buffer must read as an
        // incomplete envelope (the bytes could still be in flight), never
        // panic on a slice or overflow.
        let mut bytes = encode_wal_envelope(&[3u8; NONCE_LEN], b"x");
        let len_at = 4 + 2 + NONCE_LEN;
        bytes[len_at..len_at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        let result = decode_wal_envelope(&bytes, &path()).expect("lying length reads as incomplete");
        assert!(result.is_none());
    }

    #[test]
    fn wal_envelopes_decode_in_sequence() {
        let mut buf = encode_wal_envelope(&[1u8; NONCE_LEN], b"first");
        buf.extend(encode_wal_envelope(&[2u8; NONCE_LEN], b"second"));
        let (_, first_ct, consumed) = decode_wal_envelope(&buf, &path())
            .expect("decode ok")
            .expect("envelope");
        assert_eq!(first_ct, b"first");
        let (_, second_ct, _) = decode_wal_envelope(&buf[consumed..], &path())
            .expect("decode ok")
            .expect("envelope");
        assert_eq!(second_ct, b"second");
    }

    #[test]
    fn sst_envelope_roundtrips() {
        let nonce = [5u8; NONCE_LEN];
        let ct = b"sealed sst".to_vec();
        let bytes = encode_sst_envelope(&nonce, &ct);
        let (got_nonce, got_ct) = decode_sst_envelope(&bytes, &path()).expect("decode ok");
        assert_eq!(got_nonce, nonce);
        assert_eq!(got_ct, ct.as_slice());
    }

    #[test]
    fn sst_envelope_bad_magic_is_corrupt_error() {
        // A plaintext SST read in encrypted mode lands here: its magic
        // differs, and the diagnosis must be loud, not a silent skip.
        let plain = crate::format::sst::encode(&[]);
        let err = decode_sst_envelope(&plain, &path()).expect_err("plaintext file is not an envelope");
        assert!(matches!(err, EngineError::CorruptSst { .. }));
    }

    #[test]
    fn envelope_aads_are_distinct_per_artifact() {
        // A WAL ciphertext replayed as an SST body (or vice versa) must fail
        // the AEAD open — the two AADs differing is what guarantees it.
        assert_ne!(wal_envelope_aad(), sst_envelope_aad());
    }
}
