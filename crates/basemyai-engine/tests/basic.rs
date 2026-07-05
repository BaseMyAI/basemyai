//! Basic put/get/delete/reopen-recovers correctness checks for `Engine`.
//!
//! This is intentionally a handful of sanity tests, not exhaustive coverage:
//! a separate crash-consistency kill-loop harness is built against this same
//! public API (`open`/`put`/`get`/`delete`/`flush`/`close`) next.

use basemyai_engine::{Batch, Engine, EngineOptions};
use tempfile::tempdir;

#[test]
fn put_then_get_returns_value() {
    let dir = tempdir().expect("tempdir");
    let mut engine = Engine::open(dir.path()).expect("open");
    engine.put(b"hello", b"world").expect("put");
    assert_eq!(engine.get(b"hello").expect("get"), Some(b"world".to_vec()));
}

#[test]
fn get_missing_key_returns_none() {
    let dir = tempdir().expect("tempdir");
    let engine = Engine::open(dir.path()).expect("open");
    assert_eq!(engine.get(b"missing").expect("get"), None);
}

#[test]
fn delete_removes_value() {
    let dir = tempdir().expect("tempdir");
    let mut engine = Engine::open(dir.path()).expect("open");
    engine.put(b"key", b"value").expect("put");
    engine.delete(b"key").expect("delete");
    assert_eq!(engine.get(b"key").expect("get"), None);
}

#[test]
fn reopen_without_flush_recovers_from_wal() {
    let dir = tempdir().expect("tempdir");
    {
        let mut engine = Engine::open(dir.path()).expect("open");
        engine.put(b"a", b"1").expect("put");
        engine.put(b"b", b"2").expect("put");
        engine.delete(b"a").expect("delete");
        // Dropped without calling `close`/`flush` — simulates a process
        // ending after durable WAL writes but before any SST flush.
    }
    let engine = Engine::open(dir.path()).expect("reopen");
    assert_eq!(engine.get(b"a").expect("get"), None);
    assert_eq!(engine.get(b"b").expect("get"), Some(b"2".to_vec()));
}

#[test]
fn flush_then_reopen_reads_from_sst() {
    let dir = tempdir().expect("tempdir");
    {
        let mut engine = Engine::open(dir.path()).expect("open");
        engine.put(b"k", b"v").expect("put");
        engine.flush().expect("flush");
    }
    let engine = Engine::open(dir.path()).expect("reopen");
    assert_eq!(engine.get(b"k").expect("get"), Some(b"v".to_vec()));
}

#[test]
fn close_flushes_pending_writes() {
    let dir = tempdir().expect("tempdir");
    {
        let mut engine = Engine::open(dir.path()).expect("open");
        engine.put(b"k", b"v").expect("put");
        engine.close().expect("close");
    }
    let engine = Engine::open(dir.path()).expect("reopen");
    assert_eq!(engine.get(b"k").expect("get"), Some(b"v".to_vec()));
}

#[test]
fn overwrite_then_delete_then_reopen_stays_deleted_after_compaction() {
    let dir = tempdir().expect("tempdir");
    let options = EngineOptions {
        memtable_flush_threshold: 1,
        compaction_sst_threshold: 2,
    };
    {
        let mut engine = Engine::open_with_options(dir.path(), options).expect("open");
        for i in 0..10u32 {
            engine.put(b"counter", &i.to_le_bytes()).expect("put");
        }
        engine.delete(b"counter").expect("delete");
        assert_eq!(engine.get(b"counter").expect("get"), None);
    }
    let engine = Engine::open_with_options(dir.path(), options).expect("reopen");
    assert_eq!(engine.get(b"counter").expect("get"), None);
}

#[test]
fn many_flushes_and_compaction_preserve_latest_value() {
    let dir = tempdir().expect("tempdir");
    let options = EngineOptions {
        memtable_flush_threshold: 1,
        compaction_sst_threshold: 2,
    };
    {
        let mut engine = Engine::open_with_options(dir.path(), options).expect("open");
        for i in 0..10u32 {
            engine.put(b"counter", &i.to_le_bytes()).expect("put");
        }
        assert_eq!(engine.get(b"counter").expect("get"), Some(9u32.to_le_bytes().to_vec()));
    }
    let engine = Engine::open_with_options(dir.path(), options).expect("reopen");
    assert_eq!(engine.get(b"counter").expect("get"), Some(9u32.to_le_bytes().to_vec()));
}

#[test]
fn empty_batch_is_a_no_op() {
    let dir = tempdir().expect("tempdir");
    let mut engine = Engine::open(dir.path()).expect("open");
    engine.put(b"pre-existing", b"v").expect("put");
    let batch = Batch::new();
    assert!(batch.is_empty());
    engine.apply_batch(&batch).expect("apply empty batch");
    assert_eq!(engine.get(b"pre-existing").expect("get"), Some(b"v".to_vec()));
}

#[test]
fn batch_of_n_puts_is_all_or_nothing_visible_after_flush_and_reopen() {
    let dir = tempdir().expect("tempdir");
    {
        let mut engine = Engine::open(dir.path()).expect("open");
        let mut batch = Batch::new();
        for i in 0..50u32 {
            batch.put(&i.to_be_bytes(), &i.to_le_bytes());
        }
        assert_eq!(batch.len(), 50);
        engine.apply_batch(&batch).expect("apply batch");
        engine.flush().expect("flush");
    }
    let engine = Engine::open(dir.path()).expect("reopen");
    for i in 0..50u32 {
        assert_eq!(
            engine.get(&i.to_be_bytes()).expect("get"),
            Some(i.to_le_bytes().to_vec()),
            "key {i} missing or wrong after batch flush + reopen"
        );
    }
}

#[test]
fn batch_of_n_puts_is_all_or_nothing_visible_without_flush_via_wal_replay() {
    let dir = tempdir().expect("tempdir");
    {
        // memtable_flush_threshold high enough that this batch never
        // auto-flushes — exercises WAL replay of the Batch record itself,
        // not just the flushed-SST path.
        let options = EngineOptions {
            memtable_flush_threshold: 10_000,
            compaction_sst_threshold: 4,
        };
        let mut engine = Engine::open_with_options(dir.path(), options).expect("open");
        let mut batch = Batch::new();
        for i in 0..20u32 {
            batch.put(&i.to_be_bytes(), &i.to_le_bytes());
        }
        engine.apply_batch(&batch).expect("apply batch");
        // Dropped without flush/close — only the WAL's Batch record is durable.
    }
    let engine = Engine::open(dir.path()).expect("reopen");
    for i in 0..20u32 {
        assert_eq!(
            engine.get(&i.to_be_bytes()).expect("get"),
            Some(i.to_le_bytes().to_vec()),
            "key {i} missing or wrong after WAL-only batch replay"
        );
    }
}

#[test]
fn batch_mixing_puts_and_deletes() {
    let dir = tempdir().expect("tempdir");
    let mut engine = Engine::open(dir.path()).expect("open");
    engine.put(b"a", b"old-a").expect("put");
    engine.put(b"b", b"old-b").expect("put");

    let mut batch = Batch::new();
    batch.put(b"a", b"new-a"); // overwrite
    batch.delete(b"b"); // delete pre-existing
    batch.put(b"c", b"new-c"); // fresh insert
    engine.apply_batch(&batch).expect("apply batch");

    assert_eq!(engine.get(b"a").expect("get"), Some(b"new-a".to_vec()));
    assert_eq!(engine.get(b"b").expect("get"), None);
    assert_eq!(engine.get(b"c").expect("get"), Some(b"new-c".to_vec()));

    engine.flush().expect("flush");
    let engine = Engine::open(dir.path()).expect("reopen");
    assert_eq!(engine.get(b"a").expect("get"), Some(b"new-a".to_vec()));
    assert_eq!(engine.get(b"b").expect("get"), None);
    assert_eq!(engine.get(b"c").expect("get"), Some(b"new-c".to_vec()));
}

#[test]
fn scan_prefix_returns_live_matching_keys_sorted() {
    let dir = tempdir().expect("tempdir");
    let mut engine = Engine::open(dir.path()).expect("open");
    engine.put(b"idx/a/2", b"a2").expect("put");
    engine.put(b"idx/a/1", b"a1").expect("put");
    engine.put(b"idx/b/1", b"b1").expect("put");
    engine.put(b"idx/a/3", b"a3").expect("put");
    engine.delete(b"idx/a/2").expect("delete");

    let hits = engine.scan_prefix(b"idx/a/").expect("scan");
    let keys: Vec<&[u8]> = hits.iter().map(|(k, _)| k.as_bytes()).collect();
    assert_eq!(keys, vec![&b"idx/a/1"[..], &b"idx/a/3"[..]]);
    assert_eq!(hits[0].1, b"a1".to_vec());
}

#[test]
fn scan_prefix_merges_ssts_and_memtable_with_newest_winning() {
    let dir = tempdir().expect("tempdir");
    let mut engine = Engine::open(dir.path()).expect("open");
    engine.put(b"p/x", b"old-x").expect("put");
    engine.put(b"p/y", b"y").expect("put");
    engine.flush().expect("flush"); // both now live in an SST
    engine.put(b"p/x", b"new-x").expect("put"); // memtable overwrite
    engine.put(b"p/z", b"z").expect("put"); // memtable-only
    engine.delete(b"p/y").expect("delete"); // memtable tombstone over SST value

    let hits = engine.scan_prefix(b"p/").expect("scan");
    assert_eq!(
        hits.iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v.clone()))
            .collect::<Vec<_>>(),
        vec![(b"p/x".to_vec(), b"new-x".to_vec()), (b"p/z".to_vec(), b"z".to_vec()),]
    );
}

#[test]
fn scan_prefix_with_no_matches_is_empty() {
    let dir = tempdir().expect("tempdir");
    let mut engine = Engine::open(dir.path()).expect("open");
    engine.put(b"other", b"v").expect("put");
    assert!(engine.scan_prefix(b"nothing/").expect("scan").is_empty());
}

#[test]
fn later_op_in_same_batch_wins_for_same_key() {
    let dir = tempdir().expect("tempdir");
    let mut engine = Engine::open(dir.path()).expect("open");
    let mut batch = Batch::new();
    batch.put(b"k", b"first");
    batch.put(b"k", b"second");
    batch.delete(b"k");
    batch.put(b"k", b"final");
    engine.apply_batch(&batch).expect("apply batch");
    assert_eq!(engine.get(b"k").expect("get"), Some(b"final".to_vec()));
}
