//! Storage engine identity and capability contract.
//!
//! This is intentionally small: it describes what the current embedded backend
//! can do without turning the core into a generic SQL abstraction. Product
//! semantics stay in the consumer; backend-specific mechanics stay behind
//! [`Store`](crate::Store).

/// Built-in storage backend kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum EngineKind {
    /// libSQL-compatible embedded backend.
    Libsql,
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
    /// Capabilities of the current libSQL backend instance.
    #[must_use]
    pub const fn libsql(encrypted: bool) -> Self {
        Self {
            kind: EngineKind::Libsql,
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
