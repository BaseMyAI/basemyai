// SPDX-License-Identifier: BUSL-1.1
//! Layer 1 store: WAL + memtable + SST + crash recovery.
//!
//! [`Engine`] is the public, single-writer KV store: `open` loads existing
//! SSTs and replays the WAL (tolerating a torn trailing record left by a
//! crash mid-append); `put`/`delete` append-and-fsync to the WAL before
//! touching the in-memory memtable, so both are durable as soon as the call
//! returns `Ok`; `flush` (auto-triggered past a size threshold, or called
//! explicitly) writes the memtable out as a new SST — fsync the new file,
//! rename it into place, *then* truncate the WAL, never the other order
//! (ADR-025) — and `close` flushes and releases the store.

// Bounded LRU cache of decoded SST data blocks (N8.7, ADR-039 §5.6) —
// shared across every SST an `Engine` holds, consulted only by
// `sst_block::BlockSstFile::get`'s point-lookup path.
mod block_cache;
mod engine;
mod memtable;
// Block-based SST format (ADR-039): writer + optimized reader, the sole SST
// implementation `Engine` uses since N8.5 — see the module doc for the
// read-path/AEAD details.
mod sst_block;
mod stats;
mod wal;

pub use engine::{Batch, DEFAULT_BLOCK_CACHE_CAPACITY_BYTES, DEFAULT_BLOCK_SIZE, Engine, EngineOptions};
pub use stats::EngineStats;

/// A stored value. Kept as a plain alias (not a newtype) since, unlike
/// [`crate::key::Key`], nothing about its ordering or encoding is
/// load-bearing yet.
pub type Value = Vec<u8>;
