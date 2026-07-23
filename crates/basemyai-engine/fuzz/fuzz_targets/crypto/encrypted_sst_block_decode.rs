//! Fuzz target: `format::crypto`'s `EncryptedSstBlock` envelope decoder
//! (ADR-039 §3) on arbitrary/malformed bytes, via the
//! `fuzz_decode_encrypted_sst_block` shim.
//!
//! Unlike the WAL envelope, there is no torn-tail tolerance here — every
//! sealed section is read via an offset/length already known from the SST
//! footer or block index, never mid-stream, so any structural problem is
//! genuine corruption. `ct_len` is bounded against the buffer's actual
//! remaining length (`ct_len != buf.len() - pos`) before any slice is taken.
//! Real decoder stays `pub(crate)` (see `crypto_meta_decode`'s target doc
//! for why) — this goes through a thin `pub` shim.
#![no_main]

use std::path::Path;

use basemyai_engine::format::crypto;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    crypto::fuzz_decode_encrypted_sst_block(data, Path::new("fuzz.sst"));
});
