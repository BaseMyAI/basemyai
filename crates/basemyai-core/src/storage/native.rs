// SPDX-License-Identifier: BUSL-1.1
//! Capability-discovery wrapper around `basemyai_engine::Engine` (the
//! home-grown native backend, ADR-024/ADR-025).
//!
//! Scope boundary: this type implements only [`StorageEngine`] (backend
//! identity + capability discovery). The `MemoryStore` implementation on the
//! native backend exists since N5.1 (ADR-027) but lives in `basemyai`
//! (`storage::NativeMemoryStore`), never here — `MemoryStore` knows agents
//! and memory layers, exactly what this crate must not (ADR-001). This
//! wrapper stays agnostic: no `agent_id`, no memory layers, no
//! `Symbol`/`Edge` — just open + report.

use std::path::Path;

use basemyai_engine::Engine;

use super::engine::{EngineCapabilities, StorageEngine};

/// Thin wrapper exposing capability discovery for the native
/// (`basemyai-engine`) backend. Intentionally minimal.
pub struct NativeEngine {
    inner: Engine,
}

impl NativeEngine {
    /// Opens (creating if absent) a native engine store at `path`.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying `basemyai_engine::Engine` fails to
    /// open (I/O failure, corrupt on-disk state, an encrypted store opened
    /// without its key, etc.).
    pub fn open(path: impl AsRef<Path>) -> basemyai_engine::Result<Self> {
        let inner = Engine::open(path)?;
        Ok(Self { inner })
    }

    /// Opens (creating if absent) an **encrypted** native engine store at
    /// `path` (ADR-030) — WAL and SSTs sealed at rest, `key` verified
    /// against the store's key-wrap at open.
    ///
    /// # Errors
    ///
    /// Returns an error if the key is wrong, if `path` already holds a
    /// plaintext store (no a-posteriori encryption), or on I/O/corruption.
    pub fn open_encrypted(path: impl AsRef<Path>, key: &[u8]) -> basemyai_engine::Result<Self> {
        let inner = Engine::open_encrypted(path, key)?;
        Ok(Self { inner })
    }

    /// Borrows the underlying engine handle.
    #[must_use]
    pub fn inner(&self) -> &Engine {
        &self.inner
    }

    /// Mutably borrows the underlying engine handle.
    pub fn inner_mut(&mut self) -> &mut Engine {
        &mut self.inner
    }
}

impl StorageEngine for NativeEngine {
    fn capabilities(&self) -> EngineCapabilities {
        EngineCapabilities::native(self.inner.is_encrypted())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::EngineKind;

    #[test]
    fn native_engine_reports_honest_capabilities() {
        let dir = tempfile::tempdir().expect("tempdir for native engine test");
        let engine = NativeEngine::open(dir.path()).expect("open native engine");
        let caps = engine.capabilities();

        assert_eq!(caps.kind, EngineKind::Native);
        assert!(caps.vectors, "persistent LM-DiskANN index since N3");
        assert!(caps.full_text, "hand-rolled inverted index + BM25 since N5.2 (ADR-028)");
        assert!(caps.recursive_queries, "bounded BFS graph traversal since N4");
        assert!(caps.transactions, "apply_batch gives real atomic multi-key writes");
        assert!(!caps.encrypted, "a plaintext-opened instance must not claim encryption");
    }

    #[test]
    fn native_engine_reports_encryption_per_instance() {
        let dir = tempfile::tempdir().expect("tempdir for native engine test");
        let engine = NativeEngine::open_encrypted(dir.path(), b"capability test key").expect("open encrypted");
        assert!(
            engine.capabilities().encrypted,
            "at-rest encryption exists since N5.4 (ADR-030) and must be reported per instance"
        );
    }
}
