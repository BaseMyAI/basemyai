// SPDX-License-Identifier: BUSL-1.1
//! Immutable version set + read snapshots (ADR-043 §2 as amended for
//! ENG-COR-001, milestone J3).
//!
//! The set of live SSTs is published as an immutable [`Version`] — a list of
//! per-file [`SstHandle`]s shared by `Arc` between successive versions (a
//! flush's version shares every pre-existing SST with its predecessor).
//! Publication is always a [`VersionEdit`] applied to the *current* version
//! at commit time, never a wholesale replacement computed from an earlier
//! state — the exact property whose absence ENG-COR-001 demonstrated would
//! lose a concurrently-flushed SST once compaction leaves the writer lock
//! (J4). Under J3 the writer lock still serializes flush/compaction, so the
//! edit is trivially equivalent to a replacement — but the protocol (and its
//! INV-VS-4 validation) is in place so J4 only changes locking, not shape.
//!
//! Physical deletion is deferred (INV-VS-6): an SST superseded by an edit is
//! marked *retired*, and its file is removed only when the last
//! `Arc<SstHandle>` drops — i.e. when no [`Version`] (current or pinned by a
//! [`Snapshot`]) references it anymore. A handle that was never retired
//! deletes nothing on drop: dropping the `Engine` must never destroy the
//! store. Removal failure is best-effort, mirroring `compact()`'s historical
//! posture: the manifest already excludes the file, so a leftover is an
//! inert orphan swept at the next open (`confront_manifest_with_disk`),
//! never a resurrection risk — and `Drop` never panics.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::error::Result;
use crate::fail_point;
use crate::key::Key;
use crate::store::Value;
use crate::store::block_cache::BlockCache;
use crate::store::sst_block::BlockSstFile;

/// One live SST file, shared by `Arc` between every [`Version`] that
/// contains it. Immutable apart from the one-way `retired` flag.
pub(crate) struct SstHandle {
    pub(crate) file: BlockSstFile,
    /// Armed only by the [`VersionEdit`] that removes this id from the
    /// manifest (or never, for handles superseded by a full rotation —
    /// their whole generation directory is GC'd separately). Never set by
    /// default: an ordinary `Engine` drop must not delete live SSTs.
    retired: AtomicBool,
    /// Shared with the owning `Engine` so deferred removals that still fail
    /// after retries stay observable via
    /// `EngineStats::compaction_remove_failures`, even when the drop
    /// happens outside any `&mut Engine` context (a snapshot released after
    /// the compaction that retired the file).
    remove_failures: Arc<AtomicU64>,
}

impl SstHandle {
    pub(crate) fn new(file: BlockSstFile, remove_failures: Arc<AtomicU64>) -> Arc<Self> {
        Arc::new(Self {
            file,
            retired: AtomicBool::new(false),
            remove_failures,
        })
    }

    /// Marks this SST as superseded: its file will be removed when the last
    /// `Arc` referencing it drops. One-way — there is no un-retire.
    pub(crate) fn retire(&self) {
        self.retired.store(true, Ordering::Release);
    }
}

impl Drop for SstHandle {
    fn drop(&mut self) {
        // `&mut self` proves exclusive access — `get_mut` reads the flag
        // without an atomic RMW. No panic path below: removal failure is
        // counted, the leftover file is an inert orphan (the manifest
        // already excludes it), swept at the next open.
        if *self.retired.get_mut() && !remove_old_sst_with_retries(&self.file.path) {
            self.remove_failures.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// An immutable published set of live SSTs (oldest to newest — the same
/// invariant `Engine::ssts` kept before this type existed). Never mutated
/// after publication (INV-VS-1); the engine replaces its `Arc<Version>`
/// wholesale (INV-VS-2).
pub(crate) struct Version {
    /// The durable manifest publication counter this version was published
    /// under — `manifest.meta`'s `manifest_generation` (INV-VS-7).
    pub(crate) manifest_generation: u64,
    /// Oldest to newest.
    pub(crate) ssts: Vec<Arc<SstHandle>>,
}

impl Version {
    /// Live SST ids, oldest to newest — exactly what `manifest.meta` lists.
    pub(crate) fn ids(&self) -> Vec<u64> {
        self.ssts.iter().map(|h| h.file.id).collect()
    }
}

/// A publication delta (ADR-043 §2 amended): applied to the current
/// [`Version`] at commit time as `V_next = (V_current ∖ deleted) ∪ added`.
/// A flush is `{ added: [S_new], deleted: [] }`; a compaction is
/// `{ added: [S_out], deleted: inputs }`. Every id in `deleted` must be
/// present in the version the edit is applied to (INV-VS-4) — which is what
/// keeps an SST flushed *during* a J4 out-of-lock compaction alive
/// (INV-VS-5): it is not among the merge's inputs, so it survives into
/// `V_next` untouched.
pub(crate) struct VersionEdit {
    pub(crate) added: Vec<Arc<SstHandle>>,
    pub(crate) deleted: Vec<u64>,
}

/// A stable read view over a pinned [`Version`] — an **S1 snapshot**
/// (audit §6, ADR-043 §2 amended): it freezes the *files*, not the *view*.
/// The engine's memtable is not captured — a write, flush or compaction
/// after [`Engine::snapshot`](crate::Engine::snapshot) stays visible
/// through the `Engine` API and invisible here; conversely, an unflushed
/// write present in the memtable at snapshot time is *not* visible through
/// this snapshot. What it guarantees: every SST of the pinned version stays
/// on disk and readable for this snapshot's whole lifetime, regardless of
/// how many compactions supersede it (INV-VS-6).
///
/// Limits (documented, not defended against): a snapshot does not usefully
/// survive `Engine::rotate_key_full` (the old generation directory is GC'd
/// wholesale — later reads fail with a typed I/O error, never a panic) nor
/// the drop of the `Engine` followed by a reopen (the reopen sweeps the
/// pinned files as manifest orphans).
pub struct Snapshot {
    version: Arc<Version>,
    /// Private block cache — snapshot reads never populate the engine's
    /// cache with blocks from SSTs the current version may already have
    /// dropped. Capacity 0 keeps at most the one most-recent block.
    cache: BlockCache,
    /// Decremented on drop; feeds `EngineStats::active_snapshots`.
    active: Arc<AtomicU64>,
}

impl Snapshot {
    pub(crate) fn new(version: Arc<Version>, active: Arc<AtomicU64>) -> Self {
        active.fetch_add(1, Ordering::Relaxed);
        Self {
            version,
            cache: BlockCache::new(0),
            active,
        }
    }

    /// Point lookup over the pinned SST layers, newest to oldest — the same
    /// layering rule as [`Engine::get`](crate::Engine::get) minus the
    /// memtable (S1: files, not view). `None` for a key absent from every
    /// pinned layer *or* tombstoned in its newest one.
    pub fn get(&self, key: &[u8]) -> Result<Option<Value>> {
        let key = Key::from(key);
        for h in self.version.ssts.iter().rev() {
            let (hit, _blocks_read) = h.file.get(&key, &self.cache)?;
            if let Some(value) = hit {
                return Ok(value);
            }
        }
        Ok(None)
    }

    /// Prefix scan over the pinned SST layers (oldest to newest, later
    /// layers overwrite earlier ones, tombstones dropped) — the same merge
    /// rule as [`Engine::scan_prefix`](crate::Engine::scan_prefix) minus
    /// the memtable.
    pub fn scan_prefix(&self, prefix: &[u8]) -> Result<Vec<(Key, Value)>> {
        let mut merged: std::collections::BTreeMap<Key, Option<Value>> = std::collections::BTreeMap::new();
        for h in &self.version.ssts {
            let (matches, _blocks_read) = h.file.entries_with_prefix(prefix)?;
            for (k, v) in matches {
                merged.insert(k, v);
            }
        }
        Ok(merged
            .into_iter()
            .filter_map(|(k, v)| v.map(|value| (k, value)))
            .collect())
    }

    /// Number of SST files this snapshot pins.
    #[must_use]
    pub fn sst_count(&self) -> usize {
        self.version.ssts.len()
    }
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::Relaxed);
    }
}

/// One removal attempt for a retired SST — a thin seam so
/// `during_compaction_sst_removal` can simulate a single failed attempt
/// without aborting anything (the historical failpoint site, kept under the
/// same name now that the attempt runs at handle drop instead of inline in
/// `compact()`).
fn remove_old_sst_attempt(path: &Path) -> Result<()> {
    fail_point!("during_compaction_sst_removal");
    std::fs::remove_file(path).map_err(|e| crate::error::EngineError::io(path.to_path_buf(), e))
}

/// Retries a handful of times before giving up (ENG-DUR-002 minimal
/// correction) — absorbs the common transient case (a brief antivirus/
/// indexer/backup handle on Windows, the scenario the audit demonstrates)
/// without blocking for long. Returns `false` if every attempt failed; the
/// caller counts that rather than silently ignoring it.
fn remove_old_sst_with_retries(path: &Path) -> bool {
    const ATTEMPTS: u32 = 3;
    for attempt in 0..ATTEMPTS {
        if remove_old_sst_attempt(path).is_ok() {
            return true;
        }
        if attempt + 1 < ATTEMPTS {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_is_send_and_sync() {
        // J4 hands a snapshot to the out-of-lock merge; pin the auto-traits
        // now so a future field doesn't silently un-Send it.
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Snapshot>();
        assert_send_sync::<Version>();
    }
}
