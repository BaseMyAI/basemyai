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

mod engine;
mod memtable;
mod sst;
mod wal;

pub use engine::{Batch, Engine, EngineOptions};

/// A stored value. Kept as a plain alias (not a newtype) since, unlike
/// [`crate::key::Key`], nothing about its ordering or encoding is
/// load-bearing yet.
pub type Value = Vec<u8>;
