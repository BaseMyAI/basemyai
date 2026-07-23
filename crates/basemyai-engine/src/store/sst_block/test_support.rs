// SPDX-License-Identifier: BUSL-1.1
//! Shared test fixtures for `sst_block`'s split submodules.

use std::path::Path;

use crate::crypto::CryptoContext;
use crate::key::Key;
use crate::store::Value;

pub(super) fn entries(n: usize, val_len: usize) -> Vec<(Key, Option<Value>)> {
    (0..n)
        .map(|i| {
            let key = Key::from(format!("k/{i:06}").as_bytes());
            let value = if i % 7 == 0 { None } else { Some(vec![b'v'; val_len]) };
            (key, value)
        })
        .collect()
}

/// No tombstones, fixed per-entry wire size — deterministic block
/// boundaries, used by the anti-permutation tests where two blocks need to
/// line up byte-for-byte in length.
pub(super) fn fixed_size_entries(n: usize, val_len: usize) -> Vec<(Key, Option<Value>)> {
    (0..n)
        .map(|i| (Key::from(format!("k/{i:06}").as_bytes()), Some(vec![b'v'; val_len])))
        .collect()
}

pub(super) fn test_crypto(dir: &Path) -> CryptoContext {
    crate::crypto::create_meta(dir, b"sst block test key").expect("create crypto meta")
}
