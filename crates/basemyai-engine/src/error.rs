// SPDX-License-Identifier: BUSL-1.1
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

    /// The caller attempted to commit a batch larger than the WAL recovery
    /// decoder's anti-DoS bound. Refuse before writing, otherwise the engine
    /// could produce a WAL it will reject on the next reopen.
    #[error("WAL batch has {len} operations, exceeding the maximum {max}")]
    WalBatchTooLarge { len: usize, max: usize },

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

    /// The store at this directory is encrypted (`crypto.meta` present) but
    /// no key was supplied to `open` (ADR-030 §2).
    #[error("store at {} is encrypted — open it with Engine::open_encrypted and its key", .path.display())]
    MissingEncryptionKey { path: PathBuf },

    /// The supplied key fails to unwrap the store's DEK — an intact
    /// `crypto.meta` whose seal doesn't open under this key. Diagnosed at
    /// open time (fast, unambiguous), never as inexplicable WAL/SST
    /// corruption further in.
    #[error("wrong encryption key for store at {}", .path.display())]
    WrongEncryptionKey { path: PathBuf },

    /// A key was supplied for a store that already exists in plaintext.
    /// Encrypting a posteriori is deliberately not supported (same posture
    /// as libSQL's `rotate_key`, ADR-007/ADR-030 §2) — never silently mix
    /// plaintext and encrypted artifacts in one directory.
    #[error(
        "store at {} already exists in plaintext — it cannot be encrypted a posteriori (ADR-030 §2)",
        .path.display()
    )]
    PlaintextStoreKeySupplied { path: PathBuf },

    /// `rotate_key` was called on a store opened without encryption —
    /// nothing to rotate (parity with `Store::rotate_key`'s
    /// `CoreError::Encryption`, ADR-007).
    #[error("store at {} is not encrypted — rotate_key has nothing to rotate", .path.display())]
    NotEncrypted { path: PathBuf },

    /// The `crypto.meta` key-wrap file failed its checksum or is
    /// structurally malformed — distinct from [`Self::WrongEncryptionKey`]
    /// (intact file, wrong key): two very different diagnoses for a user.
    #[error("corrupt crypto.meta at {}: {reason}", .path.display())]
    CorruptCryptoMeta { path: PathBuf, reason: String },

    /// An AEAD seal operation failed — not reachable through corruption of
    /// on-disk data (those surface as `CorruptWal`/`CorruptSstFooter`/
    /// `CorruptEncryptedSstBlock`/etc.), only through an internal cipher
    /// failure at write time.
    #[error("encryption failure: {reason}")]
    CryptoFailure { reason: String },

    /// A `match_expr` handed to the native FTS search fell outside the
    /// narrow subset `fts_match_expr()` actually produces — quoted
    /// lowercase tokens joined by literal ` OR ` (ADR-028 §1). A franc
    /// error, never a best-effort partial parse: this engine deliberately
    /// does not implement general FTS5 query syntax.
    #[error("match_expr {match_expr:?} is not in the supported subset (quoted tokens joined by \" OR \"): {reason}")]
    UnsupportedMatchExpr { match_expr: String, reason: String },

    /// A block-based SST's [`crate::format::sst_block::SstHeader`] failed
    /// its checksum or is structurally malformed (ADR-039, N8.2 codecs).
    #[error("corrupt SST header in {}: {reason}", .path.display())]
    CorruptSstHeader { path: PathBuf, reason: String },

    /// A block-based SST header's format version is newer (or otherwise
    /// unrecognized) than what this build of the engine understands.
    #[error(
        "unsupported SST header format version {found} in {} (this build understands {expected})",
        .path.display()
    )]
    UnsupportedSstHeaderVersion { path: PathBuf, expected: u16, found: u16 },

    /// A block-based SST's [`crate::format::sst_block::SstDataBlock`]
    /// (one data block, not the whole file) failed its checksum or is
    /// structurally malformed.
    #[error("corrupt SST data block in {}: {reason}", .path.display())]
    CorruptSstDataBlock { path: PathBuf, reason: String },

    /// A block-based SST data block's format version is newer (or
    /// otherwise unrecognized) than what this build of the engine
    /// understands.
    #[error(
        "unsupported SST data block format version {found} in {} (this build understands {expected})",
        .path.display()
    )]
    UnsupportedSstDataBlockVersion { path: PathBuf, expected: u16, found: u16 },

    /// A block-based SST's [`crate::format::sst_block::SstBlockIndex`]
    /// failed its checksum or is structurally malformed.
    #[error("corrupt SST block index in {}: {reason}", .path.display())]
    CorruptSstBlockIndex { path: PathBuf, reason: String },

    /// A block-based SST block index's format version is newer (or
    /// otherwise unrecognized) than what this build of the engine
    /// understands.
    #[error(
        "unsupported SST block index format version {found} in {} (this build understands {expected})",
        .path.display()
    )]
    UnsupportedSstBlockIndexVersion { path: PathBuf, expected: u16, found: u16 },

    /// A block-based SST's [`crate::format::sst_block::SstBloomFilter`]
    /// failed its checksum or is structurally malformed.
    #[error("corrupt SST bloom filter in {}: {reason}", .path.display())]
    CorruptSstBloomFilter { path: PathBuf, reason: String },

    /// A block-based SST bloom filter's format version is newer (or
    /// otherwise unrecognized) than what this build of the engine
    /// understands.
    #[error(
        "unsupported SST bloom filter format version {found} in {} (this build understands {expected})",
        .path.display()
    )]
    UnsupportedSstBloomFilterVersion { path: PathBuf, expected: u16, found: u16 },

    /// A block-based SST's [`crate::format::sst_block::SstFooter`] failed
    /// its checksum, its trailing sentinel magic, or is otherwise
    /// structurally malformed.
    #[error("corrupt SST footer in {}: {reason}", .path.display())]
    CorruptSstFooter { path: PathBuf, reason: String },

    /// A block-based SST footer's format version is newer (or otherwise
    /// unrecognized) than what this build of the engine understands.
    #[error(
        "unsupported SST footer format version {found} in {} (this build understands {expected})",
        .path.display()
    )]
    UnsupportedSstFooterVersion { path: PathBuf, expected: u16, found: u16 },

    /// The `store.meta` store-generation marker
    /// ([`crate::format::store_meta`], ADR-039 §7) failed its checksum or
    /// is structurally malformed. Distinct from a store-generation
    /// mismatch (an intact, decodable `store.meta` whose
    /// `store_format_version` this build does not accept) — that policy
    /// decision belongs to the store-open path (N8.9), not this codec.
    #[error("corrupt store.meta at {}: {reason}", .path.display())]
    CorruptStoreMeta { path: PathBuf, reason: String },

    /// A per-section `EncryptedSstBlock` envelope
    /// ([`crate::format::crypto`], ADR-039 §3) failed structurally (bad
    /// magic, truncation, a lying `ct_len`) or failed AEAD authentication —
    /// both are reported through this single variant: by the time any
    /// section is opened, the key has already been verified against
    /// `crypto.meta`, so a failed tag is unambiguously corruption or
    /// tampering, not a wrong key.
    #[error("corrupt encrypted SST block in {}: {reason}", .path.display())]
    CorruptEncryptedSstBlock { path: PathBuf, reason: String },

    /// An `EncryptedSstBlock` envelope's format version is newer (or
    /// otherwise unrecognized) than what this build of the engine
    /// understands.
    #[error(
        "unsupported encrypted SST block format version {found} in {} (this build understands {expected})",
        .path.display()
    )]
    UnsupportedEncryptedSstBlockVersion { path: PathBuf, expected: u16, found: u16 },

    /// The store at `path` belongs to a different, incompatible on-disk
    /// store generation than this build understands (`store.meta`'s
    /// `store_format_version`, ADR-039 §7). `found == 0` is the sentinel for
    /// "no `store.meta` at all" — a store created before this marker
    /// existed (pre-ADR-039), detected because other store artifacts
    /// (`wal.log` or a `*.sst` file) are present without it. A genuinely
    /// empty/fresh directory never raises this — it just creates a new
    /// `store.meta`.
    #[error(
        "unsupported store format version {found} at {} (this build understands {expected}) — \
         recreate the store with the current version",
        .path.display()
    )]
    UnsupportedStoreFormat { path: PathBuf, expected: u16, found: u16 },
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
