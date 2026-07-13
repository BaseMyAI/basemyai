//! Fuzz target: `idx::fts::stats::decode` on arbitrary/malformed byte
//! streams.
//!
//! `FtsStats` is fixed-length (magic/version/`doc_count`/`total_terms`/
//! crc32) with an exact-length check before any field read — same shape as
//! `fts_postings_decode`/`memory_index_meta_decode`. A corrupt or
//! version-mismatched record heals lazily from `docterms` at the consumer
//! level (`PersistentFts`), but the decoder itself must still never panic on
//! adversarial bytes, only return `EngineError::CorruptFtsStats`/
//! `UnsupportedFtsStatsVersion`.
#![no_main]

use basemyai_engine::idx::fts::stats;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = stats::decode(data);
});
