// SPDX-License-Identifier: BUSL-1.1
//! Durable-write primitives and generation GC shared across phases:
//! [`publish_generation`]/[`publish_sst_manifest`] (tmp+fsync+rename) are
//! used by both [`super::open`] and [`super::rotate`] (the latter also
//! shares [`publish_sst_manifest`] with [`super::compact`]'s
//! `apply_version_edit`); [`gc_inactive_generations`]/[`gc_old_generation`]
//! are used by both `open_inner` (routine post-open sweep) and
//! `rotate_full` (immediate old-generation cleanup after publication).

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::{EngineError, Result};
use crate::format::generation_meta;
use crate::format::sst_manifest;

pub(super) fn generation_dir(root_dir: &Path, generation_id: u64) -> PathBuf {
    root_dir.join(format!("gen-{generation_id}"))
}

pub(super) fn publish_generation(root_dir: &Path, generation_id: u64) -> Result<()> {
    let final_path = root_dir.join(generation_meta::GENERATION_META_FILENAME);
    let tmp_path = final_path.with_extension("meta.tmp");
    let bytes = generation_meta::encode(&generation_meta::GenerationMeta {
        current_generation: generation_id,
    });
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
    fs::rename(&tmp_path, &final_path).map_err(|e| EngineError::io(final_path, e))?;
    // ENG-DUR-003/004: the generation pointer's rename must be durable
    // before any caller (`rotate_key_full`) acts on the strength of it —
    // notably the old-generation GC that follows immediately after.
    crate::fs_util::sync_dir(root_dir)?;
    Ok(())
}

/// Publishes `dir`'s durable SST manifest (ENG-DUR-001): tmp+fsync+rename,
/// then [`crate::fs_util::sync_dir`] (ENG-DUR-003) so the rename itself
/// survives a crash before any caller acts on the strength of it. Called
/// after `live_sst_ids`' SSTs are themselves already fsynced and durably
/// renamed — the manifest is the last thing published in a flush/compaction,
/// never the first (same ordering discipline as WAL-after-SST, ADR-025).
pub(super) fn publish_sst_manifest(dir: &Path, manifest_generation: u64, live_sst_ids: &[u64]) -> Result<()> {
    let final_path = dir.join(sst_manifest::SST_MANIFEST_FILENAME);
    let tmp_path = final_path.with_extension("meta.tmp");
    let bytes = sst_manifest::encode(&sst_manifest::SstManifest {
        manifest_generation,
        live_sst_ids: live_sst_ids.to_vec(),
    });
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
    crate::fail_point!("before_sst_manifest_publish");
    fs::rename(&tmp_path, &final_path).map_err(|e| EngineError::io(final_path, e))?;
    crate::fs_util::sync_dir(dir)?;
    Ok(())
}

/// Retries a handful of times before giving up (GC-RETRY-P2, BaseMyAI
/// adversarial audit, 2026-07-22) — the directory/file-removal counterpart
/// to `store::version::remove_old_sst_with_retries`'s discipline, applied
/// here to old-generation GC after a full key/passphrase rotation. Absorbs
/// the same common transient case (a brief antivirus/indexer/backup handle
/// on Windows) that motivated the per-SST version, without blocking for
/// long. On final failure, increments `remove_failures` rather than
/// silently discarding the error — the leftover path is still an inert
/// orphan, swept at the next `Engine::open` (`gc_inactive_generations`),
/// but for a full rotation specifically ("no byte left readable under the
/// old DEK") a silent failure here is worth surfacing.
fn remove_path_with_retries(remove: impl Fn(&Path) -> std::io::Result<()>, path: &Path, remove_failures: &AtomicU64) {
    const ATTEMPTS: u32 = 3;
    for attempt in 0..ATTEMPTS {
        if attempt_remove_path(&remove, path) {
            return;
        }
        if attempt + 1 < ATTEMPTS {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    remove_failures.fetch_add(1, Ordering::Relaxed);
}

/// One removal attempt, with a test-only seam (`during_generation_gc_removal`)
/// so a test can force every attempt in a call to fail deterministically —
/// mirrors `store::version::remove_old_sst_attempt`'s failpoint idiom,
/// applied here to directory/file GC after a full rotation.
fn attempt_remove_path(remove: &impl Fn(&Path) -> std::io::Result<()>, path: &Path) -> bool {
    #[cfg(any(test, feature = "test-util"))]
    if crate::failpoint::hit("during_generation_gc_removal").is_err() {
        return false;
    }
    remove(path).is_ok()
}

pub(super) fn gc_old_generation(
    root_dir: &Path,
    old_dir: &Path,
    current_generation: u64,
    remove_failures: &Arc<AtomicU64>,
) {
    if old_dir == root_dir {
        remove_path_with_retries(|p| fs::remove_file(p), &root_dir.join("wal.log"), remove_failures);
        hit_infallible_failpoint("during_full_rotation_gc");
        remove_path_with_retries(
            |p| fs::remove_file(p),
            &crate::crypto::crypto_meta_path(root_dir),
            remove_failures,
        );
        remove_path_with_retries(
            |p| fs::remove_file(p),
            &root_dir.join("crypto.meta.tmp"),
            remove_failures,
        );
        if let Ok(entries) = fs::read_dir(root_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
                if path.extension().and_then(|extension| extension.to_str()) == Some("sst")
                    || name.ends_with(".sst.tmp")
                {
                    remove_path_with_retries(|p| fs::remove_file(p), &path, remove_failures);
                }
            }
        }
    } else if old_dir != generation_dir(root_dir, current_generation) {
        if let Ok(mut entries) = fs::read_dir(old_dir)
            && let Some(Ok(entry)) = entries.next()
        {
            let path = entry.path();
            if path.is_dir() {
                remove_path_with_retries(|p| fs::remove_dir_all(p), &path, remove_failures);
            } else {
                remove_path_with_retries(|p| fs::remove_file(p), &path, remove_failures);
            }
            hit_infallible_failpoint("during_full_rotation_gc");
        }
        remove_path_with_retries(|p| fs::remove_dir_all(p), old_dir, remove_failures);
    }
}

#[cfg(any(test, feature = "test-util"))]
fn hit_infallible_failpoint(name: &'static str) {
    // Post-publication GC is deliberately best-effort and cannot return an
    // error to the caller. Abort actions still terminate inside `hit`; an
    // injected ordinary error is ignored to preserve that contract.
    let _ = crate::failpoint::hit(name);
}

#[cfg(not(any(test, feature = "test-util")))]
fn hit_infallible_failpoint(_name: &'static str) {}

/// Sweeps every non-current `gen-N` directory, plus (once a generation has
/// actually published, `current_generation != 0`) any generation-0 leftovers
/// still in `root_dir` — the routine post-open GC `open_inner` runs
/// unconditionally after a successful open.
pub(super) fn gc_inactive_generations(root_dir: &Path, current_generation: u64, remove_failures: &Arc<AtomicU64>) {
    let Ok(entries) = fs::read_dir(root_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(id) = name.strip_prefix("gen-").and_then(|id| id.parse::<u64>().ok()) else {
            continue;
        };
        if id != current_generation {
            remove_path_with_retries(|p| fs::remove_dir_all(p), &path, remove_failures);
        }
    }
    if current_generation != 0 {
        gc_old_generation(root_dir, root_dir, current_generation, remove_failures);
    }
}
