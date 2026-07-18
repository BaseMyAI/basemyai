// SPDX-License-Identifier: BUSL-1.1
//! On-disk layouts of the three encryption-at-rest artifacts (ADR-030):
//! the `crypto.meta` key-wrap file, the per-record WAL envelope and the
//! per-section block-based-SST envelope (`EncryptedSstBlock`, ADR-039 §3).
//!
//! This module only does *encoding*: bytes in, bytes out. No cryptography
//! happens here — sealing/opening (AEAD) and key derivation live in
//! [`crate::crypto`]; file I/O stays in `crate::store::{wal,sst_block}` and
//! `crate::crypto`. Same split as [`super::wal`]/[`super::sst_block`] vs
//! `store::{wal,sst_block}`.
//!
//! ## `crypto.meta` (`CryptoMeta:1` and `CryptoMetaV2:2` in `format.lock`)
//!
//! The single per-store key-wrap record (ADR-030 §2): the user key never
//! encrypts data — it derives a KEK that seals a random 32-byte DEK, and
//! this file holds that seal. Layout (integers little-endian):
//!
//! ```text
//! magic:       u32   = CRYPTO_META_MAGIC
//! version:     u16   = 1
//! salt:        [u8; 16]     per-store random KEK-derivation salt
//! wrap_nonce:  [u8; 24]     XChaCha20 nonce of the DEK seal
//! wrapped_len: u32   = 48   length of the sealed 32-byte DEK + 16-byte tag
//! wrapped_dek: [u8; wrapped_len]
//! crc32:       u32   over every byte above (magic..wrapped_dek)
//! ```
//!
//! `CryptoMeta:1` remains a supported raw-key decoder forever: its bytes and
//! its wrap AAD are never reinterpreted. New stores use the additive v2
//! layout below. A v2 raw-key record carries only `kdf_mode = 0`; a v2
//! passphrase record carries its complete persisted Argon2id profile:
//!
//! ```text
//! magic:       u32   = CRYPTO_META_MAGIC
//! version:     u16   = 2
//! generation_id:u64   root layout = 0; future full rotations increment it
//! salt:        [u8; 16]     per-store SHA-256 KEK-derivation salt
//! wrap_nonce:  [u8; 24]     XChaCha20 nonce of the DEK seal
//! wrapped_len: u32   = 48
//! wrapped_dek: [u8; wrapped_len]
//! kdf_mode:    u8           0 = RawKey, 1 = Argon2id
//! # only when kdf_mode == 1:
//! kdf_salt:    [u8; 16]     independent Argon2id salt
//! argon2_m:    u32          KiB
//! argon2_t:    u32
//! argon2_p:    u32
//! crc32:       u32          over every byte above (magic..KDF fields)
//! ```
//!
//! Its wrap AAD is `magic || version || generation_id || salt || kdf_mode ||
//! kdf fields`. This explicitly authenticates the KDF selector, cost profile
//! and generation as well as binding the DEK wrap to this exact metadata
//! record (ADR-042).
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
//! ## `EncryptedSstBlock` (`EncryptedSstBlock:1`, ADR-039 §3)
//!
//! Superseded the whole-file `SstEnvelope:1` (ADR-030 §3's original
//! block-based-SST-format grant: "si un futur ADR introduit un index de
//! blocs, le chiffrement par bloc sera un nouveau format versionné"). Every
//! section of a block-based SST *except* the header — each data block, the
//! block index, the bloom filter, the footer — is sealed individually as one
//! of these envelopes:
//!
//! ```text
//! magic:      u32   = ENCRYPTED_SST_BLOCK_MAGIC
//! version:    u16   = ENCRYPTED_SST_BLOCK_VERSION
//! nonce:      [u8; 24]
//! ct_len:     u32
//! ciphertext: [u8; ct_len]   sealed plain section bytes (+16-byte tag)
//! ```
//!
//! `SstHeader` is never wrapped in this envelope — it stays plaintext even
//! in an encrypted store (see `format::sst_block`'s module doc: it is the
//! bootstrap record every other section's AAD needs `sst_id` from).
//!
//! **AAD** (the anti-permutation binding ADR-039 §3 requires): `domain
//! (magic ‖ version) ‖ sst_id ‖ section_type ‖ section_no` — see
//! [`SstSectionType`] and [`encrypted_sst_block_aad`]. A block moved between
//! two SSTs (different `sst_id`) or reordered within one (different
//! `section_no`) fails its Poly1305 tag even though the block itself is
//! individually intact.
//!
//! Same no-torn-tail-tolerance contract the superseded whole-file
//! `SstEnvelope:1` carried (never [`decode_wal_envelope`]'s `Ok(None)`
//! tolerance): every section is read via an already-known offset/length
//! (from the footer or the block index), never mid-stream, so any structural
//! problem is genuine corruption, not a torn write in flight.

use std::fmt;
use std::path::Path;

use super::checksum::crc32;
use crate::crypto::{Nonce, Salt};
use crate::error::{EngineError, Result};

pub(crate) const CRYPTO_META_MAGIC: u32 = 0x424B_4559; // b"BKEY" (LE bytes: "YEKB")
pub(crate) const CRYPTO_META_V1_VERSION: u16 = 1;
pub(crate) const CRYPTO_META_V2_VERSION: u16 = 2;

pub(crate) const WAL_ENVELOPE_MAGIC: u32 = 0x4257_4C45; // b"BWLE"
pub(crate) const WAL_ENVELOPE_VERSION: u16 = 1;

pub(crate) const ENCRYPTED_SST_BLOCK_MAGIC: u32 = 0x4253_4245; // b"BSBE"
pub(crate) const ENCRYPTED_SST_BLOCK_VERSION: u16 = 1;

/// Per-store KEK-derivation salt length (ADR-030 §1).
pub(crate) const SALT_LEN: usize = 16;
/// Argon2id's independent per-store salt length (ADR-042).
pub(crate) const KDF_SALT_LEN: usize = 16;
/// Maximum Argon2id profile accepted from persisted, unauthenticated
/// metadata. PR3 only writes the RFC 9106 second profile; keeping the
/// accepted ceiling at that profile prevents pre-authentication memory/CPU
/// amplification while leaving room to lower costs on constrained targets.
pub(crate) const ARGON2_MAX_M_COST_KIB: u32 = 65_536;
pub(crate) const ARGON2_MAX_T_COST: u32 = 3;
pub(crate) const ARGON2_MAX_P_COST: u32 = 4;
/// XChaCha20-Poly1305 nonce length.
pub(crate) const NONCE_LEN: usize = 24;
/// Poly1305 authentication tag length appended to every AEAD ciphertext —
/// named here (rather than left as a "+16-byte tag" doc-comment aside) so
/// [`encrypted_sst_block_sealed_len`] can compute a section's exact sealed
/// size without duplicating the constant.
pub(crate) const AEAD_TAG_LEN: usize = 16;

/// Canonical immutable wire-format spec of legacy `CryptoMeta:1`.
pub(crate) fn crypto_meta_v1_spec() -> super::FormatSpec {
    super::FormatSpec {
        name: "CryptoMeta",
        version: CRYPTO_META_V1_VERSION,
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

/// Canonical additive wire-format spec of `CryptoMeta:2`.
pub(crate) fn crypto_meta_v2_spec() -> super::FormatSpec {
    super::FormatSpec {
        name: "CryptoMetaV2",
        version: CRYPTO_META_V2_VERSION,
        fields: &[
            ("magic", "u32"),
            ("version", "u16"),
            ("generation_id", "u64"),
            ("salt", "bytes(16)"),
            ("wrap_nonce", "bytes(24)"),
            ("wrapped_len", "u32"),
            ("wrapped_dek", "bytes(wrapped_len)"),
            ("kdf_mode", "u8"),
            ("kdf_salt", "bytes(16)?"),
            ("argon2_m_kib", "u32?"),
            ("argon2_t_cost", "u32?"),
            ("argon2_p", "u32?"),
            ("crc32", "u32"),
        ],
    }
}

/// Canonical wire-format spec of the WAL envelope, hashed into `format.lock`.
pub(crate) fn wal_envelope_spec() -> super::FormatSpec {
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

/// Canonical wire-format spec of `EncryptedSstBlock`, hashed into `format.lock`.
pub(crate) fn encrypted_sst_block_spec() -> super::FormatSpec {
    super::FormatSpec {
        name: "EncryptedSstBlock",
        version: ENCRYPTED_SST_BLOCK_VERSION,
        fields: &[
            ("magic", "u32"),
            ("version", "u16"),
            ("nonce", "bytes(24)"),
            ("ct_len", "u32"),
            ("ciphertext", "bytes(ct_len)"),
        ],
    }
}

/// One section of a block-based SST that gets its own `EncryptedSstBlock`
/// envelope (ADR-039 §3). Deliberately excludes the header — see the module
/// doc for why it stays plaintext.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SstSectionType {
    Data,
    Index,
    Bloom,
    Footer,
}

impl SstSectionType {
    const fn tag(self) -> u8 {
        match self {
            Self::Data => 0,
            Self::Index => 1,
            Self::Bloom => 2,
            Self::Footer => 3,
        }
    }
}

/// Decoded `crypto.meta` contents (the seal itself — opening it is
/// [`crate::crypto`]'s job).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Argon2Params {
    pub(crate) salt: [u8; KDF_SALT_LEN],
    pub(crate) m_cost_kib: u32,
    pub(crate) t_cost: u32,
    pub(crate) p_cost: u32,
}

impl fmt::Debug for Argon2Params {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Argon2Params")
            .field("salt", &"***")
            .field("m_cost_kib", &self.m_cost_kib)
            .field("t_cost", &self.t_cost)
            .field("p_cost", &self.p_cost)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KdfMode {
    RawKey,
    Argon2id(Argon2Params),
}

impl KdfMode {
    pub(crate) const fn tag(self) -> u8 {
        match self {
            Self::RawKey => 0,
            Self::Argon2id(_) => 1,
        }
    }
}

/// Decoded `crypto.meta` contents (the seal itself — opening it is
/// [`crate::crypto`]'s job).
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct CryptoMeta {
    pub(crate) version: u16,
    /// Monotonic DEK generation for future full rotation. Root-layout stores
    /// are generation zero; v1 predates this field and decodes as zero.
    pub(crate) generation_id: u64,
    pub(crate) salt: Salt,
    pub(crate) kdf_mode: KdfMode,
    pub(crate) wrap_nonce: Nonce,
    pub(crate) wrapped_dek: Vec<u8>,
}

impl fmt::Debug for CryptoMeta {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CryptoMeta")
            .field("version", &self.version)
            .field("generation_id", &self.generation_id)
            .field("salt", &self.salt)
            .field("kdf_mode", &self.kdf_mode)
            .field("wrap_nonce", &self.wrap_nonce)
            .field("wrapped_dek", &format!("<{} bytes>", self.wrapped_dek.len()))
            .finish()
    }
}

impl CryptoMeta {
    /// The additional authenticated data binding the DEK seal to this
    /// file's authenticated header — re-derived identically at
    /// encode and decode time, so a header spliced from another store
    /// fails the AEAD open even if the sealed bytes are intact.
    #[must_use]
    pub(crate) fn wrap_aad(&self) -> Vec<u8> {
        let mut aad = Vec::with_capacity(4 + 2 + 8 + SALT_LEN + 1 + KDF_SALT_LEN + 12);
        aad.extend_from_slice(&CRYPTO_META_MAGIC.to_le_bytes());
        aad.extend_from_slice(&self.version.to_le_bytes());
        if self.version == CRYPTO_META_V2_VERSION {
            aad.extend_from_slice(&self.generation_id.to_le_bytes());
        }
        aad.extend_from_slice(self.salt.as_bytes());
        if self.version == CRYPTO_META_V2_VERSION {
            aad.push(self.kdf_mode.tag());
            if let KdfMode::Argon2id(params) = self.kdf_mode {
                aad.extend_from_slice(&params.salt);
                aad.extend_from_slice(&params.m_cost_kib.to_le_bytes());
                aad.extend_from_slice(&params.t_cost.to_le_bytes());
                aad.extend_from_slice(&params.p_cost.to_le_bytes());
            }
        }
        aad
    }
}

const CRYPTO_META_HEADER_LEN: usize = 4 + 2 + SALT_LEN + NONCE_LEN + 4;
const CRC_LEN: usize = 4;
/// A `crypto.meta` always wraps exactly one 32-byte DEK under XChaCha20-
/// Poly1305, so its ciphertext is the DEK plus one authentication tag.
const WRAPPED_DEK_LEN: usize = 32 + AEAD_TAG_LEN;

/// Encodes a `crypto.meta` file body.
#[must_use]
pub(crate) fn encode_crypto_meta(meta: &CryptoMeta) -> Vec<u8> {
    let kdf_len = match meta.kdf_mode {
        KdfMode::RawKey => 1,
        KdfMode::Argon2id(_) => 1 + KDF_SALT_LEN + 12,
    };
    let mut buf = Vec::with_capacity(CRYPTO_META_HEADER_LEN + meta.wrapped_dek.len() + kdf_len + CRC_LEN);
    buf.extend_from_slice(&CRYPTO_META_MAGIC.to_le_bytes());
    buf.extend_from_slice(&meta.version.to_le_bytes());
    if meta.version == CRYPTO_META_V2_VERSION {
        buf.extend_from_slice(&meta.generation_id.to_le_bytes());
    }
    buf.extend_from_slice(meta.salt.as_bytes());
    buf.extend_from_slice(meta.wrap_nonce.as_bytes());
    buf.extend_from_slice(&(meta.wrapped_dek.len() as u32).to_le_bytes());
    buf.extend_from_slice(&meta.wrapped_dek);
    if meta.version == CRYPTO_META_V2_VERSION {
        buf.push(meta.kdf_mode.tag());
        if let KdfMode::Argon2id(params) = meta.kdf_mode {
            buf.extend_from_slice(&params.salt);
            buf.extend_from_slice(&params.m_cost_kib.to_le_bytes());
            buf.extend_from_slice(&params.t_cost.to_le_bytes());
            buf.extend_from_slice(&params.p_cost.to_le_bytes());
        }
    }
    let crc = crc32(&buf);
    buf.extend_from_slice(&crc.to_le_bytes());
    buf
}

/// Decodes a `crypto.meta` file body. Any structural problem (truncation,
/// bad magic, bad checksum, wire-controlled length exceeding the buffer) is
/// [`EngineError::CorruptCryptoMeta`] — never a panic (N2/N3 fuzzing
/// discipline: lengths are bounded against the real buffer before any
/// allocation or slice).
pub(crate) fn decode_crypto_meta(buf: &[u8], path: &Path) -> Result<CryptoMeta> {
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
    if version != CRYPTO_META_V1_VERSION && version != CRYPTO_META_V2_VERSION {
        return Err(EngineError::UnsupportedFormatVersion {
            path: path.to_path_buf(),
            expected: CRYPTO_META_V2_VERSION,
            found: version,
        });
    }
    let min_len = CRYPTO_META_HEADER_LEN
        + CRC_LEN
        + if version == CRYPTO_META_V2_VERSION {
            // v2's generation field plus its mandatory KDF selector.
            8 + 1
        } else {
            0
        };
    if buf.len() < min_len {
        return Err(corrupt(format!("v{version} file is shorter than its fixed header")));
    }
    let mut pos = 6;
    let generation_id = if version == CRYPTO_META_V2_VERSION {
        let generation_end = pos + 8;
        let generation_wire: [u8; 8] = buf
            .get(pos..generation_end)
            .ok_or_else(|| corrupt("v2 is missing generation_id".to_string()))?
            .try_into()
            .expect("slice is exactly 8 bytes");
        pos = generation_end;
        u64::from_le_bytes(generation_wire)
    } else {
        0
    };
    let salt_wire: [u8; SALT_LEN] = buf[pos..pos + SALT_LEN].try_into().expect("slice is exactly SALT_LEN");
    pos += SALT_LEN;
    let wrap_nonce_wire: [u8; NONCE_LEN] = buf[pos..pos + NONCE_LEN]
        .try_into()
        .expect("slice is exactly NONCE_LEN");
    pos += NONCE_LEN;
    let wrapped_len = u32::from_le_bytes(buf[pos..pos + 4].try_into().expect("slice is exactly 4 bytes")) as usize;
    pos += 4;
    if wrapped_len != WRAPPED_DEK_LEN {
        return Err(corrupt(format!(
            "wrapped_len must be {WRAPPED_DEK_LEN} bytes for a sealed DEK, got {wrapped_len}"
        )));
    }
    let kdf_min_len = usize::from(version == CRYPTO_META_V2_VERSION);
    if wrapped_len > crc_at.saturating_sub(pos + kdf_min_len) {
        return Err(corrupt(format!(
            "wrapped_len {wrapped_len} exceeds the {} bytes available before KDF fields",
            crc_at.saturating_sub(pos + kdf_min_len)
        )));
    }
    let wrapped_dek = buf[pos..pos + wrapped_len].to_vec();
    pos += wrapped_len;
    let kdf_mode = match version {
        CRYPTO_META_V1_VERSION => {
            if pos != crc_at {
                return Err(corrupt("legacy CryptoMeta:1 has trailing bytes".to_string()));
            }
            KdfMode::RawKey
        }
        CRYPTO_META_V2_VERSION => {
            let tag = *buf
                .get(pos)
                .ok_or_else(|| corrupt("v2 is missing kdf_mode".to_string()))?;
            pos += 1;
            match tag {
                0 => {
                    if pos != crc_at {
                        return Err(corrupt("v2 RawKey record has unexpected KDF fields".to_string()));
                    }
                    KdfMode::RawKey
                }
                1 => {
                    if crc_at - pos != KDF_SALT_LEN + 12 {
                        return Err(corrupt(format!(
                            "v2 Argon2id fields must be {} bytes, got {}",
                            KDF_SALT_LEN + 12,
                            crc_at - pos
                        )));
                    }
                    let salt: [u8; KDF_SALT_LEN] = buf[pos..pos + KDF_SALT_LEN]
                        .try_into()
                        .expect("slice is exactly KDF_SALT_LEN bytes");
                    pos += KDF_SALT_LEN;
                    let m_cost_kib =
                        u32::from_le_bytes(buf[pos..pos + 4].try_into().expect("slice is exactly 4 bytes"));
                    pos += 4;
                    let t_cost = u32::from_le_bytes(buf[pos..pos + 4].try_into().expect("slice is exactly 4 bytes"));
                    pos += 4;
                    let p_cost = u32::from_le_bytes(buf[pos..pos + 4].try_into().expect("slice is exactly 4 bytes"));
                    if m_cost_kib > ARGON2_MAX_M_COST_KIB || t_cost > ARGON2_MAX_T_COST || p_cost > ARGON2_MAX_P_COST {
                        return Err(corrupt(format!(
                            "Argon2id parameters exceed supported limits: m={m_cost_kib} KiB (max {ARGON2_MAX_M_COST_KIB}), t={t_cost} (max {ARGON2_MAX_T_COST}), p={p_cost} (max {ARGON2_MAX_P_COST})"
                        )));
                    }
                    KdfMode::Argon2id(Argon2Params {
                        salt,
                        m_cost_kib,
                        t_cost,
                        p_cost,
                    })
                }
                _ => return Err(corrupt(format!("unknown v2 kdf_mode {tag}"))),
            }
        }
        _ => unreachable!("version gate above accepts only v1/v2"),
    };
    Ok(CryptoMeta {
        version,
        generation_id,
        salt: Salt::from_wire(salt_wire),
        kdf_mode,
        wrap_nonce: Nonce::from_wire(wrap_nonce_wire),
        wrapped_dek,
    })
}

const WAL_ENVELOPE_HEADER_LEN: usize = 4 + 2 + NONCE_LEN + 4;

/// One decoded WAL envelope: `(nonce, ciphertext, consumed_len)`.
pub(crate) type WalEnvelopeRef<'a> = (Nonce, &'a [u8], usize);

/// Encodes one WAL envelope around already-sealed ciphertext.
#[must_use]
pub(crate) fn encode_wal_envelope(nonce: &Nonce, ciphertext: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(WAL_ENVELOPE_HEADER_LEN + ciphertext.len());
    buf.extend_from_slice(&WAL_ENVELOPE_MAGIC.to_le_bytes());
    buf.extend_from_slice(&WAL_ENVELOPE_VERSION.to_le_bytes());
    buf.extend_from_slice(nonce.as_bytes());
    buf.extend_from_slice(&(ciphertext.len() as u32).to_le_bytes());
    buf.extend_from_slice(ciphertext);
    buf
}

/// The AAD every WAL-envelope seal is bound to (magic + version).
#[must_use]
pub(crate) fn wal_envelope_aad() -> [u8; 6] {
    let mut aad = [0u8; 6];
    aad[0..4].copy_from_slice(&WAL_ENVELOPE_MAGIC.to_le_bytes());
    aad[4..6].copy_from_slice(&WAL_ENVELOPE_VERSION.to_le_bytes());
    aad
}

/// Decodes exactly one WAL envelope from the front of `buf`.
///
/// Same contract as [`super::wal::decode`]: `Ok(Some((nonce, ciphertext,
/// consumed)))` on a complete envelope, `Ok(None)` only if `buf` is a prefix
/// of an envelope that could still be in flight (torn trailing write — the
/// replay loop stops silently), `Err` for complete structurally impossible
/// headers or for a version this build does not understand. There is no
/// checksum at this layer — the Poly1305 tag inside `ciphertext` is verified
/// by the caller when opening the seal.
pub(crate) fn decode_wal_envelope<'a>(buf: &'a [u8], path: &Path) -> Result<Option<WalEnvelopeRef<'a>>> {
    let corrupt = |reason: &str| EngineError::CorruptWal {
        path: path.to_path_buf(),
        reason: reason.to_string(),
    };

    if buf.len() < 4 {
        return Ok(None);
    }
    let magic = u32::from_le_bytes(buf[0..4].try_into().expect("slice is exactly 4 bytes"));
    if magic != WAL_ENVELOPE_MAGIC {
        return Err(corrupt("bad WAL envelope magic"));
    }
    if buf.len() < WAL_ENVELOPE_HEADER_LEN {
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
    let nonce_wire: [u8; NONCE_LEN] = buf[pos..pos + NONCE_LEN]
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
    Ok(Some((Nonce::from_wire(nonce_wire), &buf[pos..end], end)))
}

const ENCRYPTED_SST_BLOCK_HEADER_LEN: usize = 4 + 2 + NONCE_LEN + 4;

/// Encodes one `EncryptedSstBlock` envelope around already-sealed ciphertext.
#[must_use]
pub(crate) fn encode_encrypted_sst_block(nonce: &Nonce, ciphertext: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(ENCRYPTED_SST_BLOCK_HEADER_LEN + ciphertext.len());
    buf.extend_from_slice(&ENCRYPTED_SST_BLOCK_MAGIC.to_le_bytes());
    buf.extend_from_slice(&ENCRYPTED_SST_BLOCK_VERSION.to_le_bytes());
    buf.extend_from_slice(nonce.as_bytes());
    buf.extend_from_slice(&(ciphertext.len() as u32).to_le_bytes());
    buf.extend_from_slice(ciphertext);
    buf
}

/// The exact on-disk length an `EncryptedSstBlock` envelope occupies for a
/// section whose *plaintext* is `plain_len` bytes — header framing plus the
/// plaintext length plus the Poly1305 tag. Used by the block-based-SST
/// reader (`store::sst_block`) to locate the sealed footer, whose plaintext
/// length ([`super::sst_block::SST_FOOTER_LEN`]) is fixed, so its sealed
/// on-disk length is too: the reader can seek to it from EOF without reading
/// anything else first, encrypted or not.
#[must_use]
pub(crate) fn encrypted_sst_block_sealed_len(plain_len: usize) -> usize {
    ENCRYPTED_SST_BLOCK_HEADER_LEN + plain_len + AEAD_TAG_LEN
}

/// The AAD every `EncryptedSstBlock` seal is bound to: domain (magic +
/// version) ‖ `sst_id` ‖ `section` ‖ `section_no` (ADR-039 §3). Binds a
/// sealed section to exactly one store, one SST generation and one position
/// within it — moving it anywhere else fails the Poly1305 tag even though
/// the bytes are individually intact.
#[must_use]
pub(crate) fn encrypted_sst_block_aad(sst_id: u64, section: SstSectionType, section_no: u32) -> Vec<u8> {
    let mut aad = Vec::with_capacity(4 + 2 + 8 + 1 + 4);
    aad.extend_from_slice(&ENCRYPTED_SST_BLOCK_MAGIC.to_le_bytes());
    aad.extend_from_slice(&ENCRYPTED_SST_BLOCK_VERSION.to_le_bytes());
    aad.extend_from_slice(&sst_id.to_le_bytes());
    aad.push(section.tag());
    aad.extend_from_slice(&section_no.to_le_bytes());
    aad
}

/// Decodes one `EncryptedSstBlock` envelope: `(nonce, ciphertext)`. Like the
/// whole-file SST envelope it superseded, there is no torn-tail tolerance —
/// every section is read via an offset/length already known from the footer
/// or block index, never mid-stream, so any structural problem is genuine
/// corruption ([`EngineError::CorruptEncryptedSstBlock`]).
pub(crate) fn decode_encrypted_sst_block<'a>(buf: &'a [u8], path: &Path) -> Result<(Nonce, &'a [u8])> {
    let corrupt = |reason: &str| EngineError::CorruptEncryptedSstBlock {
        path: path.to_path_buf(),
        reason: reason.to_string(),
    };
    if buf.len() < ENCRYPTED_SST_BLOCK_HEADER_LEN {
        return Err(corrupt("file shorter than the fixed envelope header"));
    }
    let magic = u32::from_le_bytes(buf[0..4].try_into().expect("slice is exactly 4 bytes"));
    if magic != ENCRYPTED_SST_BLOCK_MAGIC {
        return Err(corrupt("bad envelope magic"));
    }
    let version = u16::from_le_bytes(buf[4..6].try_into().expect("slice is exactly 2 bytes"));
    if version != ENCRYPTED_SST_BLOCK_VERSION {
        return Err(EngineError::UnsupportedEncryptedSstBlockVersion {
            path: path.to_path_buf(),
            expected: ENCRYPTED_SST_BLOCK_VERSION,
            found: version,
        });
    }
    let mut pos = 6;
    let nonce_wire: [u8; NONCE_LEN] = buf[pos..pos + NONCE_LEN]
        .try_into()
        .expect("slice is exactly NONCE_LEN");
    pos += NONCE_LEN;
    let ct_len = u32::from_le_bytes(buf[pos..pos + 4].try_into().expect("slice is exactly 4 bytes")) as usize;
    pos += 4;
    if ct_len != buf.len() - pos {
        return Err(corrupt("ct_len does not match the bytes actually present"));
    }
    Ok((Nonce::from_wire(nonce_wire), &buf[pos..]))
}

// Thin fuzz-only entry points (N11 §8.4): the three `decode_*` above stay
// `pub(crate)` — their return types (`CryptoMeta`, `Nonce`, `WalEnvelopeRef`)
// are deliberately crate-private (this module's own doc: crypto internals
// are guarded, only `crate::crypto` and this file touch them), and making
// the decoders themselves `pub` would leak those types into the public API
// (`private_interfaces`). These wrappers instead run the exact same decode
// and discard the result — everything a fuzz target needs (panic-freedom,
// no UB) without widening what `basemyai-engine` exposes.
#[doc(hidden)]
pub fn fuzz_decode_crypto_meta(buf: &[u8], path: &Path) {
    let _ = decode_crypto_meta(buf, path);
}

#[doc(hidden)]
pub fn fuzz_decode_wal_envelope(buf: &[u8], path: &Path) {
    let _ = decode_wal_envelope(buf, path);
}

#[doc(hidden)]
pub fn fuzz_decode_encrypted_sst_block(buf: &[u8], path: &Path) {
    let _ = decode_encrypted_sst_block(buf, path);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::test_support::DeterministicTestRng;
    use std::path::PathBuf;

    fn path() -> PathBuf {
        PathBuf::from("test.crypto")
    }

    fn sample_meta() -> CryptoMeta {
        let mut rng = DeterministicTestRng::new(0x7E57);
        CryptoMeta {
            version: CRYPTO_META_V2_VERSION,
            generation_id: 0,
            salt: Salt::generate_with(&mut rng),
            kdf_mode: KdfMode::RawKey,
            wrap_nonce: Nonce::generate_with(&mut rng),
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
        let len_at = 4 + 2 + 8 + SALT_LEN + NONCE_LEN;
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
        b.salt = Salt::generate_with(&mut DeterministicTestRng::new(0xBEEF));
        assert_ne!(a.wrap_aad(), b.wrap_aad());
    }

    #[test]
    fn legacy_crypto_meta_v1_still_roundtrips_as_raw_key() {
        let mut meta = sample_meta();
        meta.version = CRYPTO_META_V1_VERSION;
        let bytes = encode_crypto_meta(&meta);
        let decoded = decode_crypto_meta(&bytes, &path()).expect("decode legacy v1");
        assert_eq!(decoded, meta);
        assert_eq!(decoded.kdf_mode, KdfMode::RawKey);
    }

    #[test]
    fn legacy_crypto_meta_v1_fixture_stays_readable_without_reencoding() {
        // Deliberately build the historical v1 bytes directly instead of
        // calling `encode_crypto_meta`: this protects its reader contract
        // from accidental v2-only changes to the writer.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&CRYPTO_META_MAGIC.to_le_bytes());
        bytes.extend_from_slice(&CRYPTO_META_V1_VERSION.to_le_bytes());
        bytes.extend(0_u8..SALT_LEN as u8);
        bytes.extend(SALT_LEN as u8..(SALT_LEN + NONCE_LEN) as u8);
        bytes.extend_from_slice(&(WRAPPED_DEK_LEN as u32).to_le_bytes());
        bytes.extend(0x80_u8..0x80_u8 + WRAPPED_DEK_LEN as u8);
        let crc = crate::format::checksum::crc32(&bytes);
        bytes.extend_from_slice(&crc.to_le_bytes());

        let decoded = decode_crypto_meta(&bytes, &path()).expect("decode fixed v1 fixture");
        assert_eq!(decoded.version, CRYPTO_META_V1_VERSION);
        assert_eq!(decoded.generation_id, 0);
        assert_eq!(decoded.kdf_mode, KdfMode::RawKey);
        assert_eq!(decoded.salt.as_bytes(), &(0_u8..SALT_LEN as u8).collect::<Vec<_>>()[..]);
        assert_eq!(
            decoded.wrapped_dek,
            (0x80_u8..0x80_u8 + WRAPPED_DEK_LEN as u8).collect::<Vec<_>>()
        );
    }

    #[test]
    fn crypto_meta_v2_unknown_kdf_mode_with_valid_crc_is_corrupt() {
        let mut bytes = encode_crypto_meta(&sample_meta());
        let kdf_tag_at = CRYPTO_META_HEADER_LEN + 8 + WRAPPED_DEK_LEN;
        bytes[kdf_tag_at] = 0xFF;
        let crc_at = bytes.len() - CRC_LEN;
        let crc = crate::format::checksum::crc32(&bytes[..crc_at]);
        bytes[crc_at..].copy_from_slice(&crc.to_le_bytes());

        let err = decode_crypto_meta(&bytes, &path()).expect_err("unknown KDF tag is corrupt");
        assert!(matches!(err, EngineError::CorruptCryptoMeta { .. }));
    }

    #[test]
    fn crypto_meta_rejects_crc_valid_non_dek_wrap_lengths() {
        let mut bytes = encode_crypto_meta(&sample_meta());
        let len_at = 4 + 2 + 8 + SALT_LEN + NONCE_LEN;
        bytes[len_at..len_at + 4].copy_from_slice(&47_u32.to_le_bytes());
        let crc_at = bytes.len() - CRC_LEN;
        let crc = crate::format::checksum::crc32(&bytes[..crc_at]);
        bytes[crc_at..].copy_from_slice(&crc.to_le_bytes());

        let err = decode_crypto_meta(&bytes, &path()).expect_err("only a sealed DEK is valid");
        assert!(matches!(err, EngineError::CorruptCryptoMeta { .. }));
    }

    #[test]
    fn crypto_meta_v2_argon2id_roundtrips_and_aad_binds_the_profile() {
        let mut meta = sample_meta();
        meta.generation_id = 7;
        meta.kdf_mode = KdfMode::Argon2id(Argon2Params {
            salt: [0x5A; KDF_SALT_LEN],
            m_cost_kib: 65_536,
            t_cost: 3,
            p_cost: 4,
        });
        let bytes = encode_crypto_meta(&meta);
        let decoded = decode_crypto_meta(&bytes, &path()).expect("decode v2 Argon2id");
        assert_eq!(decoded, meta);

        let mut altered = decoded.clone();
        let KdfMode::Argon2id(mut params) = altered.kdf_mode else {
            panic!("test fixture must be Argon2id");
        };
        params.t_cost = 4;
        altered.kdf_mode = KdfMode::Argon2id(params);
        assert_ne!(meta.wrap_aad(), altered.wrap_aad());

        altered = decoded;
        altered.generation_id += 1;
        assert_ne!(meta.wrap_aad(), altered.wrap_aad());
    }

    #[test]
    fn crypto_meta_rejects_argon2id_costs_above_pre_auth_limits() {
        let profiles = [
            (ARGON2_MAX_M_COST_KIB + 1, ARGON2_MAX_T_COST, ARGON2_MAX_P_COST),
            (ARGON2_MAX_M_COST_KIB, ARGON2_MAX_T_COST + 1, ARGON2_MAX_P_COST),
            (ARGON2_MAX_M_COST_KIB, ARGON2_MAX_T_COST, ARGON2_MAX_P_COST + 1),
        ];
        for (m_cost_kib, t_cost, p_cost) in profiles {
            let mut meta = sample_meta();
            meta.kdf_mode = KdfMode::Argon2id(Argon2Params {
                salt: [0xA5; KDF_SALT_LEN],
                m_cost_kib,
                t_cost,
                p_cost,
            });
            let bytes = encode_crypto_meta(&meta);
            let err = decode_crypto_meta(&bytes, &path()).expect_err("excessive Argon2id cost must be rejected");
            assert!(matches!(err, EngineError::CorruptCryptoMeta { .. }));
        }
    }

    #[test]
    fn wal_envelope_roundtrips() {
        let nonce = Nonce::generate_with(&mut DeterministicTestRng::new(3));
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
        let nonce = Nonce::generate_with(&mut DeterministicTestRng::new(3));
        let bytes = encode_wal_envelope(&nonce, b"sealed bytes");
        for cut in 1..bytes.len() {
            let result = decode_wal_envelope(&bytes[..cut], &path()).expect("torn tail is not an error");
            assert!(result.is_none(), "expected None at cut={cut}");
        }
    }

    #[test]
    fn wal_envelope_bad_magic_is_corrupt_error() {
        let nonce = Nonce::generate_with(&mut DeterministicTestRng::new(3));
        let mut bytes = encode_wal_envelope(&nonce, b"sealed bytes");
        bytes[0] ^= 0xFF;
        let err = decode_wal_envelope(&bytes, &path()).expect_err("bad envelope magic is corrupt");
        assert!(matches!(err, EngineError::CorruptWal { .. }));
    }

    #[test]
    fn short_plaintext_wal_is_corrupt_envelope_not_torn_tail() {
        let bytes = crate::format::wal::encode(crate::format::wal::WalOp::Put, b"a", Some(b"1"));
        assert!(bytes.len() < WAL_ENVELOPE_HEADER_LEN);
        let err = decode_wal_envelope(&bytes, &path()).expect_err("plaintext WAL is not a torn envelope");
        assert!(matches!(err, EngineError::CorruptWal { .. }));
    }

    #[test]
    fn wal_envelope_lying_ct_len_is_none_not_panic() {
        // ct_len claiming u32::MAX bytes in a short buffer must read as an
        // incomplete envelope (the bytes could still be in flight), never
        // panic on a slice or overflow.
        let nonce = Nonce::generate_with(&mut DeterministicTestRng::new(3));
        let mut bytes = encode_wal_envelope(&nonce, b"x");
        let len_at = 4 + 2 + NONCE_LEN;
        bytes[len_at..len_at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        let result = decode_wal_envelope(&bytes, &path()).expect("lying length reads as incomplete");
        assert!(result.is_none());
    }

    #[test]
    fn wal_envelopes_decode_in_sequence() {
        let first_nonce = Nonce::generate_with(&mut DeterministicTestRng::new(1));
        let second_nonce = Nonce::generate_with(&mut DeterministicTestRng::new(2));
        let mut buf = encode_wal_envelope(&first_nonce, b"first");
        buf.extend(encode_wal_envelope(&second_nonce, b"second"));
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
    fn encrypted_sst_block_roundtrips() {
        let nonce = Nonce::generate_with(&mut DeterministicTestRng::new(5));
        let ct = b"sealed section".to_vec();
        let bytes = encode_encrypted_sst_block(&nonce, &ct);
        let (got_nonce, got_ct) = decode_encrypted_sst_block(&bytes, &path()).expect("decode ok");
        assert_eq!(got_nonce, nonce);
        assert_eq!(got_ct, ct.as_slice());
    }

    #[test]
    fn encrypted_sst_block_bad_magic_is_corrupt_error() {
        // A plaintext section read in encrypted mode lands here: its magic
        // differs, and the diagnosis must be loud, not a silent skip.
        let plain = crate::format::sst_block::encode_sst_data_block(&[]);
        let err = decode_encrypted_sst_block(&plain, &path()).expect_err("plaintext section is not an envelope");
        assert!(matches!(err, EngineError::CorruptEncryptedSstBlock { .. }));
    }

    #[test]
    fn encrypted_sst_block_truncation_is_corrupt_error_at_every_cut() {
        let nonce = Nonce::generate_with(&mut DeterministicTestRng::new(6));
        let bytes = encode_encrypted_sst_block(&nonce, b"sealed section");
        for cut in 0..bytes.len() {
            let err = decode_encrypted_sst_block(&bytes[..cut], &path()).expect_err("truncated envelope is corrupt");
            assert!(
                matches!(err, EngineError::CorruptEncryptedSstBlock { .. }),
                "cut={cut}: {err}"
            );
        }
    }

    #[test]
    fn encrypted_sst_block_lying_ct_len_is_corrupt_error() {
        let nonce = Nonce::generate_with(&mut DeterministicTestRng::new(7));
        let mut bytes = encode_encrypted_sst_block(&nonce, b"x");
        let len_at = 4 + 2 + NONCE_LEN;
        bytes[len_at..len_at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        let err = decode_encrypted_sst_block(&bytes, &path()).expect_err("lying ct_len is corrupt, not torn-tail");
        assert!(matches!(err, EngineError::CorruptEncryptedSstBlock { .. }));
    }

    #[test]
    fn encrypted_sst_block_wrong_version_is_unsupported() {
        let nonce = Nonce::generate_with(&mut DeterministicTestRng::new(8));
        let mut bytes = encode_encrypted_sst_block(&nonce, b"x");
        bytes[4..6].copy_from_slice(&99u16.to_le_bytes());
        let err = decode_encrypted_sst_block(&bytes, &path()).expect_err("wrong version is unsupported");
        assert!(matches!(err, EngineError::UnsupportedEncryptedSstBlockVersion { .. }));
    }

    #[test]
    fn encrypted_sst_block_aad_binds_sst_id_section_and_section_no() {
        // The anti-permutation property ADR-039 §3 requires: every one of
        // these coordinates changing the AAD is what makes a moved/reordered
        // section fail its tag even though the bytes are individually intact.
        let base = encrypted_sst_block_aad(1, SstSectionType::Data, 0);
        assert_ne!(
            base,
            encrypted_sst_block_aad(2, SstSectionType::Data, 0),
            "sst_id must bind"
        );
        assert_ne!(
            base,
            encrypted_sst_block_aad(1, SstSectionType::Index, 0),
            "section type must bind"
        );
        assert_ne!(
            base,
            encrypted_sst_block_aad(1, SstSectionType::Data, 1),
            "section_no must bind"
        );
    }

    #[test]
    fn encrypted_sst_block_sealed_len_accounts_for_header_and_tag() {
        assert_eq!(
            encrypted_sst_block_sealed_len(100),
            ENCRYPTED_SST_BLOCK_HEADER_LEN + 100 + AEAD_TAG_LEN
        );
    }

    #[test]
    fn envelope_aads_are_distinct_per_artifact() {
        // A WAL ciphertext replayed as an SST section (or vice versa) must
        // fail the AEAD open — the two AADs differing is what guarantees it.
        assert_ne!(
            wal_envelope_aad().to_vec(),
            encrypted_sst_block_aad(0, SstSectionType::Data, 0)
        );
    }
}
