//! Fuzz target: `format::wal_epoch::decode` on arbitrary bytes.
//!
//! ADR-044 §8 (WAL v2 anti-replay, CRYPTO-1 remediation): the new
//! `wal_epoch.meta` counter this build refuses typed on a pre-ADR-044 store
//! must never panic on arbitrary/malformed bytes, same fixed-length/magic/
//! crc discipline as `generation_meta_decode`.
#![no_main]

use std::path::Path;

use basemyai_engine::format::wal_epoch;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = wal_epoch::decode(data, Path::new("wal_epoch.meta"));
});
