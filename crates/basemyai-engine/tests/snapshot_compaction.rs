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
use basemyai_engine::{Engine, EngineOptions};

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
