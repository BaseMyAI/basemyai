// SPDX-License-Identifier: BUSL-1.1
//! Typed cryptographic material (nonces, salts, DEKs) for ADR-030.
//!
//! Production values are created only through [`Nonce::generate`],
//! [`Salt::generate`], and [`Dek::generate`] (or their `generate_with`
//! variants). [`Nonce::from_wire`] and [`Salt::from_wire`] are reserved for
//! decoding persisted wire bytes — never for sealing.

use std::fmt;

use chacha20poly1305::aead::rand_core::{CryptoRng, RngCore};
use chacha20poly1305::aead::{AeadCore, OsRng};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::format::crypto::{NONCE_LEN, SALT_LEN};

/// XChaCha20-Poly1305 nonce (24 bytes). Created randomly for sealing;
/// [`Self::from_wire`] is decode-only.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Nonce([u8; NONCE_LEN]);

/// Per-store KEK-derivation salt (16 bytes). Created randomly when writing
/// `crypto.meta`; [`Self::from_wire`] is decode-only.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Salt([u8; SALT_LEN]);

/// Data encryption key (32 bytes). Zeroized on drop.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub(crate) struct Dek([u8; 32]);

/// AEAD seal output: fresh nonce plus ciphertext (includes Poly1305 tag).
pub(crate) struct Sealed {
    pub nonce: Nonce,
    pub ciphertext: Vec<u8>,
}

impl fmt::Debug for Nonce {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Nonce(<redacted>)")
    }
}

impl fmt::Debug for Salt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Salt(<redacted>)")
    }
}

impl fmt::Debug for Dek {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Dek(<redacted>)")
    }
}

impl fmt::Debug for Sealed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Sealed")
            .field("nonce", &self.nonce)
            .field("ciphertext", &format!("<{} bytes>", self.ciphertext.len()))
            .finish()
    }
}

impl Nonce {
    /// Draws a fresh random nonce via `XChaCha20Poly1305::generate_nonce` / `OsRng`.
    #[must_use]
    pub(crate) fn generate() -> Self {
        Self::generate_with(&mut OsRng)
    }

    /// Draws a nonce using `rng` (production: `OsRng`; tests: [`super::test_support::DeterministicTestRng`]).
    #[must_use]
    pub(crate) fn generate_with(rng: &mut (impl RngCore + CryptoRng)) -> Self {
        let xnonce = XChaCha20Poly1305::generate_nonce(rng);
        Self::from_xnonce(xnonce)
    }

    /// Reconstructs a nonce from persisted wire bytes. **Decode path only** —
    /// must never feed encryption/sealing.
    #[must_use]
    pub(crate) fn from_wire(bytes: [u8; NONCE_LEN]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub(crate) fn as_bytes(&self) -> &[u8; NONCE_LEN] {
        &self.0
    }

    fn from_xnonce(xnonce: XNonce) -> Self {
        let mut bytes = [0u8; NONCE_LEN];
        bytes.copy_from_slice(xnonce.as_ref());
        Self(bytes)
    }
}

impl Salt {
    #[must_use]
    pub(crate) fn generate() -> Self {
        Self::generate_with(&mut OsRng)
    }

    #[must_use]
    pub(crate) fn generate_with(rng: &mut impl RngCore) -> Self {
        Self(fill_random_bytes::<SALT_LEN>(rng))
    }

    /// Reconstructs a salt from persisted `crypto.meta` bytes. **Decode path only**.
    #[must_use]
    pub(crate) fn from_wire(bytes: [u8; SALT_LEN]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub(crate) fn as_bytes(&self) -> &[u8; SALT_LEN] {
        &self.0
    }
}

impl Dek {
    #[must_use]
    pub(crate) fn generate() -> Self {
        Self::generate_with(&mut OsRng)
    }

    #[must_use]
    pub(crate) fn generate_with(rng: &mut impl RngCore) -> Self {
        Self(fill_random_bytes::<32>(rng))
    }

    #[must_use]
    pub(crate) fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Reconstructs a DEK from bytes unwrapped out of `crypto.meta`. Load path only.
    #[must_use]
    pub(crate) fn from_unwrapped(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

/// Fills a fixed-size buffer from `rng`. Confined here so callers never
/// write `[0u8; N]` + `fill_bytes` at encryption call sites.
fn fill_random_bytes<const N: usize>(rng: &mut impl RngCore) -> [u8; N] {
    let mut bytes = [0u8; N];
    rng.fill_bytes(&mut bytes);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::test_support::DeterministicTestRng;

    // Catastrophic-regression guard only: detects a broken RNG or constant
    // material. This is NOT a statistical security proof.
    #[test]
    fn nonce_generate_produces_distinct_values() {
        let n1 = Nonce::generate();
        let n2 = Nonce::generate();
        assert_ne!(n1, n2);
    }

    // Catastrophic-regression guard only: detects a broken RNG or constant
    // material. This is NOT a statistical security proof.
    #[test]
    fn salt_generate_produces_distinct_values() {
        let s1 = Salt::generate();
        let s2 = Salt::generate();
        assert_ne!(s1, s2);
    }

    // Catastrophic-regression guard only: detects a broken RNG or constant
    // material. This is NOT a statistical security proof.
    #[test]
    fn dek_generate_produces_distinct_values() {
        let d1 = Dek::generate();
        let d2 = Dek::generate();
        assert_ne!(d1.as_bytes(), d2.as_bytes());
    }

    #[test]
    fn deterministic_rng_produces_repeatable_nonces() {
        let mut rng1 = DeterministicTestRng::new(42);
        let mut rng2 = DeterministicTestRng::new(42);
        assert_eq!(Nonce::generate_with(&mut rng1), Nonce::generate_with(&mut rng2));
    }

    #[test]
    fn from_wire_roundtrips_as_bytes() {
        let mut rng = DeterministicTestRng::new(1);
        let nonce = Nonce::generate_with(&mut rng);
        let wire = *nonce.as_bytes();
        assert_eq!(Nonce::from_wire(wire).as_bytes(), nonce.as_bytes());
    }
}
