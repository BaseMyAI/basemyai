// SPDX-License-Identifier: BUSL-1.1
//! Offline store verification (ADR-040, N9.2): audit a whole store on
//! demand instead of waiting for a read to trip over corruption.
//!
//! [`verify_store`] walks a store directory **read-only** — it never
//! modifies anything, not even the torn-WAL-tail truncation `Engine::open`
//! allows itself (ADR-040 §2 rule 1; the WAL is scanned through
//! `store::wal::scan_readonly`, which shares `open`'s decoder but not its
//! truncation). Anomalies are collected into a typed [`VerifyReport`]
//! rather than surfaced as the first `Err` — an operator wants the full
//! diagnosis, not the first symptom. The only `Err` returns are problems
//! with the *call*, not the store's integrity: a missing directory, a
//! missing or wrong encryption key (ADR-040 §2 rule 4).
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

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};

use crate::crypto::{self, CryptoContext};
use crate::error::{EngineError, Result};
use crate::format::sst_block::SST_HEADER_TOTAL_LEN;
use crate::format::store_meta;
use crate::format::wal::WalOp;
use crate::store::Value;
use crate::store::sst_block::BlockSstFile;
use crate::store::{verify_logical, wal};

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
    crypto_meta: bool,
    wal: Option<PathBuf>,
    /// `(id, path)`, sorted ascending by id.
    ssts: Vec<(u64, PathBuf)>,
}

fn inventory(dir: &Path, report: &mut VerifyReport) -> Result<Inventory> {
    let mut inv = Inventory {
        store_meta: None,
        crypto_meta: false,
        wal: None,
        ssts: Vec::new(),
    };
    for entry in fs::read_dir(dir).map_err(|e| EngineError::io(dir.to_path_buf(), e))? {
        let entry = entry.map_err(|e| EngineError::io(dir.to_path_buf(), e))?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            report.warning(IssueKind::UnknownFile, &path, "unexpected subdirectory in a store");
            continue;
        }
        match name.as_ref() {
            "store.meta" => inv.store_meta = Some(path),
            "crypto.meta" => inv.crypto_meta = true,
            "wal.log" => inv.wal = Some(path),
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

/// Metadata-level checks on one loaded SST: block-span bounds/contiguity
/// and inter-block key order, straight off the resident index — no data
/// block touched (this is `Quick`'s per-SST depth).
fn check_sst_layout(sst: &BlockSstFile, path: &Path, report: &mut VerifyReport) {
    let mut expected_offset = SST_HEADER_TOTAL_LEN as u64;
    let mut prev_last_key: Option<&[u8]> = None;
    for (block_no, entry) in sst.block_index().iter().enumerate() {
        if entry.offset != expected_offset {
            report.error(
                IssueKind::SstBlockLayout,
                path,
                format!(
                    "block {block_no} starts at offset {} but the previous section ends at {expected_offset} — \
                     a displaced, missing or duplicated block span",
                    entry.offset
                ),
            );
        }
        if entry.offset.saturating_add(u64::from(entry.len)) > sst.file_bytes {
            report.error(
                IssueKind::SstBlockLayout,
                path,
                format!(
                    "block {block_no} (offset {}, len {}) extends past the file's {} bytes",
                    entry.offset, entry.len, sst.file_bytes
                ),
            );
        }
        // Resync so one displaced block reports once instead of cascading
        // over every subsequent block.
        expected_offset = entry.offset.saturating_add(u64::from(entry.len));

        if entry.first_key > entry.last_key {
            report.error(
                IssueKind::SstKeyOrder,
                path,
                format!("block {block_no}'s index entry has first_key > last_key"),
            );
        }
        if let Some(prev) = prev_last_key
            && prev >= entry.first_key.as_slice()
        {
            report.error(
                IssueKind::SstKeyOrder,
                path,
                format!(
                    "block {block_no}'s first_key does not sort after block {}'s last_key",
                    block_no - 1
                ),
            );
        }
        prev_last_key = Some(&entry.last_key);
    }
}

/// `FullPhysical`'s per-SST depth: decode every data block through the real
/// read path, then the checks only decoded contents can answer — strict
/// intra-block key order, `tombstone_count`, bloom no-false-negative.
///
/// With `collect: Some`, every decoded entry (tombstones included) is also
/// merged into the caller's key-value view — SSTs are visited oldest to
/// newest, so later inserts overwrite earlier ones, the same layering rule
/// as `Engine::scan_prefix`. This is how `FullLogical` builds its merged
/// view without a second decode pass.
fn check_sst_blocks(
    sst: &BlockSstFile,
    path: &Path,
    report: &mut VerifyReport,
    mut collect: Option<&mut BTreeMap<Vec<u8>, Option<Value>>>,
) {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(e) => {
            report.error(IssueKind::Io, path, format!("cannot open for block audit: {e}"));
            return;
        }
    };
    let mut bloom_miss_reported = false;
    for block_no in 0..sst.block_index().len() {
        let entries = match sst.read_block(&mut file, block_no) {
            Ok(entries) => entries,
            Err(e) => {
                report.error(sst_error_kind(&e), path, e.to_string());
                continue;
            }
        };
        report.blocks_checked += 1;
        report.records_checked += entries.len() as u64;

        if let Some(w) = entries.windows(2).position(|w| w[0].0 >= w[1].0) {
            report.error(
                IssueKind::SstKeyOrder,
                path,
                format!(
                    "block {block_no}: entries {w} and {} are not in strict ascending key order",
                    w + 1
                ),
            );
        }
        let tombstones = entries.iter().filter(|(_, v)| v.is_none()).count() as u32;
        let declared = sst.block_index()[block_no].tombstone_count;
        if tombstones != declared {
            report.error(
                IssueKind::SstMetadataMismatch,
                path,
                format!("block {block_no} holds {tombstones} tombstones but its index entry declares {declared}"),
            );
        }
        if !bloom_miss_reported && let Some((key, _)) = entries.iter().find(|(k, _)| !sst.bloom_contains(k.as_bytes()))
        {
            report.error(
                IssueKind::SstBloomFalseNegative,
                path,
                format!(
                    "key {:?} (block {block_no}) is stored but absent from the bloom filter — \
                     a false negative the filter must never produce",
                    String::from_utf8_lossy(key.as_bytes())
                ),
            );
            bloom_miss_reported = true;
        }
        if let Some(kv) = collect.as_deref_mut() {
            for (key, value) in entries {
                kv.insert(key.into_bytes(), value);
            }
        }
    }
}

/// Audits the store at `dir` without modifying it — see the module doc for
/// what each [`VerifyMode`] covers and ADR-040 for the full integrity
/// model. An empty or freshly-created directory verifies as trivially
/// healthy.
///
/// # Errors
/// Only for problems with the *call*, never the store's integrity
/// (ADR-040 §2 rule 4): [`EngineError::Io`] if `dir` does not exist,
/// [`EngineError::MissingEncryptionKey`] /
/// [`EngineError::WrongEncryptionKey`] /
/// [`EngineError::PlaintextStoreKeySupplied`] on a key/store mode mismatch
/// — the same typed contract as `Engine::open*`. Every detectable on-disk
/// anomaly lands in the returned [`VerifyReport`] instead.
pub fn verify_store(dir: impl AsRef<Path>, key: Option<&[u8]>, mode: VerifyMode) -> Result<VerifyReport> {
    let dir = dir.as_ref();
    let mut report = VerifyReport::default();
    if !dir.is_dir() {
        return Err(EngineError::io(
            dir.to_path_buf(),
            std::io::Error::new(std::io::ErrorKind::NotFound, "store directory does not exist"),
        ));
    }
    let inv = inventory(dir, &mut report)?;

    // 1. Store generation (ADR-039 §7) — same gate as `Engine::open`, but
    //    reported instead of raised. A generation mismatch stops the audit:
    //    a store written by a different generation would only produce
    //    misleading per-file noise below.
    match &inv.store_meta {
        None => {
            if inv.wal.is_some() || !inv.ssts.is_empty() {
                report.error(
                    IssueKind::StoreFormatUnsupported,
                    &dir.join("store.meta"),
                    "store artifacts exist but store.meta is absent — a store from before ADR-039, \
                     which this build does not read",
                );
                return Ok(report.finalize());
            }
            // Genuinely fresh/empty directory: trivially healthy.
            return Ok(report.finalize());
        }
        Some(meta_path) => {
            report.files_checked += 1;
            match fs::read(meta_path) {
                Err(e) => report.error(IssueKind::Io, meta_path, e.to_string()),
                Ok(bytes) => match store_meta::decode(&bytes, meta_path) {
                    Err(e) => report.error(IssueKind::StoreMetaCorrupt, meta_path, e.to_string()),
                    Ok(meta) if meta.store_format_version != store_meta::STORE_FORMAT_VERSION => {
                        report.error(
                            IssueKind::StoreFormatUnsupported,
                            meta_path,
                            format!(
                                "store format version {} (this build understands {})",
                                meta.store_format_version,
                                store_meta::STORE_FORMAT_VERSION
                            ),
                        );
                        return Ok(report.finalize());
                    }
                    Ok(_) => {}
                },
            }
        }
    }

    // 2. Encryption mode + key — `crypto.meta`'s presence is the single
    //    source of truth (ADR-030 §2), same contract as `Engine::open*`.
    let crypto: Option<CryptoContext> = match (inv.crypto_meta, key) {
        (true, None) => {
            return Err(EngineError::MissingEncryptionKey {
                path: dir.to_path_buf(),
            });
        }
        (false, Some(_)) => {
            return Err(EngineError::PlaintextStoreKeySupplied {
                path: dir.to_path_buf(),
            });
        }
        (false, None) => None,
        (true, Some(key)) => {
            report.files_checked += 1;
            match crypto::load_meta(dir, key) {
                Ok(ctx) => Some(ctx),
                Err(EngineError::CorruptCryptoMeta { path, reason }) => {
                    // Without an intact key-wrap there is no DEK, and
                    // without the DEK nothing sealed is verifiable — report
                    // the one certain fact and stop.
                    report.error(IssueKind::CryptoMetaCorrupt, &path, reason);
                    return Ok(report.finalize());
                }
                // Wrong key et al.: a problem with the call, not the store.
                Err(e) => return Err(e),
            }
        }
    };

    // 3. WAL — strictly read-only scan (never `Wal::replay`, which
    //    truncates a torn tail). Torn tail = warning: the expected state
    //    after a crash mid-append, recovered by the next `open`. The
    //    decoded records are kept: `FullLogical` overlays them onto the SST
    //    view below, exactly as `open`'s replay would into the memtable.
    let mut wal_records = Vec::new();
    if let Some(wal_path) = &inv.wal {
        report.files_checked += 1;
        match wal::scan_readonly(wal_path, crypto.as_ref()) {
            Err(e) => report.error(IssueKind::WalCorrupt, wal_path, e.to_string()),
            Ok(scan) => {
                report.records_checked += scan.records.len() as u64;
                if scan.torn_tail_bytes > 0 {
                    report.warning(
                        IssueKind::WalTornTail,
                        wal_path,
                        format!(
                            "{} trailing bytes do not form a complete record — expected after a crash \
                             mid-append; the next open recovers by truncating them",
                            scan.torn_tail_bytes
                        ),
                    );
                }
                wal_records = scan.records;
            }
        }
    }

    // 4. SSTs, oldest to newest — the visit order is load-bearing when
    //    collecting the merged view (later layers overwrite earlier ones).
    let mut kv: BTreeMap<Vec<u8>, Option<Value>> = BTreeMap::new();
    for (id, path) in &inv.ssts {
        report.files_checked += 1;
        let sst = match BlockSstFile::load(path.clone(), *id, crypto.as_ref()) {
            Ok(sst) => sst,
            Err(e) => {
                report.error(sst_error_kind(&e), path, e.to_string());
                continue;
            }
        };
        check_sst_layout(&sst, path, &mut report);
        match mode {
            VerifyMode::Quick => {}
            VerifyMode::FullPhysical => check_sst_blocks(&sst, path, &mut report, None),
            VerifyMode::FullLogical => check_sst_blocks(&sst, path, &mut report, Some(&mut kv)),
        }
    }

    // 5. Logical pass (N9.3) — only over a physically trustworthy view:
    //    running cross-structure checks on top of undecodable/corrupt
    //    blocks would just cascade one root cause into misleading
    //    secondary diagnoses.
    if mode == VerifyMode::FullLogical {
        if report.errors.is_empty() {
            // WAL overlay (newest layer), then drop tombstones: the same
            // merged live view `Engine::open` + `scan_prefix` would serve.
            for record in wal_records {
                match record.op {
                    WalOp::Put => kv.insert(record.key, Some(record.value.unwrap_or_default())),
                    WalOp::Delete => kv.insert(record.key, None),
                    // `scan_readonly` expands batches before returning,
                    // same contract as `Wal::replay`.
                    WalOp::Batch => unreachable!("scan_readonly expands Batch records before returning"),
                };
            }
            let live: BTreeMap<Vec<u8>, Value> = kv.into_iter().filter_map(|(k, v)| Some((k, v?))).collect();
            verify_logical::check_logical(&live, dir, &mut report);
        } else {
            report.warning(
                IssueKind::LogicalChecksSkipped,
                dir,
                "logical checks skipped: physical errors above make the merged key-value view \
                 untrustworthy — repair the physical layer first, then re-verify",
            );
        }
    }

    Ok(report.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Engine, EngineOptions};
    use std::collections::BTreeMap;

    const KEY: &[u8] = b"verify test user key";

    /// Small blocks + early flush so even small test stores span several
    /// data blocks per SST.
    fn small_options() -> EngineOptions {
        EngineOptions {
            memtable_flush_threshold: 1000,
            compaction_sst_threshold: 100,
            block_size: 256,
            ..EngineOptions::default()
        }
    }

    /// Builds a store with one flushed multi-block SST (values + tombstones)
    /// and a clean (empty) WAL. Returns the store dir.
    fn build_store(encrypted: bool) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut engine = if encrypted {
            Engine::open_encrypted_with_options(dir.path(), KEY, small_options()).expect("open")
        } else {
            Engine::open_with_options(dir.path(), small_options()).expect("open")
        };
        for i in 0..80u32 {
            engine
                .put(
                    format!("key-{i:04}").as_bytes(),
                    format!("value-{i}").repeat(4).as_bytes(),
                )
                .expect("put");
        }
        engine.delete(b"key-0007").expect("delete");
        engine.delete(b"key-0013").expect("delete");
        engine.close().expect("close");
        dir
    }

    fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        fs::read_dir(dir)
            .expect("read_dir")
            .map(|e| {
                let path = e.expect("entry").path();
                let bytes = fs::read(&path).expect("read");
                (path, bytes)
            })
            .collect()
    }

    fn the_sst(dir: &Path) -> PathBuf {
        fs::read_dir(dir)
            .expect("read_dir")
            .find_map(|e| {
                let path = e.expect("entry").path();
                (path.extension().and_then(|x| x.to_str()) == Some("sst")).then_some(path)
            })
            .expect("store must hold at least one SST")
    }

    #[test]
    fn healthy_store_verifies_healthy_in_both_modes_clear_and_encrypted() {
        for encrypted in [false, true] {
            let dir = build_store(encrypted);
            let key = encrypted.then_some(KEY);
            let quick = verify_store(dir.path(), key, VerifyMode::Quick).expect("verify");
            assert!(quick.healthy, "quick errors: {:?}", quick.errors);
            assert!(quick.warnings.is_empty(), "quick warnings: {:?}", quick.warnings);
            assert!(quick.files_checked >= 2);
            assert_eq!(quick.blocks_checked, 0, "Quick must not decode data blocks");

            let full = verify_store(dir.path(), key, VerifyMode::FullPhysical).expect("verify");
            assert!(full.healthy, "full errors: {:?}", full.errors);
            assert!(full.blocks_checked > 1, "test store must span several blocks");
            // The two deletes replace their keys' puts in the memtable, so
            // the flushed SST holds 80 entries: 78 values + 2 tombstones.
            assert_eq!(full.records_checked, 80);
        }
    }

    #[test]
    fn verify_never_modifies_the_store_even_with_a_torn_wal_tail() {
        let dir = build_store(false);
        // Unflushed writes + a torn tail: the WAL states verify must
        // observe without repairing.
        {
            let mut engine = Engine::open_with_options(dir.path(), small_options()).expect("reopen");
            engine.put(b"unflushed", b"tail").expect("put");
            // Drop without close: the record stays in the WAL.
        }
        let wal_path = dir.path().join("wal.log");
        let mut wal_bytes = fs::read(&wal_path).expect("read wal");
        wal_bytes.extend_from_slice(&[0xAA, 0xBB, 0xCC]);
        fs::write(&wal_path, &wal_bytes).expect("append torn tail");

        let before = snapshot(dir.path());
        for mode in [VerifyMode::Quick, VerifyMode::FullPhysical] {
            let report = verify_store(dir.path(), None, mode).expect("verify");
            assert!(report.healthy, "torn tail must not be an error: {:?}", report.errors);
            assert!(
                report.warnings.iter().any(|w| w.kind == IssueKind::WalTornTail),
                "torn tail must be a warning: {:?}",
                report.warnings
            );
            assert!(report.records_checked >= 1, "the complete WAL record is counted");
        }
        assert_eq!(snapshot(dir.path()), before, "verify must not modify a single byte");

        // The store still opens and recovers normally afterwards.
        let engine = Engine::open_with_options(dir.path(), small_options()).expect("open after verify");
        assert_eq!(engine.get(b"unflushed").expect("get").as_deref(), Some(&b"tail"[..]));
    }

    #[test]
    fn data_block_corruption_is_invisible_in_quick_and_diagnosed_in_full() {
        let dir = build_store(false);
        let sst = the_sst(dir.path());
        let mut raw = fs::read(&sst).expect("read sst");
        raw[SST_HEADER_TOTAL_LEN + 4] ^= 0xFF;
        fs::write(&sst, &raw).expect("write tampered");

        let quick = verify_store(dir.path(), None, VerifyMode::Quick).expect("verify");
        assert!(quick.healthy, "Quick is O(metadata) and cannot see payload corruption");

        let full = verify_store(dir.path(), None, VerifyMode::FullPhysical).expect("verify");
        assert!(!full.healthy);
        assert!(
            full.errors
                .iter()
                .any(|e| matches!(e.kind, IssueKind::SstDataBlockCorrupt | IssueKind::SstBlockIndexCorrupt)),
            "unexpected diagnosis: {:?}",
            full.errors
        );
        // Only the tampered block fails — the others still audit fine.
        assert!(full.blocks_checked > 0);
    }

    #[test]
    fn encrypted_data_block_tampering_is_a_sealed_section_diagnosis() {
        let dir = build_store(true);
        let sst = the_sst(dir.path());
        let mut raw = fs::read(&sst).expect("read sst");
        // Past the 34-byte EncryptedSstBlock envelope header: an AEAD tag
        // failure, not envelope-framing corruption.
        raw[SST_HEADER_TOTAL_LEN + 40] ^= 0xFF;
        fs::write(&sst, &raw).expect("write tampered");

        let quick = verify_store(dir.path(), Some(KEY), VerifyMode::Quick).expect("verify");
        assert!(quick.healthy, "Quick cannot see sealed-payload corruption either");

        let full = verify_store(dir.path(), Some(KEY), VerifyMode::FullPhysical).expect("verify");
        assert!(!full.healthy);
        assert!(
            full.errors.iter().any(|e| e.kind == IssueKind::SstSealedSectionCorrupt),
            "unexpected diagnosis: {:?}",
            full.errors
        );
    }

    #[test]
    fn truncated_sst_is_a_footer_diagnosis_already_in_quick() {
        let dir = build_store(false);
        let sst = the_sst(dir.path());
        let raw = fs::read(&sst).expect("read sst");
        fs::write(&sst, &raw[..raw.len() - 10]).expect("truncate");

        let report = verify_store(dir.path(), None, VerifyMode::Quick).expect("verify");
        assert!(!report.healthy);
        assert!(
            report
                .errors
                .iter()
                .any(|e| matches!(e.kind, IssueKind::SstFooterCorrupt | IssueKind::Io)),
            "unexpected diagnosis: {:?}",
            report.errors
        );
    }

    #[test]
    fn tampered_block_index_is_diagnosed_in_quick() {
        let dir = build_store(false);
        let sst = the_sst(dir.path());
        let mut raw = fs::read(&sst).expect("read sst");
        // Locate the index via the plaintext footer, then flip a byte in it.
        let footer_bytes = &raw[raw.len() - crate::format::sst_block::SST_FOOTER_LEN..];
        let footer = crate::format::sst_block::decode_sst_footer(footer_bytes, &sst).expect("decode footer");
        let offset = footer.index_offset as usize + 8;
        raw[offset] ^= 0xFF;
        fs::write(&sst, &raw).expect("write tampered");

        let report = verify_store(dir.path(), None, VerifyMode::Quick).expect("verify");
        assert!(!report.healthy);
        assert!(
            report.errors.iter().any(|e| e.kind == IssueKind::SstBlockIndexCorrupt),
            "unexpected diagnosis: {:?}",
            report.errors
        );
    }

    #[test]
    fn corrupt_complete_wal_record_is_an_error_not_a_torn_tail() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let mut engine = Engine::open_with_options(dir.path(), small_options()).expect("open");
            engine.put(b"a", b"1").expect("put");
            engine.put(b"b", b"2").expect("put");
            // Drop without close: both records stay in the WAL.
        }
        let wal_path = dir.path().join("wal.log");
        let mut raw = fs::read(&wal_path).expect("read wal");
        // Flip a byte inside the *first* record: fully buffered, so its bad
        // checksum is genuine corruption, not a torn tail.
        raw[6] ^= 0xFF;
        fs::write(&wal_path, &raw).expect("write tampered");

        let report = verify_store(dir.path(), None, VerifyMode::Quick).expect("verify");
        assert!(!report.healthy);
        assert!(
            report.errors.iter().any(|e| e.kind == IssueKind::WalCorrupt),
            "unexpected diagnosis: {:?}",
            report.errors
        );
    }

    #[test]
    fn tmp_orphan_is_a_warning_and_unknown_file_is_a_warning() {
        let dir = build_store(false);
        fs::write(dir.path().join("00000000000000000009.sst.tmp"), b"garbage").expect("write orphan");
        fs::write(dir.path().join("notes.txt"), b"not ours").expect("write stranger");

        let report = verify_store(dir.path(), None, VerifyMode::FullPhysical).expect("verify");
        assert!(report.healthy, "warnings must not flip healthy: {:?}", report.errors);
        assert!(report.warnings.iter().any(|w| w.kind == IssueKind::OrphanTmpFile));
        assert!(report.warnings.iter().any(|w| w.kind == IssueKind::UnknownFile));
    }

    #[test]
    fn pre_adr039_store_without_store_meta_is_unsupported() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::write(dir.path().join("wal.log"), b"old generation bytes").expect("write wal");

        let report = verify_store(dir.path(), None, VerifyMode::Quick).expect("verify");
        assert!(!report.healthy);
        assert_eq!(report.errors.len(), 1);
        assert_eq!(report.errors[0].kind, IssueKind::StoreFormatUnsupported);
    }

    #[test]
    fn empty_directory_is_trivially_healthy() {
        let dir = tempfile::tempdir().expect("tempdir");
        let report = verify_store(dir.path(), None, VerifyMode::FullPhysical).expect("verify");
        assert!(report.healthy);
        assert_eq!(report.files_checked, 0);
    }

    #[test]
    fn missing_directory_is_a_call_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let gone = dir.path().join("never-created");
        let err = verify_store(&gone, None, VerifyMode::Quick).expect_err("must fail");
        assert!(matches!(err, EngineError::Io { .. }));
    }

    #[test]
    fn key_mode_mismatches_are_typed_call_errors_not_report_entries() {
        let encrypted = build_store(true);
        let err = verify_store(encrypted.path(), None, VerifyMode::Quick).expect_err("key required");
        assert!(matches!(err, EngineError::MissingEncryptionKey { .. }));
        let err = verify_store(encrypted.path(), Some(b"not the key"), VerifyMode::Quick).expect_err("wrong key");
        assert!(matches!(err, EngineError::WrongEncryptionKey { .. }));

        let plaintext = build_store(false);
        let err = verify_store(plaintext.path(), Some(KEY), VerifyMode::Quick).expect_err("no key expected");
        assert!(matches!(err, EngineError::PlaintextStoreKeySupplied { .. }));
    }
}
