// SPDX-License-Identifier: BUSL-1.1
//! Storage engine identity and capability contract.
//!
//! This is intentionally small: it describes what the current embedded backend
//! can do without turning the core into a generic SQL abstraction. Product
//! semantics stay in the consumer; backend-specific mechanics stay behind
//! [`Store`](crate::Store).

/// Built-in storage backend kind. `Libsql` existed through ADR-011; removed
/// entirely by ADR-032 (native-only) — kept as a single variant so the type
/// isn't a redundant unit struct, but every implementor is `Native` today.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum EngineKind {
    /// Home-grown `basemyai-engine` backend (ADR-024/ADR-025/ADR-032), the
    /// only workspace backend; see
    /// [`EngineCapabilities::native`] for what it honestly supports today.
    Native,
}

/// Capabilities exposed by a storage backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct EngineCapabilities {
    /// Backend implementation kind.
    pub kind: EngineKind,
    /// Native vector index/search support.
    pub vectors: bool,
    /// Full-text search support.
    pub full_text: bool,
    /// Recursive query support.
    pub recursive_queries: bool,
    /// Transactional write support.
    pub transactions: bool,
    /// Whether this opened instance is encrypted at rest.
    pub encrypted: bool,
}

impl EngineCapabilities {
    /// Capabilities of the current `basemyai-engine` (native) backend
    /// instance. `encrypted` reflects whether the opened instance is encrypted.
    ///
    /// Honest as of N5.4 (`docs/TODO-NATIVE-ENGINE.md`, ADR-027/028/030):
    /// `basemyai-engine` is a WAL+memtable+SST KV engine with atomic
    /// multi-key batches (`Engine::apply_batch`, so `transactions: true`), a
    /// persistent LM-DiskANN vector index (N3, so `vectors: true`), a
    /// persistent graph index whose bounded BFS traversal is the behavioral
    /// port of the libSQL recursive CTE (N4, so `recursive_queries: true` —
    /// the capability this flag actually gates), a hand-rolled inverted
    /// index with BM25 scoring over the narrow `match_expr` subset
    /// `basemyai` actually produces (N5.2, so `full_text: true` — Porter
    /// stemming is a documented, assumed gap, ADR-028 §2, not a reason to
    /// report this `false`) and at-rest encryption via AEAD envelopes over
    /// WAL/SST with DEK/KEK key wrapping (N5.4, ADR-030 — per-instance,
    /// hence the parameter).
    #[must_use]
    pub const fn native(encrypted: bool) -> Self {
        Self {
            kind: EngineKind::Native,
            vectors: true,
            full_text: true,
            recursive_queries: true,
            transactions: true,
            encrypted,
        }
    }
}

/// Minimal storage engine contract.
///
/// This is not a query API and not a generic database facade. It is the first
/// stable seam for backend identity and feature discovery.
pub trait StorageEngine {
    /// Returns backend capabilities for this opened instance.
    fn capabilities(&self) -> EngineCapabilities;
}
