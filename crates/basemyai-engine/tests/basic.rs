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
        block_size: 256,
        ..EngineOptions::default()
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
        block_size: 256,
        ..EngineOptions::default()
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
            block_size: 16 * 1024,
            ..EngineOptions::default()
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

/// Chains [`Engine::scan_range_page`] pages (ADR-041 §7.3) until
/// `next_start` is `None` and returns the concatenation — the paging
/// protocol every consumer follows. Panics if paging stops making progress.
fn drain_pages(engine: &Engine, start: &[u8], end: &[u8], limit: usize) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut out = Vec::new();
    let mut cursor: Vec<u8> = start.to_vec();
    loop {
        let page = engine.scan_range_page(&cursor, end, limit).expect("scan page");
        out.extend(page.entries.into_iter().map(|(k, v)| (k.as_bytes().to_vec(), v)));
        match page.next_start {
            Some(next) => {
                assert!(next > cursor, "each page must strictly advance the cursor");
                cursor = next;
            }
            None => return out,
        }
    }
}

#[test]
fn scan_range_page_chained_pages_equal_the_full_range_scan() {
    let dir = tempdir().expect("tempdir");
    let mut engine = Engine::open(dir.path()).expect("open");
    // Three layers: two SSTs (via explicit flushes) + a memtable tail, with
    // cross-layer overwrites and tombstones — the merge cases that matter.
    for i in 0..30u32 {
        engine.put(format!("r/{i:04}").as_bytes(), b"sst1").expect("put");
    }
    engine.flush().expect("flush 1");
    for i in 10..20u32 {
        engine.put(format!("r/{i:04}").as_bytes(), b"sst2").expect("put");
    }
    engine.delete(b"r/0005").expect("delete in sst2");
    engine.flush().expect("flush 2");
    engine.put(b"r/0025", b"memtable").expect("put");
    engine.delete(b"r/0012").expect("memtable tombstone");

    let full = engine.scan_range(b"r/", b"r0").expect("full scan");
    let expected: Vec<(Vec<u8>, Vec<u8>)> = full.into_iter().map(|(k, v)| (k.as_bytes().to_vec(), v)).collect();
    for limit in [1usize, 3, 7, 100] {
        assert_eq!(
            drain_pages(&engine, b"r/", b"r0", limit),
            expected,
            "chained pages at limit={limit} must equal the one-shot range scan"
        );
    }
}

#[test]
fn scan_range_page_empty_page_advances_instead_of_terminating() {
    let dir = tempdir().expect("tempdir");
    let mut engine = Engine::open(dir.path()).expect("open");
    // A whole stretch of keys whose newest layer is a tombstone: pages over
    // it are empty yet must keep advancing (`next_start = Some`), never be
    // read as exhaustion.
    for i in 0..20u32 {
        engine.put(format!("t/{i:04}").as_bytes(), b"v").expect("put");
    }
    engine.flush().expect("flush");
    for i in 0..20u32 {
        engine.delete(format!("t/{i:04}").as_bytes()).expect("delete");
    }
    engine.put(b"t/9999", b"survivor").expect("put survivor");

    let mut pages = 0usize;
    let mut cursor: Vec<u8> = b"t/".to_vec();
    let mut live = Vec::new();
    loop {
        let page = engine.scan_range_page(&cursor, b"t0", 4).expect("scan page");
        pages += 1;
        live.extend(page.entries.into_iter().map(|(k, _)| k.as_bytes().to_vec()));
        match page.next_start {
            Some(next) => cursor = next,
            None => break,
        }
    }
    assert_eq!(live, vec![b"t/9999".to_vec()]);
    assert!(
        pages > 1,
        "the tombstoned stretch must have produced intermediate pages"
    );
}

#[test]
fn scan_range_page_truncated_memtable_defers_shadowed_sst_key_to_the_next_page() {
    let dir = tempdir().expect("tempdir");
    let mut engine = Engine::open(dir.path()).expect("open");
    // SST layer: old values for "s/a" and "s/c". Memtable layer: a fresher
    // "s/a" and a tombstone for "s/c". At limit=1 the memtable truncates
    // right after "s/a": the page frontier must stop there — if it slid to
    // "s/c" (the largest merged key), the page would resurrect the SST's
    // stale "s/c" that the not-yet-merged memtable tombstone shadows.
    engine.put(b"s/a", b"old-a").expect("put");
    engine.put(b"s/c", b"old-c").expect("put");
    engine.flush().expect("flush");
    engine.put(b"s/a", b"new-a").expect("overwrite");
    engine.delete(b"s/c").expect("tombstone");

    let first = engine.scan_range_page(b"s/", b"s0", 1).expect("scan page");
    assert_eq!(
        first
            .entries
            .iter()
            .map(|(k, v)| (k.as_bytes().to_vec(), v.clone()))
            .collect::<Vec<_>>(),
        vec![(b"s/a".to_vec(), b"new-a".to_vec())],
        "the first page must carry the fresh value and nothing past the frontier"
    );
    assert!(first.next_start.is_some());

    let total = drain_pages(&engine, b"s/", b"s0", 1);
    assert_eq!(
        total,
        vec![(b"s/a".to_vec(), b"new-a".to_vec())],
        "the tombstoned SST key must never resurface on any page"
    );
}

#[test]
fn scan_range_page_degenerate_inputs_are_empty_and_exhausted() {
    let dir = tempdir().expect("tempdir");
    let mut engine = Engine::open(dir.path()).expect("open");
    engine.put(b"x/1", b"v").expect("put");
    for (start, end, limit) in [(&b"x0"[..], &b"x/"[..], 5usize), (b"x/", b"x0", 0)] {
        let page = engine.scan_range_page(start, end, limit).expect("scan page");
        assert!(page.entries.is_empty());
        assert!(page.next_start.is_none());
    }
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
