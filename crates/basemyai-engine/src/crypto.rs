// SPDX-License-Identifier: BUSL-1.1
//! Encryption-at-rest machinery (ADR-030): KEK derivation, the DEK/KEK
//! envelope stored in `crypto.meta`, and the AEAD seal/open primitives the
//! WAL and SST paths call.
//!
//! Split of responsibilities: [`crate::format::crypto`] owns the byte
//! layouts (pure codecs), this module owns the cryptography and the
//! `crypto.meta` file I/O, and `store::{wal,sst,engine}` own *when* sealing
//! happens. The user key never encrypts data directly: it derives a KEK
//! (`SHA-256(domain || salt || user_key)`, ADR-030 §1) that seals a random
//! 32-byte DEK; WAL records and SST files are sealed under the DEK. Key
//! rotation therefore re-wraps the DEK in a new `crypto.meta` (one atomic
//! tmp+fsync+rename) and never touches the data files (ADR-030 §4).

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use chacha20poly1305::aead::rand_core::RngCore;
use chacha20poly1305::aead::{Aead, KeyInit, OsRng, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use sha2::{Digest, Sha256};

use crate::error::{EngineError, Result};
use crate::format::crypto::{self as fmt, CryptoMeta, NONCE_LEN, SALT_LEN};

/// File name of the per-store key-wrap record, next to `wal.log`.
pub(crate) const CRYPTO_META_FILENAME: &str = "crypto.meta";

/// Domain-separation label of the KEK derivation (versioned: a future
/// derivation change is a new label + a `CryptoMeta` version bump, never a
/// silent re-interpretation of existing salts).
const KEK_DOMAIN: &[u8] = b"basemyai-engine/kek/v1";

/// Derives the key-encryption key from the user key and the store's salt.
/// No key stretching (Argon2/PBKDF2) by design — the input is assumed
/// high-entropy, same posture as ADR-007 where the key goes to SQLCipher
/// as-is (ADR-030 §1 records the follow-up if that assumption changes).
fn derive_kek(user_key: &[u8], salt: &[u8; SALT_LEN]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(KEK_DOMAIN);
    hasher.update(salt);
    hasher.update(user_key);
    hasher.finalize().into()
}

/// The live encryption state of an opened encrypted store: the unsealed DEK
/// and its ready-to-use cipher. Deliberately does not remember the user key
/// — after `open`, the KEK's only trace is the wrap in `crypto.meta`.
#[derive(Clone)]
pub(crate) struct CryptoContext {
    /// Raw DEK, retained (not just the cipher) because `rotate_key` must
    /// re-wrap it under a fresh KEK (ADR-030 §4).
    dek: [u8; 32],
    cipher: XChaCha20Poly1305,
}

impl std::fmt::Debug for CryptoContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CryptoContext(***)")
    }
}

impl CryptoContext {
    fn from_dek(dek: [u8; 32]) -> Self {
        let cipher = XChaCha20Poly1305::new((&dek).into());
        Self { dek, cipher }
    }

    /// Seals `plaintext` under the DEK with a fresh random nonce.
    /// XChaCha20's 24-byte nonce space makes random nonces safe without any
    /// persisted counter to crash-reconcile (ADR-030 §1).
    pub(crate) fn seal(&self, plaintext: &[u8], aad: &[u8]) -> Result<([u8; NONCE_LEN], Vec<u8>)> {
        let mut nonce = [0u8; NONCE_LEN];
        OsRng.fill_bytes(&mut nonce);
        let ciphertext = self
            .cipher
            .encrypt(XNonce::from_slice(&nonce), Payload { msg: plaintext, aad })
            .map_err(|_| EngineError::CryptoFailure {
                reason: "AEAD seal failed".to_string(),
            })?;
        Ok((nonce, ciphertext))
    }

    /// Opens a seal produced by [`CryptoContext::seal`]. An error here means
    /// tampering or corruption — by the time data is being opened, the key
    /// itself was already verified against `crypto.meta`'s wrap. The caller
    /// maps the failure onto its artifact-specific corruption variant.
    pub(crate) fn open(&self, nonce: &[u8; NONCE_LEN], ciphertext: &[u8], aad: &[u8]) -> Option<Vec<u8>> {
        self.cipher
            .decrypt(XNonce::from_slice(nonce), Payload { msg: ciphertext, aad })
            .ok()
    }
}

pub(crate) fn crypto_meta_path(dir: &Path) -> PathBuf {
    dir.join(CRYPTO_META_FILENAME)
}

/// Creates a brand-new encrypted store's `crypto.meta`: random DEK, random
/// salt, DEK sealed under the derived KEK, written tmp+fsync+rename (the
/// same crash-safe recipe as SSTs). Returns the live context.
pub(crate) fn create_meta(dir: &Path, user_key: &[u8]) -> Result<CryptoContext> {
    let mut dek = [0u8; 32];
    OsRng.fill_bytes(&mut dek);
    let ctx = CryptoContext::from_dek(dek);
    write_meta(dir, user_key, &ctx)?;
    Ok(ctx)
}

/// Wraps `ctx`'s DEK under a KEK freshly derived from `user_key` (new salt,
/// new nonce) and atomically replaces `crypto.meta`. Shared by store
/// creation and key rotation — rotation *is* exactly this operation
/// (ADR-030 §4).
pub(crate) fn write_meta(dir: &Path, user_key: &[u8], ctx: &CryptoContext) -> Result<()> {
    let mut salt = [0u8; SALT_LEN];
    OsRng.fill_bytes(&mut salt);
    let kek = derive_kek(user_key, &salt);
    let wrap_cipher = XChaCha20Poly1305::new((&kek).into());

    let mut meta = CryptoMeta {
        salt,
        wrap_nonce: [0u8; NONCE_LEN],
        wrapped_dek: Vec::new(),
    };
    let mut nonce = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce);
    meta.wrap_nonce = nonce;
    meta.wrapped_dek = wrap_cipher
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: &ctx.dek,
                aad: &meta.wrap_aad(),
            },
        )
        .map_err(|_| EngineError::CryptoFailure {
            reason: "DEK wrap failed".to_string(),
        })?;

    let final_path = crypto_meta_path(dir);
    let tmp_path = final_path.with_extension("meta.tmp");
    let bytes = fmt::encode_crypto_meta(&meta);
    {
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp_path)
            .map_err(|e| EngineError::io(tmp_path.clone(), e))?;
        file.write_all(&bytes)
            .map_err(|e| EngineError::io(tmp_path.clone(), e))?;
        file.sync_all().map_err(|e| EngineError::io(tmp_path.clone(), e))?;
    }
    fs::rename(&tmp_path, &final_path).map_err(|e| EngineError::io(final_path.clone(), e))?;
    Ok(())
}

/// Loads `crypto.meta` and unseals the DEK with the KEK derived from
/// `user_key`. A structurally intact file whose wrap fails to open is
/// [`EngineError::WrongEncryptionKey`] — the key check every encrypted open
/// goes through, so a wrong key fails fast and unambiguously here rather
/// than as inexplicable corruption further into WAL/SST reads.
pub(crate) fn load_meta(dir: &Path, user_key: &[u8]) -> Result<CryptoContext> {
    let path = crypto_meta_path(dir);
    let mut buf = Vec::new();
    let mut file = File::open(&path).map_err(|e| EngineError::io(path.clone(), e))?;
    file.read_to_end(&mut buf)
        .map_err(|e| EngineError::io(path.clone(), e))?;
    let meta = fmt::decode_crypto_meta(&buf, &path)?;

    let kek = derive_kek(user_key, &meta.salt);
    let wrap_cipher = XChaCha20Poly1305::new((&kek).into());
    let dek_bytes = wrap_cipher
        .decrypt(
            XNonce::from_slice(&meta.wrap_nonce),
            Payload {
                msg: meta.wrapped_dek.as_slice(),
                aad: &meta.wrap_aad(),
            },
        )
        .map_err(|_| EngineError::WrongEncryptionKey { path: path.clone() })?;
    let dek: [u8; 32] = dek_bytes
        .as_slice()
        .try_into()
        .map_err(|_| EngineError::CorruptCryptoMeta {
            path,
            reason: format!("unwrapped DEK is {} bytes, expected 32", dek_bytes.len()),
        })?;
    Ok(CryptoContext::from_dek(dek))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_then_load_roundtrips_the_dek() {
        let dir = tempfile::tempdir().expect("tempdir");
        let created = create_meta(dir.path(), b"user key").expect("create");
        let loaded = load_meta(dir.path(), b"user key").expect("load");
        assert_eq!(created.dek, loaded.dek);
    }

    #[test]
    fn load_with_wrong_key_is_wrong_key_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        create_meta(dir.path(), b"right key").expect("create");
        let err = load_meta(dir.path(), b"wrong key").expect_err("wrong key must fail");
        assert!(matches!(err, EngineError::WrongEncryptionKey { .. }));
    }

    #[test]
    fn rewrap_preserves_dek_and_switches_keys() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = create_meta(dir.path(), b"old key").expect("create");
        write_meta(dir.path(), b"new key", &ctx).expect("rewrap");

        let err = load_meta(dir.path(), b"old key").expect_err("old key must no longer unwrap");
        assert!(matches!(err, EngineError::WrongEncryptionKey { .. }));
        let reloaded = load_meta(dir.path(), b"new key").expect("new key unwraps");
        assert_eq!(reloaded.dek, ctx.dek, "rotation must never change the DEK");
    }

    #[test]
    fn seal_open_roundtrips_and_rejects_tampering() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = create_meta(dir.path(), b"k").expect("create");
        let (nonce, mut ct) = ctx.seal(b"payload", b"aad").expect("seal");
        assert_eq!(ctx.open(&nonce, &ct, b"aad").as_deref(), Some(&b"payload"[..]));
        assert!(ctx.open(&nonce, &ct, b"other aad").is_none(), "AAD is binding");
        let last = ct.len() - 1;
        ct[last] ^= 0xFF;
        assert!(ctx.open(&nonce, &ct, b"aad").is_none(), "tampering must fail the tag");
    }

    #[test]
    fn seal_uses_fresh_nonces() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = create_meta(dir.path(), b"k").expect("create");
        let (n1, _) = ctx.seal(b"x", b"").expect("seal");
        let (n2, _) = ctx.seal(b"x", b"").expect("seal");
        assert_ne!(n1, n2);
    }

    #[test]
    fn corrupt_meta_file_is_corrupt_error_not_wrong_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        create_meta(dir.path(), b"k").expect("create");
        let path = crypto_meta_path(dir.path());
        let mut bytes = std::fs::read(&path).expect("read meta");
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0xFF;
        std::fs::write(&path, &bytes).expect("write corrupted");
        let err = load_meta(dir.path(), b"k").expect_err("corrupt file must fail");
        assert!(
            matches!(err, EngineError::CorruptCryptoMeta { .. }),
            "a structurally corrupt file must diagnose as corruption, not as a wrong key: {err}"
        );
    }
}
