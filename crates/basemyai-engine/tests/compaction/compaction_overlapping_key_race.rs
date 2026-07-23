// SPDX-License-Identifier: BUSL-1.1
//! Regression coverage for DUR-LSM-01 (BaseMyAI adversarial audit,
//! 2026-07-22): off-lock compaction (ADR-043 §3/J4) could silently
//! resurrect a stale value or undo a tombstone for any key touched by a
//! concurrent flush racing the compaction, because `apply_version_edit`
//! assembled `Version.ssts` by `filter`-then-`extend` instead of a
//! canonical order — `extend` always appended the compaction's merged SST
//! *after* whatever survived the filter, even when that merged SST's id
//! (reserved at `compact_prepare` time) was *lower* than a concurrently
//! flushed SST's id, putting stale data ahead of fresher data in
//! `Engine::get`'s `.iter().rev()` and `scan_prefix`'s forward overlay.
//!
//! `tests/snapshot_compaction.rs`'s
//! `compact_prepare_then_concurrent_flush_then_commit_keeps_both` already
//! exercises this exact prepare/concurrent-write/commit interleaving, but
//! deliberately with a *disjoint* key (`d`, distinct from the compacted
//! `a`/`b`/`c`) — by its own doc comment, that test pins that the
//! concurrently-flushed SST is not lost, never that its data is ordered
//! *ahead of* stale merged data for a key both versions disagree about.
//! This file closes that gap: every test below deliberately reuses a key
//! already present in the compaction's input set for the concurrent write.

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard, OnceLock};

use basemyai_engine::{Batch, Engine, EngineOptions};

fn lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Manual `compact_prepare`/`compact_commit` driving, one SST per write,
/// threshold low enough that three writes already stage a job — the same
/// idiom `compact_prepare_then_concurrent_flush_then_commit_keeps_both`
/// uses, so the interleaving below is directly comparable to it.
fn racing_options() -> EngineOptions {
    EngineOptions {
        memtable_flush_threshold: 1,
        compaction_sst_threshold: 2,
        block_size: 256,
        auto_compact_on_flush: false,
        ..EngineOptions::default()
    }
}

/// The core DUR-LSM-01 regression: a compaction merging a stale value for
/// `a`, racing a concurrent flush that updates `a` to a newer value — the
/// live engine must read the newer value, not the merged one.
#[test]
fn compaction_then_concurrent_put_on_compacted_key_keeps_the_newer_value() {
    let _serial = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let options = racing_options();
    let mut engine = Engine::open_with_options(dir.path(), options).expect("open");

    engine.put(b"a", b"1").expect("put a -> SST 0");
    engine.put(b"b", b"2").expect("put b -> SST 1");
    engine.put(b"c", b"3").expect("put c -> SST 2");
    assert!(engine.compaction_pending());

    // Prepare: fixes the merge's input to {0, 1, 2} — including the stale
    // `a -> "1"` — and reserves the merged SST's id (3) *before* the
    // concurrent write below reserves its own.
    let job = engine.compact_prepare().expect("compact_prepare").expect("job staged");

    // Concurrent write to a key the merge already committed to reading —
    // this is what `compact_prepare_then_concurrent_flush_then_commit_keeps_both`
    // deliberately does *not* do (it writes a disjoint key `d`).
    engine
        .put(b"a", b"UPDATED")
        .expect("put a -> SST 3, concurrent with the pending job");

    engine.compact_commit(job).expect("compact_commit");

    assert_eq!(
        engine.get(b"a").expect("get a"),
        Some(b"UPDATED".to_vec()),
        "DUR-LSM-01: the concurrently-flushed update must win over the stale merged value"
    );
    assert_eq!(engine.get(b"b").expect("get b"), Some(b"2".to_vec()));
    assert_eq!(engine.get(b"c").expect("get c"), Some(b"3".to_vec()));

    // Survives a reopen too.
    drop(engine);
    let reopened = Engine::open_with_options(dir.path(), options).expect("reopen");
    assert_eq!(reopened.get(b"a").expect("get a"), Some(b"UPDATED".to_vec()));
}

/// Mirror of the above with a delete instead of a put: a tombstone written
/// concurrently with a compaction that merged the pre-delete value must not
/// be undone by the merge landing "after" it in visibility order.
#[test]
fn compaction_then_concurrent_delete_on_compacted_key_stays_deleted() {
    let _serial = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let options = racing_options();
    let mut engine = Engine::open_with_options(dir.path(), options).expect("open");

    engine.put(b"a", b"1").expect("put a -> SST 0");
    engine.put(b"b", b"2").expect("put b -> SST 1");
    engine.put(b"c", b"3").expect("put c -> SST 2");
    assert!(engine.compaction_pending());

    let job = engine.compact_prepare().expect("compact_prepare").expect("job staged");
    engine
        .delete(b"a")
        .expect("delete a -> SST 3, concurrent with the pending job");
    engine.compact_commit(job).expect("compact_commit");

    assert_eq!(
        engine.get(b"a").expect("get a"),
        None,
        "DUR-LSM-01: a concurrent delete must not be undone by a compaction that merged the older value"
    );
    assert_eq!(engine.get(b"b").expect("get b"), Some(b"2".to_vec()));
    assert_eq!(engine.get(b"c").expect("get c"), Some(b"3".to_vec()));

    drop(engine);
    let reopened = Engine::open_with_options(dir.path(), options).expect("reopen");
    assert_eq!(reopened.get(b"a").expect("get a"), None);
}

/// Delete-then-reinsert during the race window: the final state must be the
/// reinsert, not the merge's stale pre-delete value.
#[test]
fn compaction_then_concurrent_delete_and_reinsert_keeps_the_reinsert() {
    let _serial = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    // `auto_compact_on_flush: false` (from `racing_options`) is what keeps
    // the two concurrent flushes below from retriggering a second
    // compaction on their own — no need to also raise the threshold, which
    // would (and did, before this fix) also stop `compact_prepare` from
    // finding the initial job.
    let options = racing_options();
    let mut engine = Engine::open_with_options(dir.path(), options).expect("open");

    engine.put(b"a", b"1").expect("put a -> SST 0");
    engine.put(b"b", b"2").expect("put b -> SST 1");
    engine.put(b"c", b"3").expect("put c -> SST 2");

    let job = engine.compact_prepare().expect("compact_prepare").expect("job staged");
    engine.delete(b"a").expect("delete a -> SST 3");
    engine.put(b"a", b"REINSERTED").expect("put a -> SST 4");
    engine.compact_commit(job).expect("compact_commit");

    assert_eq!(
        engine.get(b"a").expect("get a"),
        Some(b"REINSERTED".to_vec()),
        "the reinsert (newest write) must win over both the tombstone and the stale merge"
    );
}

/// The batch counterpart: a single atomic `apply_batch` touching one
/// compacted key (put) and one non-compacted key (delete), concurrent with
/// the pending job.
#[test]
fn compaction_then_concurrent_batch_put_and_delete_on_compacted_keys() {
    let _serial = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let options = racing_options();
    let mut engine = Engine::open_with_options(dir.path(), options).expect("open");

    engine.put(b"a", b"1").expect("put a -> SST 0");
    engine.put(b"b", b"2").expect("put b -> SST 1");
    engine.put(b"c", b"3").expect("put c -> SST 2");

    let job = engine.compact_prepare().expect("compact_prepare").expect("job staged");

    let mut batch = Batch::new();
    batch.put(b"a", b"BATCHED"); // overwrite a compacted key
    batch.delete(b"b"); // tombstone another compacted key
    engine
        .apply_batch(&batch)
        .expect("apply_batch -> SST 3, concurrent with the pending job");

    engine.compact_commit(job).expect("compact_commit");

    assert_eq!(engine.get(b"a").expect("get a"), Some(b"BATCHED".to_vec()));
    assert_eq!(engine.get(b"b").expect("get b"), None);
    assert_eq!(engine.get(b"c").expect("get c"), Some(b"3".to_vec()));
}

/// `scan_prefix`'s forward `BTreeMap`-overlay merge has the mirror bug to
/// `get`'s `.iter().rev()`: it must also prefer the concurrently-flushed
/// value over the stale merged one, not silently let the merged SST's
/// (later-processed, pre-fix) entry overwrite the newer one.
#[test]
fn scan_prefix_reflects_the_newer_concurrent_write_not_the_stale_merged_value() {
    let _serial = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let options = racing_options();
    let mut engine = Engine::open_with_options(dir.path(), options).expect("open");

    engine.put(b"k/a", b"1").expect("put -> SST 0");
    engine.put(b"k/b", b"2").expect("put -> SST 1");
    engine.put(b"k/c", b"3").expect("put -> SST 2");

    let job = engine.compact_prepare().expect("compact_prepare").expect("job staged");
    engine.put(b"k/a", b"UPDATED").expect("put -> SST 3, concurrent");
    engine.compact_commit(job).expect("compact_commit");

    let scanned: BTreeMap<Vec<u8>, Vec<u8>> = engine
        .scan_prefix(b"k/")
        .expect("scan_prefix")
        .into_iter()
        .map(|(k, v)| (k.as_bytes().to_vec(), v))
        .collect();
    assert_eq!(scanned.get(b"k/a".as_slice()), Some(&b"UPDATED".to_vec()));
    assert_eq!(scanned.get(b"k/b".as_slice()), Some(&b"2".to_vec()));
    assert_eq!(scanned.get(b"k/c".as_slice()), Some(&b"3".to_vec()));
}

/// Two compaction jobs prepared over *overlapping but not identical* input
/// sets (distinct from `second_commit_of_two_racing_compact_prepare_jobs_
/// is_refused_and_does_not_inflate_counters` in `snapshot_compaction.rs`,
/// which uses two jobs over the *same* set): the first commit wins, the
/// second is refused typed rather than resurrecting stale data for the
/// overlapping keys or silently dropping the non-overlapping one.
#[test]
fn two_partially_overlapping_compaction_jobs_the_second_is_refused_and_data_stays_consistent() {
    let _serial = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let options = EngineOptions {
        compaction_sst_threshold: 1,
        ..racing_options()
    };
    let mut engine = Engine::open_with_options(dir.path(), options).expect("open");

    engine.put(b"a", b"1").expect("put a -> SST 0");
    engine.put(b"b", b"2").expect("put b -> SST 1");
    // First job: staged over {SST0, SST1}.
    let job_a = engine.compact_prepare().expect("prepare a").expect("job a staged");

    engine.put(b"c", b"3").expect("put c -> SST 2");
    // Second job: staged over {SST0, SST1, SST2} — overlaps job_a on
    // {SST0, SST1} but also covers SST2, which job_a does not.
    let job_b = engine.compact_prepare().expect("prepare b").expect("job b staged");

    engine.compact_commit(job_a).expect("first commit wins");
    let err = engine
        .compact_commit(job_b)
        .expect_err("job_b's input set (SST0/SST1) was already retired by job_a's commit");
    assert!(
        matches!(err, basemyai_engine::EngineError::VersionEditMissingInput { .. }),
        "must be refused typed (INV-VS-4): {err:?}"
    );

    // Nothing lost or resurrected: `c` (only ever in job_b's set) is still
    // live via the ordinary flush path, `a`/`b` reflect job_a's merge.
    assert_eq!(engine.get(b"a").expect("get a"), Some(b"1".to_vec()));
    assert_eq!(engine.get(b"b").expect("get b"), Some(b"2".to_vec()));
    assert_eq!(engine.get(b"c").expect("get c"), Some(b"3".to_vec()));
}

/// A snapshot pinned before the race must stay exactly as it was (INV-VS-1/
/// INV-VS-6) regardless of the id-ordering fix — S1 semantics apply
/// unchanged to the overlapping-key case.
#[test]
fn snapshot_taken_before_the_race_is_unaffected_by_the_ordering_fix() {
    let _serial = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let options = racing_options();
    let mut engine = Engine::open_with_options(dir.path(), options).expect("open");

    engine.put(b"a", b"1").expect("put a -> SST 0");
    engine.put(b"b", b"2").expect("put b -> SST 1");
    engine.put(b"c", b"3").expect("put c -> SST 2");
    let snap = engine.snapshot();

    let job = engine.compact_prepare().expect("compact_prepare").expect("job staged");
    engine.put(b"a", b"UPDATED").expect("put a -> SST 3, concurrent");
    engine.compact_commit(job).expect("compact_commit");

    assert_eq!(
        snap.get(b"a").expect("snap get a"),
        Some(b"1".to_vec()),
        "the pre-race snapshot must still read its pinned state, oblivious to the race"
    );
    assert_eq!(engine.get(b"a").expect("get a"), Some(b"UPDATED".to_vec()));
    drop(snap);
}

/// A minimal differential oracle: replays a fixed sequence of put/delete/
/// batch/flush/compact_prepare/concurrent-write/compact_commit/reopen
/// against both a real `Engine` and a plain `BTreeMap` reference model,
/// asserting the engine's visible state matches the oracle after every
/// step — not just at the end, so a divergence introduced by any single
/// step (not necessarily the compaction ones) is caught at its source.
#[test]
fn oracle_sequence_put_delete_batch_compaction_race_reopen() {
    let _serial = lock();
    let dir = tempfile::tempdir().expect("tempdir");
    let options = EngineOptions {
        memtable_flush_threshold: 1,
        compaction_sst_threshold: 3,
        block_size: 256,
        auto_compact_on_flush: false,
        ..EngineOptions::default()
    };
    let mut engine = Engine::open_with_options(dir.path(), options).expect("open");
    let mut oracle: BTreeMap<&'static [u8], &'static [u8]> = BTreeMap::new();

    let assert_matches = |engine: &Engine, oracle: &BTreeMap<&'static [u8], &'static [u8]>, step: &str| {
        for key in [&b"a"[..], b"b", b"c", b"d", b"e"] {
            let expected = oracle.get(key).map(|v| v.to_vec());
            let actual = engine.get(key).expect("get");
            assert_eq!(actual, expected, "step {step}: mismatch on key {key:?}");
        }
        let expected_scan: Vec<(Vec<u8>, Vec<u8>)> = oracle.iter().map(|(k, v)| (k.to_vec(), v.to_vec())).collect();
        let actual_scan: Vec<(Vec<u8>, Vec<u8>)> = engine
            .scan_prefix(b"")
            .expect("scan_prefix")
            .into_iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v))
            .collect();
        assert_eq!(actual_scan, expected_scan, "step {step}: scan_prefix mismatch");
    };

    // 1: three ordinary writes, past the compaction threshold.
    engine.put(b"a", b"1").expect("put");
    oracle.insert(b"a", b"1");
    engine.put(b"b", b"2").expect("put");
    oracle.insert(b"b", b"2");
    engine.put(b"c", b"3").expect("put");
    oracle.insert(b"c", b"3");
    assert_matches(&engine, &oracle, "after 3 puts");

    // 2: delete one of them.
    engine.delete(b"b").expect("delete");
    oracle.remove(&b"b"[..]);
    assert_matches(&engine, &oracle, "after delete b");

    // 3: a batch mixing put and delete.
    let mut batch = Batch::new();
    batch.put(b"d", b"4");
    batch.delete(b"a");
    engine.apply_batch(&batch).expect("apply_batch");
    oracle.insert(b"d", b"4");
    oracle.remove(&b"a"[..]);
    assert_matches(&engine, &oracle, "after batch(put d, delete a)");

    // 4: reinsert `a`, take a snapshot, then run the exact overlapping-key
    // race (DUR-LSM-01) — compact_prepare, concurrent write to a key in the
    // input set, compact_commit — and confirm the oracle still matches.
    engine.put(b"a", b"5").expect("put");
    oracle.insert(b"a", b"5");
    assert_matches(&engine, &oracle, "after reinsert a");

    let snap = engine.snapshot();
    let snap_oracle = oracle.clone();

    if engine.compaction_pending() {
        let job = engine.compact_prepare().expect("compact_prepare").expect("job staged");
        engine.put(b"c", b"UPDATED_DURING_COMPACTION").expect("put");
        oracle.insert(b"c", b"UPDATED_DURING_COMPACTION");
        engine.put(b"e", b"6").expect("put");
        oracle.insert(b"e", b"6");
        engine.compact_commit(job).expect("compact_commit");
        assert_matches(&engine, &oracle, "after compaction racing a concurrent write");
    }

    // The snapshot taken before the race must still match the oracle state
    // as of that point, not the post-race state.
    for key in [&b"a"[..], b"b", b"c", b"d", b"e"] {
        let expected = snap_oracle.get(key).map(|v| v.to_vec());
        let actual = snap.get(key).expect("snap get");
        assert_eq!(actual, expected, "snapshot mismatch on key {key:?}");
    }
    drop(snap);

    // 5: reopen and confirm the oracle still matches — recovery must not
    // diverge from the live state the fix already produced.
    drop(engine);
    let reopened = Engine::open_with_options(dir.path(), options).expect("reopen");
    assert_matches(&reopened, &oracle, "after reopen");
}
