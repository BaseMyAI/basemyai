// SPDX-License-Identifier: BUSL-1.1
//! Flush, compaction, and version publishing — kept as one file rather than
//! split further: `apply_version_edit` is the documented shared choke point
//! both `flush` and `compact_commit` rely on (INV-VS-3/4/5), and `flush`
//! conditionally triggers `compact` — separating them would fragment a
//! genuinely indivisible unit. Also covers the off-write-lock compaction
//! protocol (ADR-043 §3/J4): [`Engine::compaction_snapshot`]/
//! [`Engine::compact_prepare`] capture cheaply under `&self`,
//! [`CompactionSnapshot::build`] runs the merge with nothing locked, and
//! [`Engine::compact_commit`] publishes the result under a brief `&mut self`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::crypto::CryptoContext;
use crate::error::{EngineError, Result};
use crate::fail_point;
use crate::key::Key;
use crate::store::Value;
use crate::store::sst_block::BlockSstFile;
use crate::store::version::{SstHandle, Version, VersionEdit};

use super::Engine;
use super::io::publish_sst_manifest;

/// Everything [`CompactionSnapshot::build`] needs to run a compaction merge
/// — captured cheaply by [`Engine::compaction_snapshot`]/
/// [`Engine::capture_compaction_snapshot`] (CONC-P1 fix, ADR-043 §3/J4
/// amended). Deliberately holds no reference to `Engine`: every field is
/// either `Arc`-shared or a cheap owned clone, so `build` can run with
/// nothing else locked — not `Engine`, not whatever outer structure a
/// caller wraps `Engine` in. Opaque outside this crate: a caller can only
/// obtain one from `Engine` and hand it to `build`, never construct or
/// inspect it directly.
pub struct CompactionSnapshot {
    input: Arc<Version>,
    next_sst_id: Arc<AtomicU64>,
    dir: PathBuf,
    block_size: u32,
    crypto: Option<CryptoContext>,
    sst_remove_failures: Arc<AtomicU64>,
}

impl CompactionSnapshot {
    /// The actual merge: folds every SST in `input` (oldest to newest, later
    /// writes win) into a single new SST, dropping tombstones entirely —
    /// safe because `input` covers *all* existing data at capture time, so a
    /// deleted key has no older layer left to resurrect from. Correctness
    /// first; a tiered/leveled strategy is deferred (ADR-025). Reserves the
    /// output SST's id from the shared counter at the exact point its
    /// content is frozen (right before `BlockSstFile::write_new`), matching
    /// `Engine::flush`'s discipline — the property `Version::build`
    /// (INV-VS-8) relies on to make id order equivalent to visibility order.
    ///
    /// No lock beyond what producing this `CompactionSnapshot` already
    /// required (CONC-P1 fix): this can run for as long as it needs to
    /// without blocking a single reader or writer anywhere.
    ///
    /// # Errors
    /// I/O or corruption errors reading the input SSTs or writing the merged
    /// output.
    pub fn build(self) -> Result<CompactionJob> {
        fail_point!("during_compaction");
        let input_bytes: u64 = self.input.ssts().iter().map(|h| h.file.file_bytes).sum();
        let mut merged: BTreeMap<Key, Option<Value>> = BTreeMap::new();
        for h in self.input.ssts() {
            for (k, v) in h.file.entries()? {
                merged.insert(k, v);
            }
        }
        let entries: Vec<(Key, Option<Value>)> = merged.into_iter().filter(|(_, v)| v.is_some()).collect();

        let id = self.next_sst_id.fetch_add(1, Ordering::Relaxed);
        let new_sst = BlockSstFile::write_new(&self.dir, id, entries, self.block_size, self.crypto.as_ref())?;
        let output_bytes = new_sst.file_bytes;
        let deleted: Vec<u64> = self.input.ssts().iter().map(|h| h.file.id).collect();
        Ok(CompactionJob {
            merged: SstHandle::new(new_sst, Arc::clone(&self.sst_remove_failures)),
            deleted,
            input_bytes,
            output_bytes,
        })
    }
}

/// A compaction merge already written to disk, staged for a brief exclusive
/// commit (ADR-043 §3/J4). Produced by [`Engine::compact_prepare`] off the
/// write lock (`&self`, no mutation beyond an atomic SST-id reservation);
/// consumed by [`Engine::compact_commit`] (`&mut self`, no merge work — just
/// the `VersionEdit` and its deferred counters). Opaque outside this crate:
/// its fields are private, so a caller can only carry it from one call to
/// the other, never construct or inspect it directly.
pub struct CompactionJob {
    merged: Arc<SstHandle>,
    deleted: Vec<u64>,
    input_bytes: u64,
    output_bytes: u64,
}

impl Engine {
    /// Forces the memtable out to a new SST regardless of the configured
    /// threshold, then truncates the WAL — in that order (ADR-025). A no-op
    /// if the memtable is empty.
    pub fn flush(&mut self) -> Result<()> {
        if self.memtable.is_empty() {
            return Ok(());
        }
        let entries: Vec<(Key, Option<Value>)> = self.memtable.iter().map(|(k, v)| (k.clone(), v.clone())).collect();

        // Reserved up front (not incremented after the write) — same
        // reasoning as before this counter became atomic: a caller that
        // keeps using this instance after a later error must never
        // `write_new` the same id twice, overwriting a file the published
        // version already reads.
        let id = self.next_sst_id.fetch_add(1, Ordering::Relaxed);
        let new_sst = BlockSstFile::write_new(&self.dir, id, entries, self.options.block_size, self.crypto.as_ref())?;
        self.counters.flush_count += 1;
        self.counters.bytes_written += new_sst.file_bytes;
        self.counters.fsync_count += 1; // write_new's one sync_all of the new SST

        // ENG-DUR-001: publish the manifest listing this new SST *before*
        // truncating the WAL — SST → manifest → WAL truncate, in that exact
        // order (the roadmap's own ordering rule for J2). If a crash lands
        // before the manifest is durable, the WAL still holds the untouched
        // tail to replay from, and `confront_manifest_with_disk` correctly
        // treats the new SST as not-yet-live (dropped as an orphan) on
        // reopen — never a case where truncated data depended on a
        // manifest entry that was never published. A flush is the edit
        // `{ added: [S_new], deleted: [] }` (ADR-043 §2 amended).
        self.apply_version_edit(VersionEdit {
            added: vec![SstHandle::new(new_sst, Arc::clone(&self.sst_remove_failures))],
            deleted: Vec::new(),
        })?;

        // The new SST *and* its manifest entry are fsynced and durably
        // renamed at this point — only now is it safe to truncate the WAL
        // (ADR-025 ordering rule).
        fail_point!("before_wal_truncate");
        self.wal.reset()?;

        self.memtable.clear();

        // `auto_compact_on_flush` (ADR-043 §3/J4, `EngineOptions` doc):
        // `false` callers (`NativeInner`) poll `compaction_pending` and run
        // the merge off their own lock instead — inline here would hold
        // *this* call's exclusive access (whatever the caller wraps `Engine`
        // in) for the whole merge, defeating the point. `true` (default)
        // callers have nothing else driving compaction for them, so this
        // stays the safety net.
        if self.options.auto_compact_on_flush && self.compaction_pending() {
            self.compact()?;
        }
        Ok(())
    }

    /// Cheap opportunistic check (ADR-043 §3/J4): `true` once the live SST
    /// count exceeds `compaction_sst_threshold`, i.e. a [`Self::compact_prepare`]
    /// call would return `Some`. `flush()` no longer triggers compaction
    /// itself (see that method's doc) — a caller with a natural place to run
    /// the merge off any exclusive lock it holds (`NativeInner::with_inner`)
    /// polls this right after a flush to decide whether to schedule one.
    /// `&self`, one integer comparison — safe to call as often as needed.
    #[must_use]
    pub fn compaction_pending(&self) -> bool {
        self.current.ssts().len() > self.options.compaction_sst_threshold
    }

    /// Overrides `EngineOptions::auto_compact_on_flush` after open (ADR-043
    /// §3/J4) — no `open*` constructor takes this per-call, since it's a
    /// property of *how* a caller wraps `Engine`, not of the store on disk.
    /// `NativeInner` is the one production caller that needs `false`: it
    /// drives compaction itself, off its own lock, via
    /// [`Self::compaction_pending`]/[`Self::compact_prepare`]/
    /// [`Self::compact_commit`] from `with_inner`. Every other caller
    /// (standalone binaries, test harnesses — anything with no equivalent
    /// external scheduler) keeps the safe default (`true`).
    pub fn set_auto_compact_on_flush(&mut self, enabled: bool) {
        self.options.auto_compact_on_flush = enabled;
    }

    /// Operator-triggered compaction (ADR-040 §3, N9.4 — the engine half of
    /// the `basemyai compact` CLI surface): flushes any pending memtable
    /// data, then runs the full-merge compaction unconditionally — unlike
    /// the automatic path, which only fires past
    /// `EngineOptions::compaction_sst_threshold`. Useful below the
    /// threshold too: merging even a single SST rewrites it without its
    /// tombstones (safe here for the same reason as the automatic path —
    /// the merge covers *all* existing data, so a deleted key has no older
    /// layer left to resurrect from). A no-op on a store with no SSTs and
    /// nothing to flush.
    pub fn compact_now(&mut self) -> Result<()> {
        self.flush()?;
        if self.current.ssts().is_empty() {
            return Ok(());
        }
        self.compact()
    }

    /// Cheap, `&self`-only capture of everything a compaction merge needs
    /// (CONC-P1 fix, ADR-043 §3/J4 amended): a handful of `Arc` clones plus
    /// one `PathBuf`/`CryptoContext` clone — no I/O, no allocation
    /// proportional to store size. The point: this is a *value*, not a
    /// borrow of `Engine`, so a caller wrapping `Engine` in its own outer
    /// lock (`NativeMemoryStore`'s `RwLock<NativeInner>`) can take that lock
    /// just long enough to call this, release it, and only then run
    /// [`CompactionSnapshot::build`] — the actual RAM-materialize-and-write
    /// pass, potentially slow — with **no lock of its own held at all**, not
    /// even a shared read lock. Before this existed, `NativeMemoryStore::
    /// run_pending_compaction` held its read lock for the merge's entire
    /// duration (calling `compact_prepare` from inside it), which blocked
    /// every writer for that whole time even though `Engine`'s own API was
    /// already lock-free at this level — see `docs/adr/
    /// ADR-043-native-version-set-snapshots-and-concurrent-compaction.md`'s
    /// third amendment.
    fn capture_compaction_snapshot(&self) -> CompactionSnapshot {
        CompactionSnapshot {
            input: Arc::clone(&self.current),
            next_sst_id: Arc::clone(&self.next_sst_id),
            dir: self.dir.clone(),
            block_size: self.options.block_size,
            crypto: self.crypto.clone(),
            sst_remove_failures: Arc::clone(&self.sst_remove_failures),
        }
    }

    /// Naive full-merge compaction, run unconditionally regardless of
    /// [`EngineOptions::compaction_sst_threshold`] — the shared core behind
    /// both [`Self::compact_prepare`]'s opportunistic, threshold-gated path
    /// and [`Self::compact`]/[`Self::compact_now`]'s "merge even a single
    /// SST" contract (that method's doc: rewriting drops tombstones even
    /// below threshold — a real behavior `compact_now_merges_below_threshold_and_purges_tombstones`
    /// and the J3 snapshot tests pin).
    fn build_compaction_job(&self) -> Result<CompactionJob> {
        self.capture_compaction_snapshot().build()
    }

    /// Prepares a compaction merge off the write lock (ADR-043 §3/J4):
    /// `&self` only, the opportunistic counterpart to
    /// [`Self::build_compaction_job`] — `None` once the live SST count no
    /// longer exceeds `compaction_sst_threshold` ([`Self::compaction_pending`]),
    /// i.e. nothing to do. A caller with a natural place to run this off any
    /// exclusive lock it holds (`NativeInner::with_inner`, under a shared
    /// read lock) calls this, then commits the returned job with
    /// [`Self::compact_commit`] under a brief exclusive lock — never holding
    /// any lock for the merge itself.
    ///
    /// The returned [`CompactionJob`] already holds a fully-written SST on
    /// disk; if the caller never commits it, that file simply becomes an
    /// ordinary orphan swept at the next open (`confront_manifest_with_disk`)
    /// — inert, not a correctness risk, since nothing ever links it into any
    /// [`Version`].
    ///
    /// # Errors
    /// I/O or corruption errors reading the input SSTs or writing the merged
    /// output.
    pub fn compact_prepare(&self) -> Result<Option<CompactionJob>> {
        if !self.compaction_pending() {
            return Ok(None);
        }
        self.build_compaction_job().map(Some)
    }

    /// Public, decomposed counterpart to [`Self::compact_prepare`] for a
    /// caller that itself sits behind an outer lock and must not hold that
    /// lock for the merge's duration (CONC-P1 fix — `NativeMemoryStore::
    /// run_pending_compaction` is the one production caller): capture
    /// under a brief lock acquisition via this method, release the lock,
    /// then call [`CompactionSnapshot::build`] with nothing held. `None`
    /// under the same threshold gate as `compact_prepare`
    /// ([`Self::compaction_pending`]).
    #[must_use]
    pub fn compaction_snapshot(&self) -> Option<CompactionSnapshot> {
        self.compaction_pending().then(|| self.capture_compaction_snapshot())
    }

    /// Commits a [`CompactionJob`] prepared by [`Self::compact_prepare`]
    /// (ADR-043 §3/J4): publishes its [`VersionEdit`] — `{ added: [merged],
    /// deleted: inputs }` — against `self.current` **at this instant**, not
    /// the snapshot the merge started from (INV-VS-3).
    /// [`Self::apply_version_edit`]'s INV-VS-4 validation is what makes this
    /// safe even when a flush published a new SST while the merge ran
    /// off-lock: that SST is absent from `deleted` (fixed at prepare time),
    /// so it survives into `V_next` untouched (INV-VS-5, ENG-COR-001).
    /// Deferred counters — skipped by `compact_prepare`/`build_compaction_job`,
    /// which must not mutate `self` beyond the atomic id reservation — are
    /// applied here, under the same exclusive access as the edit, and only
    /// once the edit actually publishes (see below). `&mut self`, brief: no
    /// merge work happens in this call, only the edit (manifest write +
    /// `Arc` swap).
    ///
    /// # Errors
    /// [`EngineError::VersionEditMissingInput`] if a concurrent compaction
    /// already retired one of `job`'s input ids — reachable under J4 (unlike
    /// under J3's exclusive lock) because `compact_prepare` runs under a
    /// *shared* read lock: two callers can both stage a job over the same
    /// input set before either commits, and the second commit is refused
    /// typed rather than publishing an incoherent manifest. Callers driving
    /// this opportunistically (`NativeInner::with_inner`, ADR-043 §3/J4)
    /// treat that as a harmless lost race, not a failure of whatever write
    /// triggered the attempt; plus I/O errors publishing the manifest.
    pub fn compact_commit(&mut self, job: CompactionJob) -> Result<()> {
        let (input_bytes, output_bytes) = (job.input_bytes, job.output_bytes);

        // ENG-DUR-001/ENG-DUR-002: the edit publishes the manifest *before*
        // any old file can be removed — from that point a leftover old SST
        // is an orphan per the manifest, never resurrectable as live on a
        // future reopen. Physical removal itself is deferred to each
        // handle's last-`Arc` drop (INV-VS-6): with no snapshot alive that
        // happens synchronously inside this call (the old version's only
        // reference was `self.current`, replaced by the edit), preserving
        // the historical inline-removal timing; with a snapshot alive the
        // files persist, still readable, until the snapshot drops.
        //
        // Counters are applied only after this succeeds — `apply_version_edit`
        // promises "on error nothing is published and `self.current` is
        // unchanged" (its own doc); a rejected edit (see `# Errors` above)
        // must not inflate `compaction_count`/`bytes_written`/etc. as if a
        // compaction had actually happened.
        self.apply_version_edit(VersionEdit {
            added: vec![job.merged],
            deleted: job.deleted,
        })?;
        self.counters.compaction_count += 1;
        self.counters.compaction_input_bytes += input_bytes;
        self.counters.compaction_output_bytes += output_bytes;
        self.counters.bytes_written += output_bytes;
        self.counters.fsync_count += 1; // write_new's one sync_all of the merged output SST
        Ok(())
    }

    /// Naive full-merge compaction, unconditional (see
    /// [`Self::build_compaction_job`]'s doc) — the synchronous path used by
    /// [`Self::compact_now`], still `&mut self` for its whole duration:
    /// callers that need the merge off the write lock use
    /// [`Self::compact_prepare`]/[`Self::compact_commit`] instead
    /// (ADR-043 §3/J4).
    fn compact(&mut self) -> Result<()> {
        let job = self.build_compaction_job()?;
        self.compact_commit(job)
    }

    /// Publishes `edit` (ADR-043 §2 amended, J3): validates that every
    /// `deleted` id is present in the current version (INV-VS-4), computes
    /// `V_next = (V_current ∖ deleted) ∪ added`, publishes `manifest.meta`
    /// listing exactly `V_next` (INV-VS-7), then atomically replaces
    /// `self.current` (INV-VS-2) after retiring the deleted handles for
    /// deferred physical removal (INV-VS-6). On error nothing is published
    /// and `self.current` is unchanged.
    ///
    /// A `put`/`delete`/`apply_batch`-triggered `flush` always calls this
    /// still under its own exclusive `&mut self`, so the current version
    /// cannot move between it reading `self.current` and this commit — the
    /// validation is a no-op there. It earns its keep for
    /// [`Self::compact_commit`] (ADR-043 §3/J4): the merge that built its
    /// `CompactionJob` ran off-lock, so `self.current` may have advanced (a
    /// concurrent flush) by the time this runs — this validation is what
    /// makes that safe, refusing typed instead of publishing a manifest that
    /// silently drops a concurrently-flushed SST (ENG-COR-001).
    pub(super) fn apply_version_edit(&mut self, edit: VersionEdit) -> Result<()> {
        let current_ids: std::collections::HashSet<u64> = self.current.ssts().iter().map(|h| h.file.id).collect();
        for id in &edit.deleted {
            if !current_ids.contains(id) {
                return Err(EngineError::VersionEditMissingInput { id: *id });
            }
        }
        let deleted: std::collections::HashSet<u64> = edit.deleted.iter().copied().collect();
        // Order here is irrelevant — `Version::build` (INV-VS-8) sorts by id
        // and rejects duplicates in debug builds. This used to be a
        // hand-assembled `Vec` (`filter` the survivors, then `extend` with
        // `edit.added`), which was correct for a flush but silently wrong
        // for an off-lock compaction commit: `edit.added`'s merged SST can
        // carry a *lower* id than a survivor flushed after the merge's input
        // snapshot was taken (`compact_prepare` reserves its output id
        // before that later flush reserves its own), so appending it
        // unconditionally put stale data after fresher data in the vector —
        // exactly DUR-LSM-01. Letting `Version::build` canonicalize removes
        // the possibility structurally rather than relying on every call
        // site to assemble things in the right order.
        let mut next_ssts: Vec<_> = self
            .current
            .ssts()
            .iter()
            .filter(|h| !deleted.contains(&h.file.id))
            .map(Arc::clone)
            .collect();
        next_ssts.extend(edit.added);
        let next = Version::build(self.current.manifest_generation + 1, next_ssts);
        publish_sst_manifest(&self.dir, next.manifest_generation, &next.ids())?;
        self.counters.fsync_count += 1; // publish_sst_manifest's one sync_all

        // The manifest no longer lists the deleted ids: retire their
        // handles (their files are removed at last-`Arc` drop) and evict
        // their cached blocks now — the cache belongs to the live view, and
        // a snapshot re-reading a retired SST uses its own private cache.
        for h in self.current.ssts() {
            if deleted.contains(&h.file.id) {
                self.block_cache.invalidate_sst(h.file.id);
                h.retire();
            }
        }
        // Single visibility point (INV-VS-2). Dropping the old version's
        // `Arc` here is what triggers the synchronous removal in the
        // no-snapshot case.
        self.current = Arc::new(next);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::sst_manifest;
    use crate::store::engine::test_support::{KEY, small_options};

    #[test]
    fn compact_now_merges_below_threshold_and_purges_tombstones() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut engine = Engine::open_encrypted_with_options(dir.path(), KEY, small_options()).expect("open");
        for i in 0..8u32 {
            engine
                .put(format!("key-{i:03}").as_bytes(), format!("value-{i}").as_bytes())
                .expect("put");
        }
        engine.delete(b"key-002").expect("delete");
        engine.delete(b"key-005").expect("delete");
        // Leave an unflushed tail too: compact_now must fold it in.
        engine.put(b"tail", b"unflushed").expect("put tail");

        engine.compact_now().expect("compact");
        let stats = engine.stats().expect("stats");
        assert_eq!(stats.sst_count, 1, "everything folds into one SST");
        assert_eq!(stats.tombstone_count, 0, "a full merge drops every tombstone");
        assert_eq!(stats.wal_bytes, 0, "flushed before compacting");

        drop(engine);
        let engine = Engine::open_encrypted_with_options(dir.path(), KEY, small_options()).expect("reopen");
        assert_eq!(engine.get(b"key-000").expect("get").as_deref(), Some(&b"value-0"[..]));
        assert_eq!(engine.get(b"key-002").expect("get"), None);
        assert_eq!(engine.get(b"key-005").expect("get"), None);
        assert_eq!(engine.get(b"tail").expect("get").as_deref(), Some(&b"unflushed"[..]));
    }

    #[test]
    fn compact_now_on_an_empty_store_is_a_noop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut engine = Engine::open(dir.path()).expect("open");
        engine.compact_now().expect("compact empty");
        let stats = engine.stats().expect("stats");
        assert_eq!(stats.sst_count, 0);
        assert_eq!(stats.compaction_count, 0, "nothing to compact, nothing counted");
    }

    /// INV-VS-4 (ADR-043 §2 amendé) : un `VersionEdit` dont un id de
    /// `deleted` n'est pas dans le `Version` courant est refusé typé, sans
    /// rien publier — ni manifest, ni bascule de `current`. Inatteignable
    /// via l'API publique tant que flush/compaction tiennent `&mut self`
    /// (J3) ; forgé ici directement, c'est le garde-fou que J4 (compaction
    /// hors verrou) viendra exercer pour de vrai.
    #[test]
    fn forged_version_edit_with_unknown_deleted_id_is_refused_and_publishes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut engine = Engine::open(dir.path()).expect("open");
        engine.put(b"a", b"1").expect("put");
        engine.flush().expect("flush -> SST 0");

        let manifest_path = dir.path().join(sst_manifest::SST_MANIFEST_FILENAME);
        let manifest_before = std::fs::read(&manifest_path).expect("read manifest before");
        let ids_before = engine.current.ids();
        let generation_before = engine.current.manifest_generation;

        let err = engine
            .apply_version_edit(VersionEdit {
                added: Vec::new(),
                deleted: vec![999],
            })
            .expect_err("an edit deleting an id absent from the current version must be refused");
        assert!(matches!(err, EngineError::VersionEditMissingInput { id: 999 }));

        // Rien n'a été publié : manifest bit-à-bit identique, `current`
        // inchangé (génération et ids), moteur toujours pleinement utilisable.
        assert_eq!(
            std::fs::read(&manifest_path).expect("read manifest after"),
            manifest_before,
            "a refused edit must not publish any manifest"
        );
        assert_eq!(engine.current.manifest_generation, generation_before);
        assert_eq!(engine.current.ids(), ids_before);
        assert_eq!(engine.get(b"a").expect("get").as_deref(), Some(&b"1"[..]));
        engine
            .put(b"b", b"2")
            .expect("the engine stays fully usable after a refused edit");
        assert_eq!(engine.get(b"b").expect("get").as_deref(), Some(&b"2"[..]));
    }
}
