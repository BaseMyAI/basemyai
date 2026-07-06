//! `EngineError` — the single error type surfaced by this crate's public API.

use std::io;
use std::path::PathBuf;

/// Errors returned by [`crate::store::Engine`] and the lower-level
/// `store`/`format` building blocks.
///
/// `#[non_exhaustive]`: later work (fuzzing findings, `format.lock`
/// violations, concurrency) will add variants without that being a breaking
/// change for callers that already match with a wildcard arm.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EngineError {
    /// Any I/O failure, tagged with the path it happened on for diagnosis.
    #[error("io error at {}: {source}", .path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// A WAL record failed its checksum, or its header/body was truncated in
    /// a way that is *not* explainable as a torn trailing write (i.e. it was
    /// not the last record replayed). A torn trailing write — the expected,
    /// recoverable crash case — is handled by silently stopping replay, not
    /// by returning this error; see `store::wal::Wal::replay`.
    #[error("corrupt WAL record in {}: {reason}", .path.display())]
    CorruptWal { path: PathBuf, reason: String },

    /// An SST file failed its checksum or is structurally malformed.
    #[error("corrupt SST file {}: {reason}", .path.display())]
    CorruptSst { path: PathBuf, reason: String },

    /// An on-disk record's format version is newer (or otherwise
    /// unrecognized) than what this build of the engine understands.
    #[error(
        "unsupported on-disk format version {found} in {} (this build understands {expected})",
        .path.display()
    )]
    UnsupportedFormatVersion { path: PathBuf, expected: u16, found: u16 },

    /// A vector-index node block (`idx::vector::node`) failed its checksum or
    /// is structurally malformed. Unlike WAL/SST corruption there is no file
    /// path to point at: node blocks are KV values (ADR-026 — one node = one
    /// KV record), so the reason string carries all available context.
    #[error("corrupt vector-index node block: {reason}")]
    CorruptVectorNode { reason: String },

    /// A vector-index node block's format version is newer (or otherwise
    /// unrecognized) than what this build of the engine understands. Pathless
    /// sibling of [`EngineError::UnsupportedFormatVersion`] — node blocks
    /// live inside the KV store, not in their own file.
    #[error("unsupported vector node format version {found} (this build understands {expected})")]
    UnsupportedVectorNodeVersion { expected: u16, found: u16 },

    /// The vector-index metadata record (`idx::vector::meta`) failed its
    /// checksum or is structurally malformed. Like
    /// [`EngineError::CorruptVectorNode`], it is a KV value with no file path
    /// of its own. This condition is recoverable by design: the data is the
    /// single source of truth (ADR-026 §Décision 3), so
    /// `PersistentVectorIndex::open` responds to it by rebuilding the index
    /// from the stored vectors rather than surfacing it to callers.
    #[error("corrupt vector-index metadata record: {reason}")]
    CorruptVectorIndexMeta { reason: String },

    /// The vector-index metadata record's format version is newer (or
    /// otherwise unrecognized) than what this build of the engine
    /// understands.
    #[error("unsupported vector-index metadata version {found} (this build understands {expected})")]
    UnsupportedVectorIndexMetaVersion { expected: u16, found: u16 },

    /// A vector handed to the index does not match the dimension the index
    /// was created with (`VectorIndexParams::dim`).
    #[error("vector dimension mismatch: index expects {expected}, got {found}")]
    VectorDimensionMismatch { expected: usize, found: usize },

    /// An insert reused an id already present in the vector index. Updates
    /// are a later, deliberate feature (delete + reinsert, ADR-026 §4) —
    /// silently overwriting would corrupt the graph's neighbor lists.
    #[error("vector id {id} already exists in the index")]
    DuplicateVectorId { id: u64 },

    /// A string handed to a graph-index key encoder (`key::graph_index`)
    /// would overflow that field's `u32` length prefix. Encoding, not
    /// decoding, but returned as an error rather than silently truncated —
    /// a truncated length prefix would desynchronize the key layout, not
    /// just misencode one field.
    #[error("graph index key field {field} is {len} bytes, exceeding the u32 length-prefix wire field")]
    GraphKeyTooLong { field: &'static str, len: usize },

    /// A graph-entity node block (`idx::graph::entity`) failed its checksum
    /// or is structurally malformed. Pathless, like
    /// [`EngineError::CorruptVectorNode`] — entity blocks are KV values, not
    /// files.
    #[error("corrupt graph entity block: {reason}")]
    CorruptGraphEntity { reason: String },

    /// A graph-entity block's format version is newer (or otherwise
    /// unrecognized) than what this build of the engine understands.
    #[error("unsupported graph entity format version {found} (this build understands {expected})")]
    UnsupportedGraphEntityVersion { expected: u16, found: u16 },

    /// A graph-edge block (`idx::graph::edge`) failed its checksum or is
    /// structurally malformed.
    #[error("corrupt graph edge block: {reason}")]
    CorruptGraphEdge { reason: String },

    /// A graph-edge block's format version is newer (or otherwise
    /// unrecognized) than what this build of the engine understands.
    #[error("unsupported graph edge format version {found} (this build understands {expected})")]
    UnsupportedGraphEdgeVersion { expected: u16, found: u16 },

    /// A string handed to a memory-index key encoder (`key::memory_index`)
    /// would overflow that field's `u32` length prefix. Sibling of
    /// [`EngineError::GraphKeyTooLong`], same rationale.
    #[error("memory index key field {field} is {len} bytes, exceeding the u32 length-prefix wire field")]
    MemoryKeyTooLong { field: &'static str, len: usize },

    /// A memory-record block (`idx::memory::record`) failed its checksum or
    /// is structurally malformed. Pathless, like
    /// [`EngineError::CorruptVectorNode`] — record blocks are KV values.
    #[error("corrupt memory record block: {reason}")]
    CorruptMemoryRecord { reason: String },

    /// A memory-record block's format version is newer (or otherwise
    /// unrecognized) than what this build of the engine understands.
    #[error("unsupported memory record format version {found} (this build understands {expected})")]
    UnsupportedMemoryRecordVersion { expected: u16, found: u16 },

    /// A vector-id mapping record (`idx::memory::vecmap`) failed its
    /// checksum or is structurally malformed.
    #[error("corrupt memory vecmap record: {reason}")]
    CorruptMemoryVecMap { reason: String },

    /// A vector-id mapping record's format version is newer (or otherwise
    /// unrecognized) than what this build of the engine understands.
    #[error("unsupported memory vecmap format version {found} (this build understands {expected})")]
    UnsupportedMemoryVecMapVersion { expected: u16, found: u16 },

    /// The memory-index metadata record (`idx::memory::meta`, the monotonic
    /// `next_vec_id` allocator) failed its checksum or is structurally
    /// malformed. Recoverable by design: `PersistentMemoryIndex::open` heals
    /// it from the data (max of node ∪ vecmap keys + 1, ADR-027 §4) rather
    /// than surfacing it to callers.
    #[error("corrupt memory-index metadata record: {reason}")]
    CorruptMemoryIndexMeta { reason: String },

    /// The memory-index metadata record's format version is newer (or
    /// otherwise unrecognized) than what this build of the engine
    /// understands.
    #[error("unsupported memory-index metadata version {found} (this build understands {expected})")]
    UnsupportedMemoryIndexMetaVersion { expected: u16, found: u16 },

    /// A memory put reused an `(agent, id)` pair whose record already exists
    /// — mirrors the UNIQUE-constraint violation the libSQL backend raises,
    /// never a silent overwrite (which would leave the old record's live
    /// vector node unreferenced, polluting searches; ADR-027 §6).
    #[error("memory record already exists for agent {agent:?}, id {id:?}")]
    DuplicateMemoryId { agent: String, id: String },

    /// A string handed to an FTS-index key encoder (`key::fts_index`) would
    /// overflow that field's `u32` length prefix. Sibling of
    /// [`EngineError::GraphKeyTooLong`]/[`EngineError::MemoryKeyTooLong`],
    /// same rationale.
    #[error("fts index key field {field} is {len} bytes, exceeding the u32 length-prefix wire field")]
    FtsKeyTooLong { field: &'static str, len: usize },

    /// A posting block (`idx::fts::postings`) failed its checksum or is
    /// structurally malformed. Pathless, like [`EngineError::CorruptVectorNode`]
    /// — postings are KV values, not files.
    #[error("corrupt fts posting block: {reason}")]
    CorruptFtsPosting { reason: String },

    /// A posting block's format version is newer (or otherwise unrecognized)
    /// than what this build of the engine understands.
    #[error("unsupported fts posting format version {found} (this build understands {expected})")]
    UnsupportedFtsPostingVersion { expected: u16, found: u16 },

    /// A doc-terms block (`idx::fts::docterms`) failed its checksum or is
    /// structurally malformed.
    #[error("corrupt fts docterms block: {reason}")]
    CorruptFtsDocTerms { reason: String },

    /// A doc-terms block's format version is newer (or otherwise
    /// unrecognized) than what this build of the engine understands.
    #[error("unsupported fts docterms format version {found} (this build understands {expected})")]
    UnsupportedFtsDocTermsVersion { expected: u16, found: u16 },

    /// A per-agent BM25-stats record (`idx::fts::stats`) failed its checksum
    /// or is structurally malformed. Recoverable by design: callers heal it
    /// on demand from the agent's `docterms` (ADR-028 §3), lazily — never a
    /// hard error surfaced to a search.
    #[error("corrupt fts stats record: {reason}")]
    CorruptFtsStats { reason: String },

    /// A per-agent BM25-stats record's format version is newer (or
    /// otherwise unrecognized) than what this build of the engine
    /// understands.
    #[error("unsupported fts stats format version {found} (this build understands {expected})")]
    UnsupportedFtsStatsVersion { expected: u16, found: u16 },

    /// A `match_expr` handed to the native FTS search fell outside the
    /// narrow subset `fts_match_expr()` actually produces — quoted
    /// lowercase tokens joined by literal ` OR ` (ADR-028 §1). A franc
    /// error, never a best-effort partial parse: this engine deliberately
    /// does not implement general FTS5 query syntax.
    #[error("match_expr {match_expr:?} is not in the supported subset (quoted tokens joined by \" OR \"): {reason}")]
    UnsupportedMatchExpr { match_expr: String, reason: String },
}

impl EngineError {
    /// Wraps a raw I/O error with the path it happened on.
    pub(crate) fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

/// Convenience alias used throughout this crate's public API.
pub type Result<T> = std::result::Result<T, EngineError>;
