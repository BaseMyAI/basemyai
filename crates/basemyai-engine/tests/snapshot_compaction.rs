// SPDX-License-Identifier: BUSL-1.1
//! ADR-043 §2 (amended for ENG-COR-001), milestone J3: immutable version
//! set, S1 read snapshots, and deferred physical removal of superseded SSTs.
//!
//! The J3 exit criterion from the ADR's own list, made executable: *"Aucune
//! SST n'est supprimée du disque tant qu'un `Snapshot` la référence encore —
//! test qui prend un snapshot, déclenche une compaction qui la remplacerait,
//! vérifie que le fichier existe toujours et reste lisible par le snapshot,
//! puis vérifie sa suppression après libération du snapshot."*
//!
//! Determinism note: deferred removal runs inline in the last
//! `Arc<SstHandle>` drop, so `drop(snapshot)` is the synchronization point —
//! no sleeps anywhere. Every test takes the file-wide mutex because the
//! failpoint registry is process-global and one test below arms
//! `during_compaction_sst_removal`, which would otherwise fire in a
//! concurrently-running test's own removals.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use basemyai_engine::failpoint::{self, Action};
use basemyai_engine::{Engine, EngineError, EngineOptions};

fn lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

struct ClearOnDrop;
impl Drop for ClearOnDrop {
    fn drop(&mut self) {
        failpoint::clear_all();
    }
}

/// One SST per write, no automatic compaction — `compact_now()` is the only
/// trigger. Same idiom as `tests/compaction_remove_retry.rs`.
fn small_options() -> EngineOptions {
    EngineOptions {
        memtable_flush_threshold: 1,
        compaction_sst_threshold: 100,
        block_size: 256,
        ..EngineOptions::default()
    }
}

fn sst_path(dir: &Path, id: u64) -> PathBuf {
    dir.join(format!("{id:020}.sst"))
}

/// The mandated end-to-end test: pin → compact → old files survive and stay
/// readable through the snapshot → drop → files removed → the current
/// version lost nothing.
#[test]
fn snapshot_pins_superseded_ssts_until_drop_and_current_loses_nothing() {
    let _serial = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let mut engine = Engine::open_with_options(dir.path(), small_options()).expect("open");

    // Three deterministic SSTs: value, value, tombstone (ids 0, 1, 2).
    engine.put(b"a", b"1").expect("put a -> SST 0");
    engine.put(b"b", b"2").expect("put b -> SST 1");
    engine.delete(b"a").expect("delete a -> SST 2 (tombstone)");
    assert_eq!(engine.stats().expect("stats").sst_count, 3);
    let pinned: Vec<PathBuf> = (0..3).map(|id| sst_path(dir.path(), id)).collect();
    for p in &pinned {
        assert!(p.exists(), "{} must exist before the snapshot", p.display());
    }

    let snap = engine.snapshot();
    assert_eq!(snap.sst_count(), 3);
    assert_eq!(engine.stats().expect("stats").active_snapshots, 1);

    // A write AFTER the snapshot (SST 3), then a full compaction (SST 4).
    engine.put(b"c", b"3").expect("put c -> SST 3");
    engine.compact_now().expect("compact");
    assert_eq!(engine.stats().expect("stats").sst_count, 1);

    // The snapshot's three SSTs must survive the compaction that superseded
    // them (INV-VS-6)…
    for p in &pinned {
        assert!(p.exists(), "{} must survive while the snapshot lives", p.display());
    }
    // …while SST 3 — retired by the same compaction but pinned by no
    // snapshot — is already gone: retention is per-SST, not "everything
    // that ever existed".
    assert!(
        !sst_path(dir.path(), 3).exists(),
        "SST 3 is referenced by no snapshot and must be removed by the compaction"
    );

    // The snapshot still reads the exact pinned state (S1: the files at
    // snapshot time — the tombstone for `a` was already flushed, `c` came
    // later and is invisible).
    assert_eq!(snap.get(b"b").expect("snap get b"), Some(b"2".to_vec()));
    assert_eq!(
        snap.get(b"a").expect("snap get a"),
        None,
        "tombstone visible at pin time"
    );
    assert_eq!(snap.get(b"c").expect("snap get c"), None, "written after the pin");
    assert_eq!(
        snap.scan_prefix(b"").expect("snap scan"),
        vec![(b"b".as_slice().into(), b"2".to_vec())],
        "scan over the pinned layers: `a` tombstoned, `c` absent"
    );

    // The current version lost nothing.
    assert_eq!(engine.get(b"a").expect("get a"), None);
    assert_eq!(engine.get(b"b").expect("get b"), Some(b"2".to_vec()));
    assert_eq!(engine.get(b"c").expect("get c"), Some(b"3".to_vec()));

    // Releasing the snapshot is the deterministic point where the deferred
    // removals run — no sleeps, no background thread.
    drop(snap);
    assert_eq!(engine.stats().expect("stats").active_snapshots, 0);
    for p in &pinned {
        assert!(
            !p.exists(),
            "{} must be gone once the last snapshot dropped",
            p.display()
        );
    }

    // Still nothing lost, live or across a reopen (manifest is the source
    // of truth).
    assert_eq!(engine.get(b"b").expect("get b"), Some(b"2".to_vec()));
    assert_eq!(engine.get(b"c").expect("get c"), Some(b"3".to_vec()));
    drop(engine);
    let reopened = Engine::open_with_options(dir.path(), small_options()).expect("reopen");
    assert_eq!(reopened.get(b"a").expect("get a"), None);
    assert_eq!(reopened.get(b"b").expect("get b"), Some(b"2".to_vec()));
    assert_eq!(reopened.get(b"c").expect("get c"), Some(b"3".to_vec()));
}

/// Pins the documented S1 semantics (ADR-043 §2 amended, audit §6): a
/// snapshot freezes the *files*, not the *view* — an unflushed memtable
/// write is invisible through it, visible through the engine.
#[test]
fn snapshot_is_s1_files_not_view_memtable_writes_are_invisible() {
    let _serial = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    // Default flush threshold: puts stay in the memtable.
    let mut engine = Engine::open(dir.path()).expect("open");
    engine.put(b"unflushed", b"v").expect("put");

    let snap = engine.snapshot();
    assert_eq!(
        snap.get(b"unflushed").expect("snap get"),
        None,
        "S1: the memtable is not captured"
    );
    assert_eq!(
        engine.get(b"unflushed").expect("get"),
        Some(b"v".to_vec()),
        "the live view still sees it"
    );

    // Once flushed, a *new* snapshot sees it; the old one still doesn't
    // (its version predates the flush).
    engine.flush().expect("flush");
    assert_eq!(snap.get(b"unflushed").expect("old snap get"), None);
    assert_eq!(
        engine.snapshot().get(b"unflushed").expect("new snap get"),
        Some(b"v".to_vec())
    );
}

/// Two snapshots pinning overlapping versions: each SST's file lives
/// exactly as long as its last referencing snapshot — retention is
/// per-file (`Arc<SstHandle>` shared between versions), not per-version.
#[test]
fn overlapping_snapshots_release_each_sst_with_its_last_holder() {
    let _serial = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let mut engine = Engine::open_with_options(dir.path(), small_options()).expect("open");

    engine.put(b"k0", b"v").expect("put -> SST 0");
    engine.put(b"k1", b"v").expect("put -> SST 1");
    let snap1 = engine.snapshot(); // pins {0, 1}
    engine.put(b"k2", b"v").expect("put -> SST 2");
    let snap2 = engine.snapshot(); // pins {0, 1, 2}
    assert_eq!(engine.stats().expect("stats").active_snapshots, 2);

    engine.compact_now().expect("compact -> SST 3");
    for id in 0..3 {
        assert!(sst_path(dir.path(), id).exists(), "SST {id} pinned by a snapshot");
    }

    // snap2 was the only holder of SST 2; SSTs 0 and 1 are still shared
    // with snap1.
    drop(snap2);
    assert!(!sst_path(dir.path(), 2).exists(), "only snap2 pinned SST 2");
    assert!(sst_path(dir.path(), 0).exists(), "snap1 still pins SST 0");
    assert!(sst_path(dir.path(), 1).exists(), "snap1 still pins SST 1");
    assert_eq!(snap1.get(b"k1").expect("snap1 get"), Some(b"v".to_vec()));

    drop(snap1);
    assert!(!sst_path(dir.path(), 0).exists());
    assert!(!sst_path(dir.path(), 1).exists());
    assert_eq!(engine.stats().expect("stats").active_snapshots, 0);
    // The merged SST is untouched by all that releasing.
    for k in [&b"k0"[..], b"k1", b"k2"] {
        assert_eq!(engine.get(k).expect("get"), Some(b"v".to_vec()));
    }
}

/// A deferred removal that fails every retry is counted (the historical
/// `compaction_remove_failures` contract, now fed from the handle drop) and
/// the leftover file is an inert orphan: excluded by the manifest, swept at
/// the next open, never a resurrection (ENG-DUR-002's exact scenario, run
/// through the deferred path).
#[test]
fn failed_deferred_removal_is_counted_then_swept_as_orphan_at_reopen() {
    let _serial = lock();
    let _clear = ClearOnDrop;
    failpoint::clear_all();

    let dir = tempfile::tempdir().expect("tempdir");
    let mut engine = Engine::open_with_options(dir.path(), small_options()).expect("open");
    engine.put(b"key", b"original").expect("put -> SST 0 (value)");
    engine.delete(b"key").expect("delete -> SST 1 (tombstone)");

    let snap = engine.snapshot();
    engine
        .compact_now()
        .expect("compaction publishes the manifest but defers removal: a snapshot is alive");
    let leftovers = [sst_path(dir.path(), 0), sst_path(dir.path(), 1)];
    assert!(leftovers.iter().all(|p| p.exists()));

    failpoint::set("during_compaction_sst_removal", Action::Error);
    drop(snap); // every removal attempt fails all its retries — no panic
    failpoint::clear_all();

    assert!(
        leftovers.iter().all(|p| p.exists()),
        "failed removals leave the files in place"
    );
    assert_eq!(
        engine.stats().expect("stats").compaction_remove_failures,
        2,
        "each of the two retired SSTs failed its removal exactly once"
    );
    assert_eq!(engine.get(b"key").expect("get"), None);
    drop(engine);

    let reopened = Engine::open_with_options(dir.path(), small_options()).expect("reopen");
    assert_eq!(
        reopened.get(b"key").expect("get"),
        None,
        "the leftover value-bearing SST is a manifest orphan — never a resurrection"
    );
    assert!(
        leftovers.iter().all(|p| !p.exists()),
        "the reopen's manifest confrontation sweeps the orphans"
    );
}

/// ADR-043 §3, milestone J4: `compact_prepare`/`compact_commit` split the
/// merge from its commit — `compact_prepare` takes `&self` only, so nothing
/// stops a caller from flushing new data (and publishing its own edit)
/// between the two calls. This is the ENG-COR-001 scenario exercised through
/// real code (not a forged edit like
/// `forged_version_edit_with_unknown_deleted_id_is_refused_and_publishes_nothing`
/// in `engine.rs`'s own test module, which pins the validation directly —
/// this pins the end-to-end protocol that validation exists for): the
/// concurrently-flushed SST must survive the commit untouched, every key
/// (merged and concurrent) must read correctly, and a snapshot taken before
/// `compact_prepare` must stay exactly as it was throughout.
#[test]
fn compact_prepare_then_concurrent_flush_then_commit_keeps_both() {
    let _serial = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let options = EngineOptions {
        memtable_flush_threshold: 1,
        compaction_sst_threshold: 2,
        block_size: 256,
        // This test drives `compact_prepare`/`compact_commit` by hand to
        // stage the exact ENG-COR-001 interleaving below — the `flush()`
        // safety net (`auto_compact_on_flush`, ADR-043 §3/J4) would collapse
        // the three SSTs below back to one on its own third call, before the
        // test ever gets to call `compact_prepare`.
        auto_compact_on_flush: false,
        ..EngineOptions::default()
    };
    let mut engine = Engine::open_with_options(dir.path(), options).expect("open");

    // Three SSTs (0, 1, 2) — past the threshold of 2, so `compact_prepare`
    // has a job to stage.
    engine.put(b"a", b"1").expect("put a -> SST 0");
    engine.put(b"b", b"2").expect("put b -> SST 1");
    engine.put(b"c", b"3").expect("put c -> SST 2");
    assert_eq!(engine.stats().expect("stats").sst_count, 3);
    assert!(engine.compaction_pending());

    // A snapshot pinned *before* prepare — must stay exactly as it was
    // through the whole sequence below (INV-VS-1/INV-VS-6), regardless of
    // however many edits land on `current` around it.
    let snap = engine.snapshot();
    assert_eq!(snap.sst_count(), 3);

    // Prepare: `&self` only, no mutation of `current` — the input set is
    // fixed to {0, 1, 2} right here, at prepare time, not at commit time.
    let job = engine
        .compact_prepare()
        .expect("compact_prepare")
        .expect("threshold exceeded, a job must be staged");

    // "Concurrently" (in production this runs off the write lock the merge
    // above never took): a fresh write, flushed to a brand-new SST 3 —
    // published into `current` *before* the compaction commits. `job`'s
    // `deleted` was fixed to {0, 1, 2} at prepare time, so SST 3 is not
    // among them — exactly the ENG-COR-001 scenario.
    engine
        .put(b"d", b"4")
        .expect("put d -> SST 3, concurrent with the pending job");
    assert_eq!(engine.stats().expect("stats").sst_count, 4);

    // Commit: publishes { added: [merged], deleted: [0, 1, 2] } against
    // `current` *now* (which already includes SST 3), never against the
    // snapshot the merge started from (INV-VS-3).
    engine.compact_commit(job).expect("compact_commit");

    // SST 3 survived (INV-VS-5): the live version is the merged output plus
    // SST 3 — nothing lost, nothing resurrected.
    assert_eq!(
        engine.stats().expect("stats").sst_count,
        2,
        "the merged output plus the concurrently-flushed SST 3"
    );

    // Every key — both merged and concurrently-flushed — reads correctly
    // through the live engine.
    assert_eq!(engine.get(b"a").expect("get a"), Some(b"1".to_vec()));
    assert_eq!(engine.get(b"b").expect("get b"), Some(b"2".to_vec()));
    assert_eq!(engine.get(b"c").expect("get c"), Some(b"3".to_vec()));
    assert_eq!(engine.get(b"d").expect("get d"), Some(b"4".to_vec()));

    // The pre-prepare snapshot is untouched throughout: same 3 SSTs, same
    // values, oblivious both to the compaction that superseded them and to
    // the concurrent write that arrived after it was pinned.
    assert_eq!(snap.sst_count(), 3);
    assert_eq!(snap.get(b"a").expect("snap get a"), Some(b"1".to_vec()));
    assert_eq!(snap.get(b"b").expect("snap get b"), Some(b"2".to_vec()));
    assert_eq!(snap.get(b"c").expect("snap get c"), Some(b"3".to_vec()));
    assert_eq!(
        snap.get(b"d").expect("snap get d"),
        None,
        "SST 3 was flushed after the snapshot was pinned"
    );
    drop(snap);

    // Survives a reopen too — the manifest is the source of truth.
    drop(engine);
    let reopened = Engine::open_with_options(dir.path(), options).expect("reopen");
    for (k, v) in [(&b"a"[..], &b"1"[..]), (b"b", b"2"), (b"c", b"3"), (b"d", b"4")] {
        assert_eq!(reopened.get(k).expect("get"), Some(v.to_vec()));
    }
}

/// Pins a correctness gap found in code review of J4, not by any test above:
/// `compact_prepare` runs under a *shared* lock (`&self`), so two independent
/// callers (in production, two `NativeInner::with_inner` writers each
/// observing `compaction_pending()` off the exclusive write lock, ADR-043
/// §3/J4) can stage a job over the identical input set before either
/// commits. The second `compact_commit` must be refused typed
/// (`EngineError::VersionEditMissingInput`, INV-VS-4) — already exercised by
/// `forged_version_edit_with_unknown_deleted_id_is_refused_and_publishes_nothing`
/// with a hand-forged edit — but the bug this test actually pins is
/// different: the rejected commit must not silently inflate
/// `EngineStats::compaction_count`/`bytes_written`/etc. as though a second
/// compaction had actually happened, breaking `apply_version_edit`'s own
/// documented promise ("on error nothing is published and `self.current` is
/// unchanged") for the counters specifically.
#[test]
fn second_commit_of_two_racing_compact_prepare_jobs_is_refused_and_does_not_inflate_counters() {
    let _serial = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let options = EngineOptions {
        memtable_flush_threshold: 1,
        compaction_sst_threshold: 2,
        block_size: 256,
        auto_compact_on_flush: false,
        ..EngineOptions::default()
    };
    let mut engine = Engine::open_with_options(dir.path(), options).expect("open");

    engine.put(b"a", b"1").expect("put a -> SST 0");
    engine.put(b"b", b"2").expect("put b -> SST 1");
    engine.put(b"c", b"3").expect("put c -> SST 2");
    assert!(engine.compaction_pending());

    // Two jobs staged over the same input set {0, 1, 2} — the race this
    // test pins, not a hypothetical (see doc comment above).
    let job_a = engine.compact_prepare().expect("prepare a").expect("job a staged");
    let job_b = engine.compact_prepare().expect("prepare b").expect("job b staged");

    engine.compact_commit(job_a).expect("first commit wins");
    let stats_after_first = engine.stats().expect("stats");
    assert_eq!(stats_after_first.compaction_count, 1);

    let err = engine
        .compact_commit(job_b)
        .expect_err("second commit races a version already advanced");
    assert!(
        matches!(err, EngineError::VersionEditMissingInput { .. }),
        "must be refused typed (INV-VS-4), not a corrupted publish: {err:?}"
    );

    // The bug this test pins: a rejected commit must not look like a second
    // successful compaction happened.
    let stats_after_second = engine.stats().expect("stats");
    assert_eq!(
        stats_after_second.compaction_count, stats_after_first.compaction_count,
        "a refused compact_commit must not increment compaction_count"
    );
    assert_eq!(
        stats_after_second.bytes_written, stats_after_first.bytes_written,
        "a refused compact_commit must not increment bytes_written"
    );
    assert_eq!(
        stats_after_second.sst_count, stats_after_first.sst_count,
        "a refused compact_commit must not change the live SST count"
    );

    // And the store itself is still exactly as healthy as after the first
    // commit alone — every key still reads correctly.
    assert_eq!(engine.get(b"a").expect("get a"), Some(b"1".to_vec()));
    assert_eq!(engine.get(b"b").expect("get b"), Some(b"2".to_vec()));
    assert_eq!(engine.get(b"c").expect("get c"), Some(b"3".to_vec()));
}
