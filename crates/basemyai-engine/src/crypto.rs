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
//!
//! Typed nonces, salts, and DEKs live in [`material`] — see
//! [`docs/security/crypto-material.md`](../../docs/security/crypto-material.md).

mod material;

#[cfg(test)]
pub(crate) mod test_support;

pub(crate) use material::{Dek, Nonce, Salt, Sealed};

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::error::{EngineError, Result};
use crate::format::crypto::{self as fmt, CryptoMeta};

/// File name of the per-store key-wrap record, next to `wal.log`.
pub(crate) const CRYPTO_META_FILENAME: &str = "crypto.meta";

/// Domain-separation label of the KEK derivation (versioned: a future
/// derivation change is a new label + a `CryptoMeta` version bump, never a
/// silent re-interpretation of existing salts).
const KEK_DOMAIN: &[u8] = b"basemyai-engine/kek/v1";

/// Derives the key-encryption key from the user key and the store's salt.
/// No key stretching (Argon2/PBKDF2) by design — the input is assumed
/// high-entropy, same posture as ADR-007 (user-supplied passphrase, never stored).
/// as-is (ADR-030 §1 records the follow-up if that assumption changes).
fn derive_kek(user_key: &[u8], salt: &Salt) -> Zeroizing<[u8; 32]> {
    let mut hasher = Sha256::new();
    hasher.update(KEK_DOMAIN);
    hasher.update(salt.as_bytes());
    hasher.update(user_key);
    Zeroizing::new(hasher.finalize().into())
}

/// The live encryption state of an opened encrypted store: the unsealed DEK
/// and its ready-to-use cipher. Deliberately does not remember the user key
/// — after `open`, the KEK's only trace is the wrap in `crypto.meta`.
#[derive(Clone)]
pub(crate) struct CryptoContext {
    /// Raw DEK, retained (not just the cipher) because `rotate_key` must
    /// re-wrap it under a fresh KEK (ADR-030 §4).
    dek: Dek,
    cipher: XChaCha20Poly1305,
}

impl std::fmt::Debug for CryptoContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CryptoContext(***)")
    }
}

impl CryptoContext {
    fn from_dek(dek: Dek) -> Self {
        let cipher = XChaCha20Poly1305::new(dek.as_bytes().into());
        Self { dek, cipher }
    }

    /// Seals `plaintext` under the DEK with a fresh random nonce.
    /// XChaCha20's 24-byte nonce space makes random nonces safe without any
    /// persisted counter to crash-reconcile (ADR-030 §1).
    pub(crate) fn seal(&self, plaintext: &[u8], aad: &[u8]) -> Result<Sealed> {
        let nonce = Nonce::generate();
        let ciphertext = self
            .cipher
            .encrypt(XNonce::from_slice(nonce.as_bytes()), Payload { msg: plaintext, aad })
            .map_err(|_| EngineError::CryptoFailure {
                reason: "AEAD seal failed".to_string(),
            })?;
        Ok(Sealed { nonce, ciphertext })
    }

    /// Opens a seal produced by [`CryptoContext::seal`]. An error here means
    /// tampering or corruption — by the time data is being opened, the key
    /// itself was already verified against `crypto.meta`'s wrap. The caller
    /// maps the failure onto its artifact-specific corruption variant.
    ///
    /// `nonce` must come from persisted wire bytes ([`Nonce::from_wire`]) —
    /// never from a fresh [`Nonce::generate`] used for sealing.
    pub(crate) fn open(&self, nonce: &Nonce, ciphertext: &[u8], aad: &[u8]) -> Option<Vec<u8>> {
        self.cipher
            .decrypt(XNonce::from_slice(nonce.as_bytes()), Payload { msg: ciphertext, aad })
            .ok()
    }

    #[cfg(test)]
    fn test_dek_bytes(&self) -> &[u8; 32] {
        self.dek.as_bytes()
    }
}

pub(crate) fn crypto_meta_path(dir: &Path) -> PathBuf {
    dir.join(CRYPTO_META_FILENAME)
}

/// Creates a brand-new encrypted store's `crypto.meta`: random DEK, random
/// salt, DEK sealed under the derived KEK, written tmp+fsync+rename (the
/// same crash-safe recipe as SSTs). Returns the live context.
pub(crate) fn create_meta(dir: &Path, user_key: &[u8]) -> Result<CryptoContext> {
    let ctx = CryptoContext::from_dek(Dek::generate());
    write_meta(dir, user_key, &ctx)?;
    Ok(ctx)
}

/// Wraps `ctx`'s DEK under a KEK freshly derived from `user_key` (new salt,
/// new nonce) and atomically replaces `crypto.meta`. Shared by store
/// creation and key rotation — rotation *is* exactly this operation
/// (ADR-030 §4).
pub(crate) fn write_meta(dir: &Path, user_key: &[u8], ctx: &CryptoContext) -> Result<()> {
    let salt = Salt::generate();
    let kek = derive_kek(user_key, &salt);
    let wrap_cipher = XChaCha20Poly1305::new((&*kek).into());
    let wrap_nonce = Nonce::generate();

    let wrap_aad = CryptoMeta {
        salt: salt.clone(),
        wrap_nonce: wrap_nonce.clone(),
        wrapped_dek: Vec::new(),
    }
    .wrap_aad();

    let wrapped_dek = wrap_cipher
        .encrypt(
            XNonce::from_slice(wrap_nonce.as_bytes()),
            Payload {
                msg: ctx.dek.as_bytes(),
                aad: &wrap_aad,
            },
        )
        .map_err(|_| EngineError::CryptoFailure {
            reason: "DEK wrap failed".to_string(),
        })?;

    let meta = CryptoMeta {
        salt,
        wrap_nonce,
        wrapped_dek,
    };

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
    let wrap_cipher = XChaCha20Poly1305::new((&*kek).into());
    let dek_bytes = Zeroizing::new(
        wrap_cipher
            .decrypt(
                XNonce::from_slice(meta.wrap_nonce.as_bytes()),
                Payload {
                    msg: meta.wrapped_dek.as_slice(),
                    aad: &meta.wrap_aad(),
                },
            )
            .map_err(|_| EngineError::WrongEncryptionKey { path: path.clone() })?,
    );
    let dek_array: [u8; 32] = dek_bytes
        .as_slice()
        .try_into()
        .map_err(|_| EngineError::CorruptCryptoMeta {
            path,
            reason: format!("unwrapped DEK is {} bytes, expected 32", dek_bytes.len()),
        })?;
    Ok(CryptoContext::from_dek(Dek::from_unwrapped(dek_array)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_then_load_roundtrips_the_dek() {
        let dir = tempfile::tempdir().expect("tempdir");
        let created = create_meta(dir.path(), b"user key").expect("create");
        let loaded = load_meta(dir.path(), b"user key").expect("load");
        assert_eq!(created.test_dek_bytes(), loaded.test_dek_bytes());
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
        assert_eq!(
            reloaded.test_dek_bytes(),
            ctx.test_dek_bytes(),
            "rotation must never change the DEK"
        );
    }

    #[test]
    fn seal_open_roundtrips_and_rejects_tampering() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = create_meta(dir.path(), b"k").expect("create");
        let Sealed { nonce, mut ciphertext } = ctx.seal(b"payload", b"aad").expect("seal");
        assert_eq!(ctx.open(&nonce, &ciphertext, b"aad").as_deref(), Some(&b"payload"[..]));
        assert!(ctx.open(&nonce, &ciphertext, b"other aad").is_none(), "AAD is binding");
        let last = ciphertext.len() - 1;
        ciphertext[last] ^= 0xFF;
        assert!(
            ctx.open(&nonce, &ciphertext, b"aad").is_none(),
            "tampering must fail the tag"
        );
    }

    #[test]
    fn seal_uses_fresh_nonces() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = create_meta(dir.path(), b"k").expect("create");
        let Sealed { nonce: n1, .. } = ctx.seal(b"x", b"").expect("seal");
        let Sealed { nonce: n2, .. } = ctx.seal(b"x", b"").expect("seal");
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
