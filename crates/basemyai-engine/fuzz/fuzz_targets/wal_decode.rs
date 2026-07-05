//! Fuzz target: `format::wal::decode` on arbitrary/malformed byte streams.
//!
//! This is the WAL replay path's decoder (`store::wal::Wal::replay` calls it
//! in a loop over whatever bytes are actually on disk, which after a crash
//! mid-append may be truncated or bit-flipped). It must never panic, and
//! must only ever return `Ok(None)` (torn tail — not yet a full record) or a
//! clean `EngineError::CorruptWal`/`UnsupportedFormatVersion`, never a raw
//! panic/index-out-of-bounds/overflow on attacker-controlled `key_len`/
//! `val_len` fields.
//!
//! We also fuzz the replay loop shape directly: decode repeatedly, advancing
//! by `consumed`, mirroring `Wal::replay` — a decoder that returns
//! `consumed == 0` on `Some(..)` would spin forever in that loop, so we
//! assert forward progress here too.

#![no_main]

use std::path::Path;

use basemyai_engine::format::wal;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let path = Path::new("fuzz.wal");
    let mut offset = 0usize;
    // Bound the loop: a well-behaved decoder always advances by >0 on
    // `Some`, but if it didn't, we want a fuzzer timeout/assert, not an
    // infinite loop wedging the whole corpus run.
    let mut iterations = 0usize;
    while offset < data.len() {
        iterations += 1;
        if iterations > data.len() + 1 {
            panic!("wal::decode did not make forward progress (possible infinite loop)");
        }
        match wal::decode(&data[offset..], path) {
            Ok(Some((_record, consumed))) => {
                assert!(consumed > 0, "decode must consume at least one byte on Some(..)");
                offset += consumed;
            }
            Ok(None) => break,  // torn tail — replay stops here, not an error
            Err(_) => break,    // genuine corruption — clean error, no panic
        }
    }
});
