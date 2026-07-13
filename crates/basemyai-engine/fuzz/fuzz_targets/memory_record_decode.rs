//! Fuzz target: `idx::memory::record::decode` on arbitrary/malformed byte
//! streams — the primary memory-record block (ADR-027 §2).
//!
//! The most consequential decoder in this crate to get wrong: unlike every
//! derived structure this crate can rebuild from data, a `MemoryRecord` is
//! primary — nothing else in the store can reconstruct a lost/corrupted one
//! (ADR-040 §1's classification; `store::repair` deliberately never
//! rewrites it). N2/N3 fuzzing lesson applied: `layer_len`/`content_len`/
//! `source_len` are wire-controlled count fields, bounded against the
//! buffer's actual length via an exact-length equation
//! (`HEADER_LEN + layer_len + content_len + source_len + CRC_LEN`) before
//! any string is materialized — a lying length must yield
//! `EngineError::CorruptMemoryRecord`, never a panic or an oversized
//! allocation.
#![no_main]

use basemyai_engine::idx::memory::record;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = record::decode(data);
});
