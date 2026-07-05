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
    /// open (I/O failure, corrupt on-disk state, etc.).
    pub fn open(path: impl AsRef<Path>) -> basemyai_engine::Result<Self> {
        let inner = Engine::open(path)?;
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
        EngineCapabilities::native()
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
        assert!(!caps.full_text, "no FTS/BM25 until N5.2");
        assert!(caps.recursive_queries, "bounded BFS graph traversal since N4");
        assert!(caps.transactions, "apply_batch gives real atomic multi-key writes");
        assert!(!caps.encrypted, "no at-rest encryption until N5.4");
    }
}
