//! Fuzz target: `format::crypto`'s per-record WAL envelope decoder
//! (ADR-030 §3) on arbitrary/malformed bytes, via the
//! `fuzz_decode_wal_envelope` shim.
//!
//! Mirrors the shape of `wal_decode` (the plaintext record decoder): the
//! encrypted store overlays one envelope per plain WAL record, and this
//! decoder shares its torn-tail contract — `Ok(None)` for an incomplete
//! trailing envelope (expected crash shape), `Err` only for a fully-buffered
//! envelope that is structurally impossible, never a panic. `ct_len` is a
//! wire-controlled count field bounded via `checked_add` + a buffer-length
//! check (never a raw index) before any slice is taken. Real decoder stays
//! `pub(crate)` (see `crypto_meta_decode`'s target doc for why) — this goes
//! through a thin `pub` shim.
#![no_main]

use std::path::Path;

use basemyai_engine::format::crypto;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    crypto::fuzz_decode_wal_envelope(data, Path::new("fuzz-wal.log"));
});
