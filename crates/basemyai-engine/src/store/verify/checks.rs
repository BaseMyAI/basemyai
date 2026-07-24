// SPDX-License-Identifier: BUSL-1.1
//! Physical-audit passes plus the [`verify_store`] orchestration: directory
//! inventory → generation resolution → crypto/key → WAL scan → per-SST
//! layout/block checks → manifest confrontation → dispatch into
//! [`super::super::verify_logical`] for `FullLogical`.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::Path;

use crate::crypto::{self, CryptoContext};
use crate::error::{EngineError, Result};
use crate::format::generation_meta;
use crate::format::sst_block::SST_HEADER_TOTAL_LEN;
use crate::format::sst_manifest;
use crate::format::store_meta;
use crate::format::wal::WalOp;
use crate::store::Value;
use crate::store::sst_block::{self, BlockSstFile};
use crate::store::{verify_logical, wal};

use super::{IssueKind, VerifyMode, VerifyReport, acquire_verification_lock, inventory, sst_error_kind};

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
    verify_store_with_key_mode(dir.as_ref(), key.map(|key| (key, crypto::KeyMode::RawKey)), mode)
}

/// Passphrase counterpart to [`verify_store`]. A passphrase store must be
/// verified through this explicit mode so it never falls back to raw-key
/// derivation (ADR-042).
pub fn verify_store_with_passphrase(
    dir: impl AsRef<Path>,
    passphrase: Option<&[u8]>,
    mode: VerifyMode,
) -> Result<VerifyReport> {
    verify_store_with_key_mode(
        dir.as_ref(),
        passphrase.map(|passphrase| (passphrase, crypto::KeyMode::Passphrase)),
        mode,
    )
}

fn verify_store_with_key_mode(
    root_dir: &Path,
    key: Option<(&[u8], crypto::KeyMode)>,
    mode: VerifyMode,
) -> Result<VerifyReport> {
    let mut report = VerifyReport::default();
    if !root_dir.is_dir() {
        return Err(EngineError::io(
            root_dir.to_path_buf(),
            std::io::Error::new(std::io::ErrorKind::NotFound, "store directory does not exist"),
        ));
    }
    // Every current-format store keeps this stable file after its first
    // writable open. Holding a shared lock prevents pointer publication or
    // generation GC from racing the multi-step inventory below, while an
    // old/fresh directory without the file remains strictly read-only.
    let _verification_lock = acquire_verification_lock(root_dir)?;
    let root_inv = inventory(root_dir, &mut report)?;

    // 1. Store generation (ADR-039 §7) — same gate as `Engine::open`, but
    //    reported instead of raised. A generation mismatch stops the audit:
    //    a store written by a different generation would only produce
    //    misleading per-file noise below. `store_id` is captured here for
    //    the WAL AAD reconstruction below (ADR-044 §4) — `None` only for a
    //    legacy `StoreMeta:1` record verify has never upgraded (read-only).
    let mut store_id: Option<uuid::Uuid> = None;
    match &root_inv.store_meta {
        None => {
            if root_inv.wal.is_some()
                || !root_inv.ssts.is_empty()
                || root_inv.crypto_meta
                || root_inv.generation_meta.is_some()
                || !root_inv.generation_dirs.is_empty()
            {
                report.error(
                    IssueKind::StoreFormatUnsupported,
                    &root_dir.join("store.meta"),
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
                    Ok(meta) => store_id = meta.store_id,
                },
            }
        }
    }

    // 2. Active generation (ADR-042 §3) — resolve the pointer before
    // inventorying crypto/WAL/SST state. Root artifacts and sibling
    // generations are never mixed into the active view.
    let (dir, generation_id, inv) = match &root_inv.generation_meta {
        None => (root_dir.to_path_buf(), 0, root_inv),
        Some(pointer_path) => {
            report.files_checked += 1;
            let bytes = match fs::read(pointer_path) {
                Ok(bytes) => bytes,
                Err(error) => {
                    report.error(IssueKind::Io, pointer_path, error.to_string());
                    return Ok(report.finalize());
                }
            };
            let meta = match generation_meta::decode(&bytes, pointer_path) {
                Ok(meta) => meta,
                Err(EngineError::UnsupportedFormatVersion { .. }) => {
                    report.error(
                        IssueKind::StoreFormatUnsupported,
                        pointer_path,
                        "generation.meta version is not supported by this build",
                    );
                    return Ok(report.finalize());
                }
                Err(error) => {
                    report.error(IssueKind::GenerationMetaCorrupt, pointer_path, error.to_string());
                    return Ok(report.finalize());
                }
            };
            if meta.current_generation == 0 {
                report.error(
                    IssueKind::GenerationMetaCorrupt,
                    pointer_path,
                    "generation.meta must point to a non-zero gen-N directory",
                );
                return Ok(report.finalize());
            }
            let active_dir = root_dir.join(format!("gen-{}", meta.current_generation));
            if !active_dir.is_dir() {
                report.error(
                    IssueKind::GenerationMetaCorrupt,
                    pointer_path,
                    format!("active generation directory {} is missing", active_dir.display()),
                );
                return Ok(report.finalize());
            }
            let active_inv = inventory(&active_dir, &mut report)?;
            (active_dir, meta.current_generation, active_inv)
        }
    };

    // 3. Encryption mode + key — `crypto.meta`'s presence is the single
    //    source of truth (ADR-030 §2), same contract as `Engine::open*`.
    if generation_id != 0 && !inv.crypto_meta {
        report.error(
            IssueKind::CryptoMetaCorrupt,
            &dir.join("crypto.meta"),
            "published generation is missing crypto.meta",
        );
        return Ok(report.finalize());
    }
    let crypto: Option<CryptoContext> = match (inv.crypto_meta, key) {
        (true, None) => {
            return Err(EngineError::MissingEncryptionKey { path: dir.clone() });
        }
        (false, Some(_)) => {
            return Err(EngineError::PlaintextStoreKeySupplied { path: dir.clone() });
        }
        (false, None) => None,
        (true, Some((key, key_mode))) => {
            report.files_checked += 1;
            let loaded = if generation_id == 0 {
                crypto::load_meta_with_mode(&dir, key, key_mode)
            } else {
                crypto::load_meta_for_generation(&dir, key, key_mode, generation_id)
            };
            match loaded {
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
        let scan_result = match store_id {
            None => Err(EngineError::CorruptStoreMeta {
                path: dir.join("store.meta"),
                reason: "store.meta has no store_id — cannot reconstruct the WAL AAD (ADR-044 §4); \
                         reopen the store for writing once to upgrade it"
                    .to_string(),
            }),
            Some(store_id) => match wal::read_wal_epoch_for_verify(&dir) {
                Err(e) => Err(e),
                Ok(None) => Err(EngineError::UnsupportedFormatVersion {
                    path: wal_path.clone(),
                    expected: crate::format::wal::WAL_RECORD_VERSION,
                    found: 0, // sentinel: no wal_epoch.meta at all (pre-ADR-044 WAL)
                }),
                Ok(Some(wal_epoch)) => wal::scan_readonly(wal_path, crypto.as_ref(), store_id, wal_epoch),
            },
        };
        match scan_result {
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

    // 4.5. Durable SST manifest (ENG-DUR-001, ADR-043 §1) — confront the
    //    directory listing above against manifest.meta's live-SST list,
    //    the same check `Engine::open`'s `confront_manifest_with_disk`
    //    performs. Gated on `FullLogical` alongside the rest of the deepest
    //    audit. A missing `manifest.meta` itself is not flagged here: it is
    //    a legitimate, transient state (bootstrap on next open), never
    //    reported as corruption by construction — only a *present* manifest
    //    that disagrees with disk is a real finding.
    if mode == VerifyMode::FullLogical {
        let manifest_path = dir.join(sst_manifest::SST_MANIFEST_FILENAME);
        if manifest_path.is_file() {
            report.files_checked += 1;
            match fs::read(&manifest_path) {
                Err(e) => report.error(IssueKind::Io, &manifest_path, e.to_string()),
                Ok(bytes) => match sst_manifest::decode(&bytes, &manifest_path) {
                    Err(e) => report.error(IssueKind::SstManifestCorrupt, &manifest_path, e.to_string()),
                    Ok(manifest) => {
                        let found_ids: std::collections::HashSet<u64> = inv.ssts.iter().map(|(id, _)| *id).collect();
                        for id in &manifest.live_sst_ids {
                            if !found_ids.contains(id) {
                                report.error(
                                    IssueKind::LiveSstMissing,
                                    &sst_block::sst_path(&dir, *id),
                                    format!("manifest.meta lists SST {id} as live, but no such file exists on disk"),
                                );
                            }
                        }
                    }
                },
            }
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
            verify_logical::check_logical(&live, &dir, &mut report);
        } else {
            report.warning(
                IssueKind::LogicalChecksSkipped,
                &dir,
                "logical checks skipped: physical errors above make the merged key-value view \
                 untrustworthy — repair the physical layer first, then re-verify",
            );
        }
    }

    Ok(report.finalize())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use super::*;
    use crate::store::{Engine, EngineOptions};

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
    fn full_rotation_verifies_only_the_published_generation() {
        let root = tempfile::tempdir().expect("tempdir");
        {
            let mut engine =
                Engine::open_encrypted_with_options(root.path(), KEY, small_options()).expect("open encrypted");
            engine.put(b"kept", b"current").expect("put");
            engine.flush().expect("flush");
            engine.rotate_key_full(b"fresh key").expect("full rotate");
        }

        // A corrupt sibling must be ignored: only generation.meta chooses
        // the audited store. Engine::open will garbage-collect it later.
        let sibling = root.path().join("gen-999");
        fs::create_dir(&sibling).expect("create sibling");
        fs::write(sibling.join("corrupt.sst"), b"not an sst").expect("write sibling");

        let report = verify_store(root.path(), Some(b"fresh key"), VerifyMode::FullLogical).expect("verify");
        assert!(report.healthy, "errors: {:?}", report.errors);
        assert!(report.warnings.is_empty(), "warnings: {:?}", report.warnings);
        assert!(report.records_checked > 0, "the active generation must be audited");
    }

    #[test]
    fn full_passphrase_rotation_verifies_in_passphrase_mode() {
        let root = tempfile::tempdir().expect("tempdir");
        {
            let mut engine = Engine::open_encrypted(root.path(), KEY).expect("open encrypted");
            engine.put(b"key", b"value").expect("put");
            engine
                .rotate_passphrase_full(b"fresh passphrase")
                .expect("full passphrase rotate");
        }

        let report = verify_store_with_passphrase(root.path(), Some(b"fresh passphrase"), VerifyMode::FullLogical)
            .expect("verify passphrase generation");
        assert!(report.healthy, "errors: {:?}", report.errors);
        assert!(report.warnings.is_empty(), "warnings: {:?}", report.warnings);
    }

    #[test]
    fn published_generation_without_crypto_meta_is_reported_as_corrupt() {
        let root = tempfile::tempdir().expect("tempdir");
        {
            let mut engine = Engine::open_encrypted(root.path(), KEY).expect("open encrypted");
            engine.rotate_key_full(b"fresh key").expect("full rotate");
        }
        fs::remove_file(root.path().join("gen-1").join("crypto.meta")).expect("remove crypto meta");

        let report = verify_store(root.path(), Some(b"fresh key"), VerifyMode::Quick).expect("verify");
        assert!(!report.healthy);
        assert!(
            report
                .errors
                .iter()
                .any(|issue| issue.kind == IssueKind::CryptoMetaCorrupt)
        );
    }

    #[test]
    fn corrupt_generation_pointer_is_reported_without_auditing_root_artifacts() {
        let root = build_store(false);
        fs::write(
            root.path().join(generation_meta::GENERATION_META_FILENAME),
            b"not a generation pointer",
        )
        .expect("write corrupt pointer");

        let report = verify_store(root.path(), None, VerifyMode::FullLogical).expect("verify");
        assert!(!report.healthy);
        assert_eq!(report.errors.len(), 1);
        assert_eq!(report.errors[0].kind, IssueKind::GenerationMetaCorrupt);
        assert_eq!(
            report.records_checked, 0,
            "pointer failure must stop before root artifacts"
        );
    }

    #[test]
    fn pointer_to_missing_generation_is_reported_without_fallback_to_root() {
        let root = build_store(false);
        let pointer = generation_meta::encode(&generation_meta::GenerationMeta { current_generation: 7 });
        fs::write(root.path().join(generation_meta::GENERATION_META_FILENAME), pointer).expect("write pointer");

        let report = verify_store(root.path(), None, VerifyMode::FullLogical).expect("verify");
        assert!(!report.healthy);
        assert_eq!(report.errors.len(), 1);
        assert_eq!(report.errors[0].kind, IssueKind::GenerationMetaCorrupt);
        assert_eq!(report.records_checked, 0, "must not fall back to root generation zero");
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
    fn generation_directory_without_store_meta_is_not_fresh() {
        let dir = tempfile::tempdir().expect("tempdir");
        fs::create_dir(dir.path().join("gen-1")).expect("create orphan generation");

        let report = verify_store(dir.path(), None, VerifyMode::Quick).expect("verify");
        assert!(!report.healthy);
        assert_eq!(report.errors.len(), 1);
        assert_eq!(report.errors[0].kind, IssueKind::StoreFormatUnsupported);
    }

    #[test]
    fn verification_refuses_to_race_a_live_writer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _engine = Engine::open_encrypted(dir.path(), KEY).expect("open writer");

        let err = verify_store(dir.path(), Some(KEY), VerifyMode::Quick).expect_err("writer lock must win");
        assert!(matches!(err, EngineError::StoreLocked { .. }));
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
