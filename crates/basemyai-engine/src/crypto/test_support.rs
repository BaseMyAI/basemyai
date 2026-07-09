// SPDX-License-Identifier: BUSL-1.1
//! Non-production RNG for crypto/format tests. Never used in sealing paths.

use chacha20poly1305::aead::rand_core::{CryptoRng, Error, RngCore};

/// Deterministic LCG for wire-format and codec tests only.
pub(crate) struct DeterministicTestRng {
    state: u64,
}

impl DeterministicTestRng {
    #[must_use]
    pub(crate) fn new(seed: u64) -> Self {
        Self { state: seed }
    }
}

impl RngCore for DeterministicTestRng {
    fn next_u32(&mut self) -> u32 {
        self.next_u64() as u32
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        self.state
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        for byte in dest.iter_mut() {
            *byte = (self.next_u64() & 0xFF) as u8;
        }
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), Error> {
        self.fill_bytes(dest);
        Ok(())
    }
}

// Marker required by `AeadCore::generate_nonce` in test builds.
impl CryptoRng for DeterministicTestRng {}
