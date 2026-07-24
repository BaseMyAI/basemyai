// SPDX-License-Identifier: BUSL-1.1
//! Home-grown bloom filter (double hashing h1 + i*h2, ADR-039 §6): wraps
//! `format::sst_block::SstBloomFilter` (the wire bytes) with the actual
//! hash-key-into-bits algorithm — the same scheme measured in the N8.1
//! spike (`src/bin/block_spike.rs`). The hash function itself is part of
//! what `SstBloomFilter:1` freezes (ADR-039 §6): changing it without a
//! version bump would make an old filter's bits meaningless to a new
//! build's `contains`, silently turning "maybe present" into wrong answers.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::format::sst_block::SstBloomFilter;

const BLOOM_BITS_PER_KEY: u32 = 10; // ~1% false-positive rate, N8.1 spike
const BLOOM_NUM_HASHES: u64 = 7; // N8.1 spike

pub(super) struct Bloom {
    bits: Vec<u8>,
    num_bits: u64,
}

impl Bloom {
    /// Sizes the bit array for `expected_keys` at [`BLOOM_BITS_PER_KEY`]
    /// bits/key, rounded up to a whole byte and floored at 64 bits so an
    /// empty SST still produces a well-formed (if useless) filter.
    pub(super) fn new(expected_keys: usize) -> Self {
        let num_bits = (expected_keys as u64 * u64::from(BLOOM_BITS_PER_KEY)).max(64);
        let num_bytes = num_bits.div_ceil(8);
        Self {
            bits: vec![0u8; num_bytes as usize],
            num_bits: num_bytes * 8,
        }
    }

    fn hashes(&self, key: &[u8]) -> (u64, u64) {
        let mut h1 = DefaultHasher::new();
        key.hash(&mut h1);
        let mut h2 = DefaultHasher::new();
        0xB10C_5EED_u64.hash(&mut h2);
        key.hash(&mut h2);
        (h1.finish(), h2.finish() | 1)
    }

    pub(super) fn insert(&mut self, key: &[u8]) {
        let (h1, h2) = self.hashes(key);
        for i in 0..BLOOM_NUM_HASHES {
            let bit = h1.wrapping_add(i.wrapping_mul(h2)) % self.num_bits;
            self.bits[(bit / 8) as usize] |= 1 << (bit % 8);
        }
    }

    pub(super) fn contains(&self, key: &[u8]) -> bool {
        let (h1, h2) = self.hashes(key);
        (0..BLOOM_NUM_HASHES).all(|i| {
            let bit = h1.wrapping_add(i.wrapping_mul(h2)) % self.num_bits;
            self.bits[(bit / 8) as usize] & (1 << (bit % 8)) != 0
        })
    }

    /// Wire-format snapshot of the current bits — non-consuming (the writer
    /// keeps `self` afterward, as the freshly-written file's resident
    /// filter).
    pub(super) fn to_filter(&self) -> SstBloomFilter {
        SstBloomFilter {
            num_bits: self.num_bits,
            num_hashes: BLOOM_NUM_HASHES as u32,
            bits: self.bits.clone(),
        }
    }

    pub(super) fn from_filter(filter: SstBloomFilter) -> Self {
        Self {
            bits: filter.bits,
            num_bits: filter.num_bits,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Bloom;

    #[test]
    fn bloom_has_no_false_negatives() {
        let mut bloom = Bloom::new(1000);
        let keys: Vec<Vec<u8>> = (0..1000).map(|i| format!("key-{i}").into_bytes()).collect();
        for key in &keys {
            bloom.insert(key);
        }
        for key in &keys {
            assert!(bloom.contains(key), "false negative for {key:?}");
        }
    }

    #[test]
    fn bloom_filter_roundtrips_through_wire_format() {
        let mut bloom = Bloom::new(100);
        bloom.insert(b"present");
        let filter = bloom.to_filter();
        let reloaded = Bloom::from_filter(filter);
        assert!(reloaded.contains(b"present"));
    }
}
