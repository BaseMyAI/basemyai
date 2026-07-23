// SPDX-License-Identifier: BUSL-1.1
//! ENG-DUR-002 minimal correction (P0,
//! `docs/audits/2026-07-engine-architecture-safety-audit.md`
//! §"ENG-DUR-002 — Résurrection possible de clés supprimées après
//! compaction"): `compact()` used to discard the result of removing each
//! superseded SST (`let _ = fs::remove_file(...)`), silently. It now retries
//! a few times and, on persistent failure, counts it via
//! [`basemyai_engine::EngineStats::compaction_remove_failures`] instead of
//! swallowing it. The durable manifest landed since (ENG-DUR-001, N13/J2):
//! `orphan_after_persistent_remove_failure_never_resurrects_a_deleted_key`
//! below is the full closing proof this file's original comment said was
//! still missing — a leftover, undeleted old SST is now an orphan per the
//! manifest, not a resurrection risk.
//!
//! Same failpoint/lock idiom as `tests/failpoints.rs`.

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

fn small_options() -> EngineOptions {
    EngineOptions {
        memtable_flush_threshold: 1,
        compaction_sst_threshold: 100,
        block_size: 256,
        ..EngineOptions::default()
    }
}

/// A persistently failing removal of the (single) old SST is retried, then
/// counted — `compact()` itself still succeeds (the merged SST is already
/// durable and correct regardless of old-file cleanup), and the failure is
/// no longer invisible.
#[test]
fn persistent_remove_failure_is_retried_then_counted_not_ignored() {
    let _serial = lock();
    let _clear = ClearOnDrop;
    failpoint::clear_all();

    let dir = tempfile::tempdir().expect("tempdir");
    let mut engine = Engine::open_with_options(dir.path(), small_options()).expect("open");
    engine
        .put(b"key", b"value")
        .expect("put flushes immediately (threshold 1), producing one SST");
    assert_eq!(
        engine.stats().expect("stats").sst_count,
        1,
        "exactly one input SST is required for this test's failure count to be exact"
    );

    failpoint::set("during_compaction_sst_removal", Action::Error);
    engine
        .compact_now()
        .expect("compact_now must still succeed: the merged SST is durable regardless of old-file cleanup");
    failpoint::clear_all();

    let stats = engine.stats().expect("stats");
    assert_eq!(
        stats.compaction_remove_failures, 1,
        "the one old SST's removal failed on every retry and must be counted exactly once"
    );
    assert_eq!(stats.sst_count, 1, "compaction still produced exactly one live SST");
    assert_eq!(
        engine.get(b"key").expect("get must not error"),
        Some(b"value".to_vec()),
        "the merged SST's data is unaffected by the old file's failed removal"
    );
}

/// The full ENG-DUR-002 scenario the audit demonstrated: a value-bearing SST
/// whose removal fails every retry during a compaction that also removes
/// the tombstone-bearing SST that superseded it. Before the durable
/// manifest (ENG-DUR-001), reopening would let `scan_existing` pick the
/// leftover value-bearing SST back up and resurrect the deleted key. With
/// the manifest, that leftover file is an orphan from the moment
/// `compact()` publishes the merged set — dropped on reopen, never
/// resurrected.
#[test]
fn orphan_after_persistent_remove_failure_never_resurrects_a_deleted_key() {
    let _serial = lock();
    let _clear = ClearOnDrop;
    failpoint::clear_all();

    let dir = tempfile::tempdir().expect("tempdir");
    let mut engine = Engine::open_with_options(dir.path(), small_options()).expect("open");
    engine
        .put(b"key", b"original value")
        .expect("put flushes immediately, producing the value-bearing SST 0");
    engine
        .delete(b"key")
        .expect("delete flushes immediately, producing the tombstone-bearing SST 1");
    assert_eq!(
        engine.stats().expect("stats").sst_count,
        2,
        "two input SSTs are required to reproduce the audit's exact scenario"
    );
    assert_eq!(
        engine.get(b"key").expect("get"),
        None,
        "deleted before compaction ever runs"
    );

    failpoint::set("during_compaction_sst_removal", Action::Error);
    engine
        .compact_now()
        .expect("compact_now must still succeed despite every old-SST removal failing");
    failpoint::clear_all();
    assert!(
        engine.stats().expect("stats").compaction_remove_failures >= 1,
        "at least one old SST (the value-bearing one, per the audit's exact scenario) must have \
         failed every retry"
    );
    // The live in-process view is already correct (`self.ssts` is the
    // merged set only) — the real proof is what a *fresh* reopen sees.
    assert_eq!(engine.get(b"key").expect("get"), None);
    drop(engine);

    let reopened = Engine::open_with_options(dir.path(), small_options()).expect("reopen");
    assert_eq!(
        reopened.get(b"key").expect("get"),
        None,
        "ENG-DUR-002 fully closed: the orphaned, undeleted value-bearing SST must never resurrect \
         a deleted key once the manifest (not the directory listing) decides liveness"
    );
}
