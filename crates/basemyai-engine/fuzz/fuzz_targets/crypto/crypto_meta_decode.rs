//! Fuzz target: `format::crypto`'s `crypto.meta` decoder (ADR-030 §2) on
//! arbitrary/malformed bytes, via the `fuzz_decode_crypto_meta` shim.
//!
//! `crypto.meta` is the single per-store key-wrap record: `wrapped_len` is a
//! wire-controlled count field, bounded against the buffer's actual
//! remaining length (`wrapped_len != crc_at - pos`) before any slice is
//! materialized — the same N2/N3 fuzzing discipline as every other decoder
//! in this crate. The real decoder (`decode_crypto_meta`) stays
//! `pub(crate)`: its `CryptoMeta`/`Nonce` return types are deliberately
//! crate-private (crypto internals are guarded, ADR-030), so this target
//! goes through a thin `pub` wrapper that runs the same decode and discards
//! the result instead of widening the crate's public API just for fuzzing.
#![no_main]

use std::path::Path;

use basemyai_engine::format::crypto;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    crypto::fuzz_decode_crypto_meta(data, Path::new("fuzz-crypto.meta"));
});
