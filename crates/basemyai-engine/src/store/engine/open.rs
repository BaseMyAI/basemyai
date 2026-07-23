// SPDX-License-Identifier: BUSL-1.1
//! Open + crash recovery: store-generation gate (N8.9), generation
//! resolution (ADR-042 §3), crypto load/create, SST scan + manifest
//! confrontation (ENG-DUR-001), WAL replay, and the routine post-open
//! generation GC ([`super::io::gc_inactive_generations`]).

use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use crate::crypto;
use crate::error::{EngineError, Result};
use crate::format::generation_meta;
use crate::format::store_meta::{self, StoreMeta};
use crate::format::wal::WalOp;
use crate::key::Key;
use crate::store::block_cache::BlockCache;
use crate::store::memtable::Memtable;
use crate::store::sst_block::{self, BlockSstFile};
use crate::store::stats::Counters;
use crate::store::version::{SstHandle, Version};
use crate::store::wal::Wal;

use super::io::gc_inactive_generations;
use super::{Engine, EngineOptions};

impl Engine {
    /// Opens (creating if absent) the store at `path` with default
    /// [`EngineOptions`]: loads existing SSTs, then replays the WAL
    /// (tolerating a torn trailing record) to rebuild whatever memtable
    /// state hadn't been flushed yet.
    ///
    /// # Errors
    /// Besides I/O and corruption: [`EngineError::MissingEncryptionKey`] if
    /// the store at `path` is encrypted (`crypto.meta` present) — use
    /// [`Engine::open_encrypted`] for it.
    ///
    /// Réservé aux tests (`test-util`) : la production ouvre toujours via
    /// [`Engine::open_encrypted`] (ADR-030/033).
    #[cfg(any(test, feature = "test-util"))]
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_options(path, EngineOptions::default())
    }

    /// Same as [`Engine::open`] with explicit tunables.
    #[cfg(any(test, feature = "test-util"))]
    pub fn open_with_options(path: impl AsRef<Path>, options: EngineOptions) -> Result<Self> {
        Self::open_inner(path.as_ref(), options, None)
    }

    /// Opens (creating if absent) an **encrypted** store at `path`
    /// (ADR-030). On first open of a fresh directory, generates the store's
    /// DEK and writes `crypto.meta`; on reopen, unwrapping the DEK verifies
    /// `key` — a wrong key fails here, fast and typed, never as
    /// inexplicable corruption further in.
    ///
    /// # Errors
    /// [`EngineError::WrongEncryptionKey`] if `key` doesn't unwrap the
    /// store's DEK; [`EngineError::PlaintextStoreKeySupplied`] if `path`
    /// already holds a plaintext store (encrypting a posteriori is
    /// deliberately unsupported, ADR-030 §2); plus the usual I/O/corruption
    /// errors.
    pub fn open_encrypted(path: impl AsRef<Path>, key: &[u8]) -> Result<Self> {
        Self::open_encrypted_with_options(path, key, EngineOptions::default())
    }

    /// Opens (creating if absent) an encrypted store with a human
    /// passphrase. Fresh stores persist `CryptoMeta:2` in Argon2id mode;
    /// existing records must already declare that same mode, so raw keys and
    /// passphrases never silently substitute for one another (ADR-042).
    pub fn open_with_passphrase(path: impl AsRef<Path>, passphrase: &[u8]) -> Result<Self> {
        Self::open_with_passphrase_and_options(path, passphrase, EngineOptions::default())
    }

    /// Opens (or creates) a passphrase store using an explicit Argon2id cost
    /// profile when a new `crypto.meta` must be written. Existing stores
    /// always replay the parameters persisted in their metadata.
    pub fn open_with_passphrase_and_profile(
        path: impl AsRef<Path>,
        passphrase: &[u8],
        profile: crypto::Argon2idProfile,
    ) -> Result<Self> {
        Self::open_with_passphrase_profile_and_options(path, passphrase, profile, EngineOptions::default())
    }

    /// Same as [`Engine::open_encrypted`] with explicit tunables.
    pub fn open_encrypted_with_options(path: impl AsRef<Path>, key: &[u8], options: EngineOptions) -> Result<Self> {
        Self::open_inner(
            path.as_ref(),
            options,
            Some((key, crypto::KeyMode::RawKey, crypto::Argon2idProfile::Default)),
        )
    }

    /// Same as [`Engine::open_with_passphrase`] with explicit tunables.
    pub fn open_with_passphrase_and_options(
        path: impl AsRef<Path>,
        passphrase: &[u8],
        options: EngineOptions,
    ) -> Result<Self> {
        Self::open_with_passphrase_profile_and_options(path, passphrase, crypto::Argon2idProfile::Default, options)
    }

    fn open_with_passphrase_profile_and_options(
        path: impl AsRef<Path>,
        passphrase: &[u8],
        profile: crypto::Argon2idProfile,
        options: EngineOptions,
    ) -> Result<Self> {
        Self::open_inner(
            path.as_ref(),
            options,
            Some((passphrase, crypto::KeyMode::Passphrase, profile)),
        )
    }

    fn open_inner(
        path: &Path,
        options: EngineOptions,
        key: Option<(&[u8], crypto::KeyMode, crypto::Argon2idProfile)>,
    ) -> Result<Self> {
        let root_dir = path.to_path_buf();
        fs::create_dir_all(&root_dir).map_err(|e| EngineError::io(root_dir.clone(), e))?;

        // This lock deliberately precedes *all* store metadata checks and
        // mutations. In particular, legacy StoreMeta:1 upgrading must never
        // race another open which could stamp a different store_id.
        let writer_lock = acquire_writer_lock(&root_dir)?;

        // Store-generation check (N8.9, ADR-039 §7) — before touching
        // crypto/WAL/SST state at all: an incompatible or pre-ADR-039 store
        // must fail fast and typed, not as inexplicable corruption further
        // in. A genuinely fresh directory publishes a new `store.meta` here.
        let store_meta = check_or_create_store_meta(&root_dir)?;
        let store_id = store_meta
            .store_id
            .expect("check_or_create_store_meta always returns a stamped StoreMeta:2");

        let (dir, generation_id) = resolve_active_generation(&root_dir)?;

        // `crypto.meta`'s presence is the single source of truth for the
        // store's mode (ADR-030 §2) — never guessed from file contents.
        let meta_exists = crypto::crypto_meta_path(&dir).exists();
        if generation_id != 0 && !meta_exists {
            return Err(EngineError::CorruptCryptoMeta {
                path: crypto::crypto_meta_path(&dir),
                reason: "published generation is missing crypto.meta".to_string(),
            });
        }
        let crypto = match (meta_exists, key) {
            (true, Some((key, crypto::KeyMode::RawKey, _))) if generation_id == 0 => {
                Some(crypto::load_meta(&dir, key)?)
            }
            (true, Some((key, crypto::KeyMode::RawKey, _))) => Some(crypto::load_meta_for_generation(
                &dir,
                key,
                crypto::KeyMode::RawKey,
                generation_id,
            )?),
            (true, Some((key, crypto::KeyMode::Passphrase, _))) => Some(crypto::load_meta_for_generation(
                &dir,
                key,
                crypto::KeyMode::Passphrase,
                generation_id,
            )?),
            (true, None) => return Err(EngineError::MissingEncryptionKey { path: dir }),
            (false, Some((key, mode, profile))) => {
                // Refuse to mix modes: a directory that already holds
                // plaintext artifacts cannot be encrypted a posteriori.
                let has_wal = dir.join("wal.log").exists();
                let has_sst = sst_files_present(&dir)?;
                if has_wal || has_sst {
                    return Err(EngineError::PlaintextStoreKeySupplied { path: dir });
                }
                Some(match (mode, generation_id) {
                    (crypto::KeyMode::RawKey, 0) => crypto::create_meta(&dir, key)?,
                    (mode, generation_id) => {
                        crypto::create_meta_for_generation_with_profile(&dir, key, mode, profile, generation_id)?
                    }
                })
            }
            (false, None) => None,
        };

        let scanned = sst_block::scan_existing(&dir, crypto.as_ref())?;
        // `next_sst_id` derives from the *unfiltered* scan — including any
        // id `confront_manifest_with_disk` is about to drop as an orphan —
        // so a fresh SST never reuses an id an orphan file still occupies
        // on disk (ENG-DUR-002).
        let next_sst_id = scanned.iter().map(|s| s.id + 1).max().unwrap_or(0);
        let (ssts, manifest_generation) = confront_manifest_with_disk(&dir, scanned)?;
        // Observation only (R0): counts pre-existing `*.tmp` orphans without
        // touching or removing them — `scan_existing`/`sst_files_present`
        // above already ignored them exactly as before this counter existed.
        let orphan_bytes_at_open = scan_orphan_tmp_bytes(&dir)?;

        // Everything loaded at open was read from disk: the O(metadata)
        // bytes each SST's lazy `load` actually reads (N8.4 — never the
        // whole file), plus the WAL bytes replayed just below.
        let mut counters = Counters {
            bytes_read: ssts.iter().map(|s| s.bytes_read_at_open).sum(),
            ..Counters::default()
        };

        let sst_remove_failures = Arc::new(AtomicU64::new(0));
        let generation_remove_failures = Arc::new(AtomicU64::new(0));
        let current = Arc::new(Version::build(
            manifest_generation,
            ssts.into_iter()
                .map(|file| SstHandle::new(file, Arc::clone(&sst_remove_failures)))
                .collect(),
        ));

        let wal_path = dir.join("wal.log");
        let mut wal = Wal::open_for_append(wal_path, crypto.clone())?;
        counters.bytes_read += wal.size_on_disk()?;
        let mut memtable = Memtable::new();
        for record in wal.replay()? {
            match record.op {
                WalOp::Put => {
                    memtable.put(Key::from(record.key), record.value.unwrap_or_default());
                }
                WalOp::Delete => memtable.delete(Key::from(record.key)),
                // `Wal::replay` always expands `Batch` records into their
                // individual `Put`/`Delete` sub-operations before returning
                // — a bare `Batch` here would be a bug in that expansion,
                // not a reachable on-disk state.
                WalOp::Batch => {
                    unreachable!("Wal::replay expands Batch records into Put/Delete before returning")
                }
            }
        }

        let block_cache = BlockCache::new(options.block_cache_capacity_bytes);
        gc_inactive_generations(&root_dir, generation_id, &generation_remove_failures);
        Ok(Self {
            root_dir,
            dir,
            generation_id,
            _writer_lock: writer_lock,
            store_id,
            wal,
            memtable,
            current,
            next_sst_id: Arc::new(AtomicU64::new(next_sst_id)),
            options,
            crypto,
            counters,
            orphan_bytes_at_open,
            sst_remove_failures,
            generation_remove_failures,
            active_snapshots: Arc::new(AtomicU64::new(0)),
            point_lookup_full_sst_read: AtomicU64::new(0),
            block_cache,
        })
    }
}

/// `true` if `dir` contains at least one `*.sst` file — the "existing
/// plaintext store" half of the mode check in `Engine::open_inner` (the
/// other half is `wal.log`'s existence), and also half of the old-store
/// detection in [`check_or_create_store_meta`].
fn sst_files_present(dir: &Path) -> Result<bool> {
    if !dir.exists() {
        return Ok(false);
    }
    for entry in fs::read_dir(dir).map_err(|e| EngineError::io(dir.to_path_buf(), e))? {
        let entry = entry.map_err(|e| EngineError::io(dir.to_path_buf(), e))?;
        if entry.path().extension().and_then(|e| e.to_str()) == Some("sst") {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Sums the on-disk size of every `*.tmp` file directly inside `dir` — the
/// [`crate::store::stats::EngineStats::orphan_bytes`] observation (R0):
/// `*.sst.tmp`, `crypto.meta.tmp`, `generation.meta.tmp`, `store.meta.tmp`
/// left behind by a crash mid atomic-replace. Purely observational: does
/// not remove or otherwise touch any file, and does not change which
/// artifacts `scan_existing`/`sst_files_present` treat as live (they
/// already skip `.tmp` files by extension, unaffected by this function).
fn scan_orphan_tmp_bytes(dir: &Path) -> Result<u64> {
    if !dir.exists() {
        return Ok(0);
    }
    let mut total = 0u64;
    for entry in fs::read_dir(dir).map_err(|e| EngineError::io(dir.to_path_buf(), e))? {
        let entry = entry.map_err(|e| EngineError::io(dir.to_path_buf(), e))?;
        let path = entry.path();
        let is_tmp = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".tmp"));
        if is_tmp {
            let metadata = entry.metadata().map_err(|e| EngineError::io(path.clone(), e))?;
            total += metadata.len();
        }
    }
    Ok(total)
}

/// Store-generation gate (N8.9, ADR-039 §7) — the very first thing
/// `Engine::open_inner` does to `dir`, before crypto/WAL/SST state is
/// touched at all:
///
/// - `store.meta` present and its `store_format_version` matches
///   [`store_meta::STORE_FORMAT_VERSION`]: nothing to do, this build
///   understands the store.
/// - `store.meta` present but its version does not match: typed
///   [`EngineError::UnsupportedStoreFormat`] with `found` set to the actual
///   on-disk version.
/// - `store.meta` absent, but `wal.log` or a `*.sst` file already exists:
///   this is a pre-ADR-039 store (or, in principle, any store this build's
///   writer never produced) — [`EngineError::UnsupportedStoreFormat`] with
///   the sentinel `found: 0` (no store.meta was ever written by any
///   version, since [`store_meta::STORE_FORMAT_VERSION`] starts at 2 — "no
///   generation-1 `store.meta`", see that module's doc).
/// - `store.meta` absent and no other artifact present: a genuinely fresh
///   directory — publish a new `store.meta` now, crash-safe (tmp + fsync +
///   rename), behind the `before_manifest_publish` failpoint reserved for
///   this since N7.4.
///
/// Deliberately checks only `wal.log`/`*.sst`, not `crypto.meta`: for a
/// brand-new *encrypted* store, `crypto.meta` is created moments later in
/// the very same `open_inner` call (after this function returns) — treating
/// its presence as an "old artifact" here would make every first-ever
/// encrypted-store open falsely look like an incompatible reopen.
fn acquire_writer_lock(dir: &Path) -> Result<File> {
    let path = dir.join(".basemyai.lock");
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        // This stable lock file has no payload. Never truncate it while a
        // previous opener may still hold the OS-level advisory lock.
        .truncate(false)
        .open(&path)
        .map_err(|e| EngineError::io(path.clone(), e))?;
    match file.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            return Err(EngineError::StoreLocked {
                path: dir.to_path_buf(),
            });
        }
        Err(std::fs::TryLockError::Error(error)) => return Err(EngineError::io(path, error)),
    }
    Ok(file)
}

/// Confronts `scan_existing`'s result against `dir`'s durable manifest
/// (ENG-DUR-001, `docs/audits/2026-07-engine-architecture-safety-audit.md`).
/// Returns the live subset of `found` (orphans dropped) and the manifest's
/// current publication counter.
///
/// - **Manifest absent**: bootstrap — publish one now listing exactly
///   `found`'s ids (generation 0), keep everything `found` contains. Under
///   `STORE_FORMAT_VERSION` 3, `check_or_create_store_meta` already rejects
///   any store built before this milestone, so the only way to reach this
///   branch is a genuinely fresh store, or a crash between a flush/
///   compaction's SST publish and the manifest publish meant to record it
///   — both are safe to treat as "adopt what's on disk as the live set
///   right now" (a deliberate simplification over the ADR-043 §1 draft's
///   additive-migration proposal, enabled by the version bump — see the
///   design note in that ADR).
/// - **Manifest present**: an id it lists but `found` doesn't contain is
///   [`EngineError::MissingLiveSst`] — a live SST silently missing from
///   disk, closing the N11.3 gap
///   (`corruption_smoke.rs::deleted_sst_is_detected_once_catalog_lands`).
///   An id `found` contains but the manifest doesn't list is an orphan:
///   dropped from the returned set and removed best-effort — a failed
///   removal here is inert, not a resurrection risk, because the manifest
///   (not the directory listing) decides liveness from here on.
fn confront_manifest_with_disk(dir: &Path, found: Vec<BlockSstFile>) -> Result<(Vec<BlockSstFile>, u64)> {
    let manifest_path = dir.join(crate::format::sst_manifest::SST_MANIFEST_FILENAME);
    if !manifest_path.exists() {
        let live_sst_ids: Vec<u64> = found.iter().map(|s| s.id).collect();
        super::io::publish_sst_manifest(dir, 0, &live_sst_ids)?;
        return Ok((found, 0));
    }
    let bytes = fs::read(&manifest_path).map_err(|e| EngineError::io(manifest_path.clone(), e))?;
    let manifest = crate::format::sst_manifest::decode(&bytes, &manifest_path)?;

    let found_ids: std::collections::HashSet<u64> = found.iter().map(|s| s.id).collect();
    for id in &manifest.live_sst_ids {
        if !found_ids.contains(id) {
            return Err(EngineError::MissingLiveSst {
                id: *id,
                path: sst_block::sst_path(dir, *id),
            });
        }
    }

    let live: std::collections::HashSet<u64> = manifest.live_sst_ids.iter().copied().collect();
    let mut kept = Vec::with_capacity(found.len());
    for sst in found {
        if live.contains(&sst.id) {
            kept.push(sst);
        } else {
            let _ = fs::remove_file(&sst.path);
        }
    }
    Ok((kept, manifest.manifest_generation))
}

/// True if `root_dir` contains at least one `gen-<id>` directory. Used only
/// by [`resolve_active_generation`] to tell a genuinely fresh/legacy store
/// (no pointer, no generation directories) apart from a store whose
/// generation pointer was lost while real data still lives under `gen-N`
/// (ENG-DUR-004, `docs/audits/2026-07-engine-architecture-safety-audit.md`).
fn any_generation_dir_present(root_dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(root_dir) else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry
            .path()
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix("gen-"))
            .is_some_and(|id| id.parse::<u64>().is_ok())
    })
}

/// True if `root_dir` itself still holds its own generation-0 artifacts
/// (`wal.log`, `crypto.meta`, or any `*.sst`). Distinguishes the two ways a
/// `gen-N` directory can sit next to a missing pointer:
///
/// - A **full rotation aborted before `publish_generation`** (e.g. killed at
///   `after_full_rotation_new_dek`) leaves a half-built `gen-N` next to a
///   root that is still fully intact — generation 0 never stopped being
///   live, and `gc_inactive_generations` sweeping the incomplete `gen-N`
///   away at the end of a normal open is the correct, already-tested
///   recovery (`full_rotation_abort_boundaries_keep_the_published_generation_healthy`).
/// - A **lost pointer rename after a completed rotation** (ENG-DUR-004's
///   actual danger) leaves root *empty* — `gc_old_generation` already swept
///   `wal.log`/`crypto.meta`/`*.sst` out of it, durably, as part of the very
///   rotation whose pointer publication didn't survive. Only this case is
///   the impossible, refuse-typed state.
fn root_generation_zero_artifacts_present(root_dir: &Path) -> Result<bool> {
    Ok(
        root_dir.join("wal.log").exists()
            || crypto::crypto_meta_path(root_dir).exists()
            || sst_files_present(root_dir)?,
    )
}

/// Resolves the only data directory an opener may inspect. Before a full
/// rotation there is no pointer and the root is the legacy logical generation
/// zero. Once a pointer exists, arbitrary root artifacts and sibling
/// generations are ignored; only `gen-<current>` is active.
fn resolve_active_generation(root_dir: &Path) -> Result<(PathBuf, u64)> {
    let pointer_path = root_dir.join(generation_meta::GENERATION_META_FILENAME);
    if !pointer_path.exists() {
        // ENG-DUR-004: a missing pointer next to a live `gen-N` *and* a root
        // with no generation-0 artifacts of its own is an impossible state
        // outside a crash mid-rotation *after* the old generation was
        // already swept — treating it as "generation 0" would make the
        // unconditional post-open GC (`gc_inactive_generations`) delete the
        // only real generation, destroying the store it was meant to
        // protect. Refuse instead of "repairing". A `gen-N` next to a root
        // that *still* has its own artifacts is the ordinary "rotation
        // aborted before publish" case — root is still genuinely live, and
        // ignoring the half-built `gen-N` (swept up by the routine
        // post-open GC below) is correct, not a state to refuse.
        if any_generation_dir_present(root_dir) && !root_generation_zero_artifacts_present(root_dir)? {
            return Err(EngineError::CorruptGenerationMeta {
                path: pointer_path,
                reason: "generation pointer missing, a gen-N directory exists, and the root has no \
                         generation-0 artifacts of its own — refusing to treat this store as a \
                         fresh generation 0"
                    .to_string(),
            });
        }
        return Ok((root_dir.to_path_buf(), 0));
    }
    let bytes = fs::read(&pointer_path).map_err(|e| EngineError::io(pointer_path.clone(), e))?;
    let meta = generation_meta::decode(&bytes, &pointer_path)?;
    if meta.current_generation == 0 {
        return Err(EngineError::CorruptGenerationMeta {
            path: pointer_path,
            reason: "generation.meta must point to a non-zero gen-N directory".to_string(),
        });
    }
    let active_dir = root_dir.join(format!("gen-{}", meta.current_generation));
    if !active_dir.is_dir() {
        return Err(EngineError::CorruptGenerationMeta {
            path: pointer_path,
            reason: format!("active generation directory {} is missing", active_dir.display()),
        });
    }
    Ok((active_dir, meta.current_generation))
}

fn check_or_create_store_meta(dir: &Path) -> Result<StoreMeta> {
    let meta_path = dir.join("store.meta");
    if meta_path.exists() {
        let bytes = fs::read(&meta_path).map_err(|e| EngineError::io(meta_path.clone(), e))?;
        let mut meta = store_meta::decode(&bytes, &meta_path)?;
        if meta.store_format_version != store_meta::STORE_FORMAT_VERSION {
            return Err(EngineError::UnsupportedStoreFormat {
                path: dir.to_path_buf(),
                expected: store_meta::STORE_FORMAT_VERSION,
                found: meta.store_format_version,
            });
        }
        if meta.store_id.is_none() {
            meta.store_id = Some(uuid::Uuid::now_v7());
            write_store_meta(&meta_path, &meta)?;
        }
        return Ok(meta);
    }

    let has_generation_dir = fs::read_dir(dir)
        .map_err(|e| EngineError::io(dir.to_path_buf(), e))?
        .filter_map(std::result::Result::ok)
        .any(|entry| {
            entry.path().is_dir()
                && entry
                    .file_name()
                    .to_str()
                    .and_then(|name| name.strip_prefix("gen-"))
                    .is_some_and(|id| id.parse::<u64>().is_ok())
        });
    let has_old_artifacts = dir.join("wal.log").exists()
        || crypto::crypto_meta_path(dir).exists()
        || dir.join(generation_meta::GENERATION_META_FILENAME).exists()
        || has_generation_dir
        || sst_files_present(dir)?;
    if has_old_artifacts {
        return Err(EngineError::UnsupportedStoreFormat {
            path: dir.to_path_buf(),
            expected: store_meta::STORE_FORMAT_VERSION,
            found: 0, // sentinel: no store.meta at all (pre-ADR-039 store)
        });
    }

    let meta = StoreMeta {
        store_format_version: store_meta::STORE_FORMAT_VERSION,
        store_id: Some(uuid::Uuid::now_v7()),
    };
    write_store_meta(&meta_path, &meta)?;
    Ok(meta)
}

fn write_store_meta(meta_path: &Path, meta: &StoreMeta) -> Result<()> {
    let tmp_path = meta_path.with_extension("meta.tmp");
    let bytes = store_meta::encode(meta);
    {
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp_path)
            .map_err(|e| EngineError::io(tmp_path.clone(), e))?;
        file.write_all(&bytes)
            .map_err(|e| EngineError::io(tmp_path.clone(), e))?;
        file.sync_all().map_err(|e| EngineError::io(tmp_path.clone(), e))?;
    }
    crate::fail_point!("before_manifest_publish");
    fs::rename(&tmp_path, meta_path).map_err(|e| EngineError::io(meta_path, e))?;
    // ENG-DUR-003: see `crate::fs_util`. `store.meta` has no meaningful
    // parent besides the store root itself.
    if let Some(dir) = meta_path.parent() {
        crate::fs_util::sync_dir(dir)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Engine;
    use crate::store::engine::test_support::{KEY, small_options};

    #[test]
    fn encrypted_roundtrip_survives_flush_compaction_and_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let mut engine = Engine::open_encrypted_with_options(dir.path(), KEY, small_options()).expect("open");
            assert!(engine.is_encrypted());
            for i in 0..20u32 {
                engine
                    .put(format!("key-{i:03}").as_bytes(), format!("value-{i}").as_bytes())
                    .expect("put");
            }
            engine.delete(b"key-003").expect("delete");
            // Unflushed tail stays in the WAL on purpose: reopen must
            // replay encrypted WAL records, not just load SSTs.
            engine.put(b"tail", b"unflushed").expect("put tail");
        }
        let engine = Engine::open_encrypted_with_options(dir.path(), KEY, small_options()).expect("reopen");
        assert_eq!(engine.get(b"key-000").expect("get").as_deref(), Some(&b"value-0"[..]));
        assert_eq!(engine.get(b"key-019").expect("get").as_deref(), Some(&b"value-19"[..]));
        assert_eq!(engine.get(b"key-003").expect("get"), None);
        assert_eq!(engine.get(b"tail").expect("get").as_deref(), Some(&b"unflushed"[..]));
    }

    #[test]
    fn legacy_store_meta_is_stamped_with_a_stable_id_under_the_writer_lock() {
        let dir = tempfile::tempdir().expect("tempdir");
        let meta_path = dir.path().join("store.meta");
        let legacy = StoreMeta {
            store_format_version: store_meta::STORE_FORMAT_VERSION,
            store_id: None,
        };
        fs::write(&meta_path, store_meta::encode(&legacy)).expect("write legacy StoreMeta:1");

        let engine = Engine::open_encrypted(dir.path(), KEY).expect("open upgrades StoreMeta:1");
        let decoded = store_meta::decode(&fs::read(&meta_path).expect("read upgraded store.meta"), &meta_path)
            .expect("decode upgraded StoreMeta:2");
        assert_eq!(decoded.store_id, Some(engine.store_id()));
    }

    #[test]
    fn generation_pointer_never_falls_back_to_root_artifacts() {
        let root = tempfile::tempdir().expect("tempdir");
        drop(Engine::open_encrypted(root.path(), KEY).expect("create legacy root store"));
        let pointer_path = root.path().join(generation_meta::GENERATION_META_FILENAME);
        fs::write(
            &pointer_path,
            generation_meta::encode(&generation_meta::GenerationMeta { current_generation: 1 }),
        )
        .expect("write pointer");

        let Err(err) = Engine::open_encrypted(root.path(), KEY) else {
            panic!("missing active generation must fail");
        };
        assert!(matches!(err, EngineError::CorruptGenerationMeta { .. }));
    }

    #[test]
    fn generation_pointer_requires_matching_crypto_meta_generation() {
        let root = tempfile::tempdir().expect("tempdir");
        drop(Engine::open_encrypted(root.path(), KEY).expect("create root store metadata"));
        let generation_dir = root.path().join("gen-1");
        fs::create_dir(&generation_dir).expect("create generation directory");
        // Deliberately write a generation-zero wrap under gen-1: an active
        // pointer must never make this silently usable.
        crypto::create_meta_for_generation(&generation_dir, KEY, crypto::KeyMode::RawKey, 0)
            .expect("create mismatched crypto meta");
        fs::write(
            root.path().join(generation_meta::GENERATION_META_FILENAME),
            generation_meta::encode(&generation_meta::GenerationMeta { current_generation: 1 }),
        )
        .expect("write pointer");

        let Err(err) = Engine::open_encrypted(root.path(), KEY) else {
            panic!("mismatched generation must fail");
        };
        assert!(matches!(err, EngineError::CorruptCryptoMeta { .. }));
    }

    #[test]
    fn published_generation_missing_crypto_meta_is_never_reinitialized() {
        let root = tempfile::tempdir().expect("tempdir");
        {
            let mut engine = Engine::open_encrypted(root.path(), KEY).expect("open");
            engine.put(b"key", b"value").expect("put");
            engine.rotate_key_full(b"fresh key").expect("full rotate");
        }
        fs::remove_file(root.path().join("gen-1").join("crypto.meta")).expect("remove active crypto meta");

        for key in [None, Some(&b"fresh key"[..])] {
            let result = match key {
                Some(key) => Engine::open_encrypted(root.path(), key),
                None => Engine::open(root.path()),
            };
            assert!(matches!(result, Err(EngineError::CorruptCryptoMeta { .. })));
        }
    }

    #[test]
    fn published_generation_without_store_meta_is_not_restamped() {
        let root = tempfile::tempdir().expect("tempdir");
        {
            let mut engine = Engine::open_encrypted(root.path(), KEY).expect("open");
            engine.rotate_key_full(b"fresh key").expect("full rotate");
        }
        fs::remove_file(root.path().join("store.meta")).expect("remove store meta");

        let Err(error) = Engine::open_encrypted(root.path(), b"fresh key") else {
            panic!("missing store.meta must not be recreated for a published generation");
        };
        assert!(matches!(error, EngineError::UnsupportedStoreFormat { found: 0, .. }));
        assert!(!root.path().join("store.meta").exists());
    }

    #[test]
    fn wrong_key_fails_fast_and_typed() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let mut engine = Engine::open_encrypted(dir.path(), KEY).expect("open");
            engine.put(b"a", b"1").expect("put");
        }
        let Err(err) = Engine::open_encrypted(dir.path(), b"not the key") else {
            panic!("wrong key must fail")
        };
        assert!(matches!(err, EngineError::WrongEncryptionKey { .. }));
    }

    #[test]
    fn encrypted_store_without_key_is_missing_key_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            Engine::open_encrypted(dir.path(), KEY).expect("open");
        }
        let Err(err) = Engine::open(dir.path()) else {
            panic!("plaintext open of an encrypted store must fail")
        };
        assert!(matches!(err, EngineError::MissingEncryptionKey { .. }));
    }

    #[test]
    fn encrypted_reopen_rejects_short_plaintext_wal_without_truncating() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            Engine::open_encrypted(dir.path(), KEY).expect("create encrypted meta");
        }
        let wal_path = dir.path().join("wal.log");
        let plaintext = crate::format::wal::encode(crate::format::wal::WalOp::Put, b"a", Some(b"1"));
        std::fs::write(&wal_path, &plaintext).expect("write plaintext wal");

        let Err(err) = Engine::open_encrypted(dir.path(), KEY) else {
            panic!("plaintext wal must be corrupt in encrypted mode")
        };
        assert!(matches!(err, EngineError::CorruptWal { .. }));
        assert_eq!(
            std::fs::metadata(&wal_path).expect("wal metadata").len(),
            plaintext.len() as u64
        );
    }

    #[test]
    fn key_on_existing_plaintext_store_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let mut engine = Engine::open(dir.path()).expect("open plaintext");
            engine.put(b"a", b"1").expect("put");
        }
        let Err(err) = Engine::open_encrypted(dir.path(), KEY) else {
            panic!("a posteriori encryption is refused")
        };
        assert!(matches!(err, EngineError::PlaintextStoreKeySupplied { .. }));
    }

    #[test]
    fn rotation_orphan_tmp_is_ignored_on_reopen() {
        // A crash between tmp-write and rename during rotation leaves a
        // `crypto.meta.tmp` orphan; the store must reopen on the committed
        // wrap as if the rotation never started.
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let mut engine = Engine::open_encrypted(dir.path(), KEY).expect("open");
            engine.put(b"a", b"1").expect("put");
        }
        std::fs::write(dir.path().join("crypto.meta.tmp"), b"torn rotation garbage").expect("write orphan");
        let engine = Engine::open_encrypted(dir.path(), KEY).expect("reopen ignores the orphan");
        assert_eq!(engine.get(b"a").expect("get").as_deref(), Some(&b"1"[..]));
    }
}
