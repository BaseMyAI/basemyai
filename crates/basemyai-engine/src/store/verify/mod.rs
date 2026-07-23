// SPDX-License-Identifier: BUSL-1.1
//! Offline store verification (ADR-040, N9.2): audit a whole store on
//! demand instead of waiting for a read to trip over corruption.
//!
//! [`verify_store`] (in [`checks`], the orchestration + physical-audit half)
//! walks a store directory **read-only** — it never modifies anything, not
//! even the torn-WAL-tail truncation `Engine::open` allows itself (ADR-040
//! §2 rule 1; the WAL is scanned through `store::wal::scan_readonly`, which
//! shares `open`'s decoder but not its truncation). Anomalies are collected
//! into a typed [`VerifyReport`] rather than surfaced as the first `Err` —
//! an operator wants the full diagnosis, not the first symptom. The only
//! `Err` returns are problems with the *call*, not the store's integrity: a
//! missing directory, a missing or wrong encryption key (ADR-040 §2 rule 4).
//!
//! Two modes ship with N9.2 (`FullLogical` — cross-structure consistency —
//! is N9.3's, which is why [`VerifyMode`] is `#[non_exhaustive]`):
//!
//! - [`VerifyMode::Quick`] — O(metadata), the same I/O budget as an open:
//!   `store.meta`/`crypto.meta`, each SST's header/footer/index/bloom
//!   (magic, version, checksum/AEAD, cross-checks), block offset bounds and
//!   contiguity, inter-block key order from the index, and a structural
//!   read-only WAL scan. **No data block is decoded** — payload corruption
//!   is invisible in `Quick`, by construction.
//! - [`VerifyMode::FullPhysical`] — `Quick` plus every data block decoded
//!   through the real read path (crc32/AEAD + index cross-checks), strict
//!   intra-block key order, per-block `tombstone_count`, and the bloom
//!   filter's no-false-negative invariant over the file's actual keys.

use std::fs;
use std::fs::File;
use std::path::{Path, PathBuf};

use crate::error::{EngineError, Result};
use crate::format::generation_meta;
use crate::format::sst_manifest;

mod checks;

pub use checks::{verify_store, verify_store_with_passphrase};

/// How deep a [`verify_store`] audit goes — see the module doc for what
/// each mode covers. `#[non_exhaustive]`: room for future depths without a
/// breaking change (ADR-040 §2).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyMode {
    /// Metadata-only: every persisted structure's framing, checksums and
    /// cross-checks, but no data-block decode. The routine health check.
    Quick,
    /// Everything `Quick` checks, plus every data block decoded and
    /// cross-verified. The full physical audit — O(data).
    FullPhysical,
    /// Everything `FullPhysical` checks, plus cross-structure consistency
    /// over the reserved `idx/` keyspaces (N9.3, see
    /// [`super::verify_logical`]): record ↔ vecmap ↔ vector node linkage,
    /// allocator monotonicity, vector dimensions/metadata, FTS postings ↔
    /// doc-terms ↔ recomputed BM25 stats, graph edge endpoints, per-agent
    /// isolation. Skipped (with an explicit warning, never silently) when
    /// physical errors make the merged view untrustworthy. The deepest
    /// audit — materializes the whole live keyspace in memory.
    FullLogical,
}

/// What kind of anomaly an [`IntegrityIssue`] is — stable, `match`-able
/// taxonomy (ADR-040 §2 rule 3: typed diagnosis, never a bare "corrupt
/// somewhere" boolean). The exact text lives in [`IntegrityIssue::detail`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueKind {
    /// `store.meta` is unreadable or fails its checksum.
    StoreMetaCorrupt,
    /// The store belongs to a generation this build does not understand —
    /// `store.meta`'s version mismatches, or store artifacts exist without
    /// any `store.meta` at all (pre-ADR-039 store).
    StoreFormatUnsupported,
    /// The active-generation pointer is malformed, references generation
    /// zero, or names a generation directory that is not present.
    GenerationMetaCorrupt,
    /// `crypto.meta` is present but structurally corrupt (distinct from a
    /// wrong key, which is an `Err` of the call, not a report entry).
    CryptoMetaCorrupt,
    /// An SST header failed to decode or cross-check.
    SstHeaderCorrupt,
    /// An SST footer failed to decode or cross-check.
    SstFooterCorrupt,
    /// An SST block index failed to decode or cross-check.
    SstBlockIndexCorrupt,
    /// An SST bloom filter failed to decode.
    SstBloomCorrupt,
    /// An SST data block failed its checksum, framing, or index
    /// cross-checks (first/last key, entry count).
    SstDataBlockCorrupt,
    /// A sealed section of an encrypted SST failed AEAD authentication or
    /// envelope framing — tampering or corruption, never a wrong key (the
    /// key was verified against `crypto.meta` before any section is read).
    SstSealedSectionCorrupt,
    /// Block offsets recorded in the index are out of the file's bounds or
    /// not contiguous — a deleted, duplicated or displaced block span.
    SstBlockLayout,
    /// Keys are out of order — between blocks (index routing keys) or
    /// within one (decoded entries).
    SstKeyOrder,
    /// A key present in a data block is missing from the SST's bloom
    /// filter — a false negative, which the filter must never produce.
    SstBloomFalseNegative,
    /// Per-block metadata (e.g. `tombstone_count`) disagrees with the
    /// block's decoded contents.
    SstMetadataMismatch,
    /// A fully-formed WAL record failed its checksum/AEAD, or a batch
    /// payload is malformed — genuine corruption, never the torn tail.
    WalCorrupt,
    /// The WAL ends in a torn trailing record — the *expected* state after
    /// a crash mid-append, which `Engine::open` recovers from. Always a
    /// warning, never an error.
    WalTornTail,
    /// A `*.tmp` orphan left by a crash between write and rename — ignored
    /// by the engine, harmless, reclaimable. Always a warning.
    OrphanTmpFile,
    /// A file the engine did not write and does not recognize. Always a
    /// warning — the engine never deletes what it does not own.
    UnknownFile,
    /// An I/O failure while auditing this specific file.
    Io,
    /// A key inside a reserved `idx/` keyspace does not parse against that
    /// keyspace's documented layout — a foreign or corrupted key the engine's
    /// own writers never produce.
    IdxKeyMalformed,
    /// A value inside a reserved `idx/` keyspace fails its codec (checksum,
    /// framing or version) — the KV pair is intact at the storage layer but
    /// its logical content is not decodable.
    IdxValueCorrupt,
    /// The record ↔ vecmap ↔ vector-node linkage (ADR-027) is broken: a
    /// record whose `vec_id` has no mapping or no live node, an orphan
    /// mapping, a live node no mapping points to, or a `vec_id` claimed by
    /// two records.
    MemoryLinkBroken,
    /// The persisted `next_vec_id` allocator is not strictly above every
    /// vector id in use — the next insert would reuse an id (ADR-027 §4
    /// forbids reuse, ever). A *decodable* stale counter is trusted by
    /// `open` (healing only fires on absent/corrupt), so this is a real
    /// error, not a healable state.
    AllocatorStale,
    /// A vector node's neighbor list references an id with no node block.
    VectorNeighborMissing,
    /// A vector node's dimension disagrees with the index metadata (or with
    /// the other nodes, when the metadata itself is absent).
    VectorDimMismatch,
    /// The vector-index metadata (count, entry point) disagrees with the
    /// nodes actually stored. Always a **warning**: `PersistentVectorIndex::
    /// open` detects exactly this and rebuilds the metadata from the data
    /// (ADR-026 — the data is the single source of truth).
    VectorMetaInconsistent,
    /// The FTS forward/inverted indexes disagree (ADR-028): a posting with
    /// no matching doc-terms entry, a doc-term with no matching posting, a
    /// `tf` mismatch between the two, or a zero `tf`.
    FtsLinkBroken,
    /// An agent's stored BM25 stats disagree with the values recomputed
    /// from its doc-terms. An intact-but-wrong stats record silently skews
    /// every BM25 score (lazy healing only fires on a *corrupt* record), so
    /// a mismatch is an error; a *missing* stats record while documents
    /// exist is a warning (healed on the next search).
    FtsStatsInconsistent,
    /// A graph edge references a source or destination entity with no
    /// entity block. Always a **warning**: the engine's graph API never
    /// enforced endpoint existence (edges and entities are upserted
    /// independently), so a dangling endpoint is tolerated by traversal,
    /// just worth surfacing.
    GraphEdgeDangling,
    /// A structure keyed under one agent resolves to data owned by another
    /// agent — a cross-agent leak that structural key isolation (ADR-006)
    /// is supposed to make impossible.
    AgentIsolationBreach,
    /// The logical pass did not run because physical errors make the merged
    /// key-value view untrustworthy — fix (or repair) the physical layer
    /// first, then re-verify. Always paired with the physical errors that
    /// caused it.
    LogicalChecksSkipped,
    /// `manifest.meta` (ENG-DUR-001) is present but unreadable or fails its
    /// checksum.
    SstManifestCorrupt,
    /// `manifest.meta` lists an SST id as live, but no such file exists on
    /// disk — a live SST silently went missing (ENG-DUR-001, closes the
    /// N11.3 gap: `FullLogical` previously did not detect this either).
    LiveSstMissing,
    /// Any anomaly not covered by a more specific kind.
    Other,
}

/// One anomaly found by [`verify_store`]: a typed kind, the file it was
/// found in, and the exact diagnostic text.
#[derive(Debug, Clone)]
pub struct IntegrityIssue {
    pub kind: IssueKind,
    pub path: PathBuf,
    pub detail: String,
}

impl std::fmt::Display for IntegrityIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{:?}] {}: {}", self.kind, self.path.display(), self.detail)
    }
}

/// Outcome of one [`verify_store`] run (ADR-040 §2).
#[derive(Debug, Default)]
pub struct VerifyReport {
    /// `errors.is_empty()` — warnings (torn WAL tail, tmp orphans) do not
    /// make a store unhealthy: they are expected post-crash states the
    /// engine recovers from on its own.
    pub healthy: bool,
    /// Files examined (`store.meta`, `crypto.meta`, `wal.log`, each SST).
    pub files_checked: u64,
    /// Data blocks decoded and cross-verified (always `0` in `Quick`).
    pub blocks_checked: u64,
    /// Records seen: WAL records structurally scanned, plus (in
    /// `FullPhysical`) every entry decoded out of every data block.
    pub records_checked: u64,
    pub errors: Vec<IntegrityIssue>,
    pub warnings: Vec<IntegrityIssue>,
}

impl VerifyReport {
    pub(crate) fn error(&mut self, kind: IssueKind, path: &Path, detail: impl Into<String>) {
        self.errors.push(IntegrityIssue {
            kind,
            path: path.to_path_buf(),
            detail: detail.into(),
        });
    }

    pub(crate) fn warning(&mut self, kind: IssueKind, path: &Path, detail: impl Into<String>) {
        self.warnings.push(IntegrityIssue {
            kind,
            path: path.to_path_buf(),
            detail: detail.into(),
        });
    }

    fn finalize(mut self) -> Self {
        self.healthy = self.errors.is_empty();
        self
    }
}

/// Maps a typed [`EngineError`] raised while reading an SST to the issue
/// kind it diagnoses — the error's own `Display` text (kept in
/// [`IntegrityIssue::detail`]) already carries the precise reason.
fn sst_error_kind(err: &EngineError) -> IssueKind {
    match err {
        EngineError::CorruptSstHeader { .. } | EngineError::UnsupportedSstHeaderVersion { .. } => {
            IssueKind::SstHeaderCorrupt
        }
        EngineError::CorruptSstFooter { .. } | EngineError::UnsupportedSstFooterVersion { .. } => {
            IssueKind::SstFooterCorrupt
        }
        EngineError::CorruptSstBlockIndex { .. } | EngineError::UnsupportedSstBlockIndexVersion { .. } => {
            IssueKind::SstBlockIndexCorrupt
        }
        EngineError::CorruptSstBloomFilter { .. } | EngineError::UnsupportedSstBloomFilterVersion { .. } => {
            IssueKind::SstBloomCorrupt
        }
        EngineError::CorruptSstDataBlock { .. } | EngineError::UnsupportedSstDataBlockVersion { .. } => {
            IssueKind::SstDataBlockCorrupt
        }
        EngineError::CorruptEncryptedSstBlock { .. } | EngineError::UnsupportedEncryptedSstBlockVersion { .. } => {
            IssueKind::SstSealedSectionCorrupt
        }
        EngineError::Io { .. } => IssueKind::Io,
        _ => IssueKind::Other,
    }
}

/// Everything the one-pass directory inventory finds — file classification
/// happens once, up front, so the rest of the audit works from this instead
/// of re-listing the directory.
struct Inventory {
    store_meta: Option<PathBuf>,
    generation_meta: Option<PathBuf>,
    generation_dirs: Vec<PathBuf>,
    crypto_meta: bool,
    wal: Option<PathBuf>,
    /// `(id, path)`, sorted ascending by id.
    ssts: Vec<(u64, PathBuf)>,
}

fn inventory(dir: &Path, report: &mut VerifyReport) -> Result<Inventory> {
    let mut inv = Inventory {
        store_meta: None,
        generation_meta: None,
        generation_dirs: Vec::new(),
        crypto_meta: false,
        wal: None,
        ssts: Vec::new(),
    };
    for entry in fs::read_dir(dir).map_err(|e| EngineError::io(dir.to_path_buf(), e))? {
        let entry = entry.map_err(|e| EngineError::io(dir.to_path_buf(), e))?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() && name.strip_prefix("gen-").is_some_and(|id| id.parse::<u64>().is_ok()) {
            // Generation directories are resolved exclusively through the
            // root pointer. Retain their presence so an otherwise metadata-
            // free directory is not misreported as a fresh empty store.
            inv.generation_dirs.push(path);
            continue;
        }
        if path.is_dir() {
            report.warning(IssueKind::UnknownFile, &path, "unexpected subdirectory in a store");
            continue;
        }
        match name.as_ref() {
            "store.meta" => inv.store_meta = Some(path),
            generation_meta::GENERATION_META_FILENAME => inv.generation_meta = Some(path),
            "crypto.meta" => inv.crypto_meta = true,
            "wal.log" => inv.wal = Some(path),
            // ADR-042's live-writer advisory lock is engine-owned metadata,
            // not an integrity artifact. Verification never reads or mutates it.
            ".basemyai.lock" => {}
            // ENG-DUR-001's durable SST manifest — recognized here so it is
            // never reported as an unknown file; its own structural checks
            // and confrontation against `inv.ssts` happen later, gated on
            // `FullLogical` (step 4.5 below).
            sst_manifest::SST_MANIFEST_FILENAME => {}
            _ if name.ends_with(".tmp") => {
                report.warning(
                    IssueKind::OrphanTmpFile,
                    &path,
                    "orphan left by a crash between write and rename — ignored by the engine, safe to reclaim",
                );
            }
            _ => {
                let is_sst = path.extension().and_then(|e| e.to_str()) == Some("sst");
                let id = path.file_stem().and_then(|s| s.to_str()).and_then(|s| s.parse().ok());
                match (is_sst, id) {
                    (true, Some(id)) => inv.ssts.push((id, path)),
                    _ => report.warning(
                        IssueKind::UnknownFile,
                        &path,
                        "file the engine did not write and does not recognize",
                    ),
                }
            }
        }
    }
    inv.ssts.sort_by_key(|(id, _)| *id);
    Ok(inv)
}

fn acquire_verification_lock(dir: &Path) -> Result<Option<File>> {
    let path = dir.join(".basemyai.lock");
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(EngineError::io(path, error)),
    };
    match file.try_lock_shared() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Err(EngineError::StoreLocked {
            path: dir.to_path_buf(),
        }),
        Err(std::fs::TryLockError::Error(error)) => Err(EngineError::io(path, error)),
    }
}
