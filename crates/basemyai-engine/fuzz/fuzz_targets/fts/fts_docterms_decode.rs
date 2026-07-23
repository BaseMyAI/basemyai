//! Fuzz target: `idx::fts::docterms::decode` on arbitrary/malformed byte
//! streams.
//!
//! N2/N3 fuzzing lesson applied: `count` (number of `(term, tf)` entries) is
//! bounded against `MIN_ENTRY_LEN` — the smallest an entry could possibly be
//! — before it drives any allocation, and every `term_len` is bounded
//! against the buffer's actual remaining length before a string is
//! materialized. A lying count or length must yield
//! `EngineError::CorruptFtsDocTerms`, never a panic or an oversized
//! allocation (same discipline as `format::sst_block::decode_sst_data_block`
//! and `memory_record_decode`).
#![no_main]

use basemyai_engine::idx::fts::docterms;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = docterms::decode(data);
});
