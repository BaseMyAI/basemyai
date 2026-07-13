// SPDX-License-Identifier: BUSL-1.1
//! N7.1 — `Engine::stats()` : les compteurs et jauges reflètent l'activité
//! réelle du moteur (put/delete/batch/flush/compaction, clair et chiffré).
//! `bytes_read`/`point_lookup_full_sst_read` couvrent les invariants N8.4
//! (ADR-039 §4/§5.5/§8.1) : ouverture O(métadonnées), jamais plus d'un bloc
//! lu par point lookup. `block_cache_hits`/`block_cache_misses` (N8.7,
//! ADR-039 §5.6) sont réels depuis ce jalon : un premier lookup d'un bloc
//! est un miss, un lookup répété du même bloc est un hit.

use basemyai_engine::{Batch, Engine, EngineOptions};

const KEY: &[u8] = b"stats test key";

fn small_options() -> EngineOptions {
    EngineOptions {
        memtable_flush_threshold: 4,
        compaction_sst_threshold: 2,
        block_size: 256,
        ..EngineOptions::default()
    }
}

#[test]
fn fresh_store_stats_are_zeroed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let engine = Engine::open(dir.path()).expect("open");
    let stats = engine.stats().expect("stats");
    assert_eq!(stats.wal_records, 0);
    assert_eq!(stats.wal_bytes, 0);
    assert_eq!(stats.memtable_bytes, 0);
    assert_eq!(stats.sst_count, 0);
    assert_eq!(stats.sst_bytes, 0);
    assert_eq!(stats.tombstone_count, 0);
    assert_eq!(stats.flush_count, 0);
    assert_eq!(stats.compaction_count, 0);
    assert_eq!(stats.bytes_written, 0);
}

#[test]
fn writes_move_wal_and_memtable_gauges() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut engine = Engine::open(dir.path()).expect("open");
    engine.put(b"alpha", b"12345").expect("put");
    engine.delete(b"beta").expect("delete");

    let stats = engine.stats().expect("stats");
    assert_eq!(stats.wal_records, 2);
    assert!(stats.wal_bytes > 0, "two records must occupy WAL bytes");
    // memtable: "alpha"(5) + "12345"(5) + tombstone key "beta"(4) = 14.
    assert_eq!(stats.memtable_bytes, 14);
    assert_eq!(stats.tombstone_count, 1);
    assert_eq!(stats.bytes_written, stats.wal_bytes, "no SST yet: written == WAL bytes");
    assert_eq!(stats.sst_count, 0);
}

#[test]
fn a_batch_is_one_wal_record() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut engine = Engine::open(dir.path()).expect("open");
    let mut batch = Batch::new();
    batch.put(b"a", b"1");
    batch.put(b"b", b"2");
    batch.delete(b"a");
    engine.apply_batch(&batch).expect("apply");
    assert_eq!(engine.stats().expect("stats").wal_records, 1);
}

#[test]
fn flush_and_compaction_counters_track_real_activity() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut engine = Engine::open_with_options(dir.path(), small_options()).expect("open");
    // threshold 4 → 20 puts = 5 auto-flushes; compaction threshold 2 → at
    // least one compaction fires along the way.
    for i in 0..20u32 {
        engine
            .put(format!("key-{i:03}").as_bytes(), format!("value-{i}").as_bytes())
            .expect("put");
    }
    let stats = engine.stats().expect("stats");
    assert_eq!(stats.flush_count, 5);
    assert!(stats.compaction_count >= 1, "compaction threshold 2 must have fired");
    assert!(
        stats.compaction_input_bytes > stats.compaction_output_bytes,
        "merging several SSTs into one must shrink bytes (framing overhead deduped)"
    );
    assert!(stats.sst_count >= 1);
    assert!(stats.sst_bytes > 0);
    assert!(
        stats.bytes_written > stats.sst_bytes,
        "written counts WAL records + every SST generation, not just live files"
    );
    // Every flushed memtable was cleared; the WAL was truncated by the last
    // flush cycle, so only post-flush residue may remain.
    assert_eq!(stats.memtable_bytes, 0);
    assert_eq!(stats.wal_bytes, 0);
}

#[test]
fn compaction_drops_tombstones_from_the_gauge() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut engine = Engine::open_with_options(dir.path(), small_options()).expect("open");
    for i in 0..8u32 {
        engine.put(format!("k{i}").as_bytes(), b"v").expect("put");
    }
    for i in 0..4u32 {
        engine.delete(format!("k{i}").as_bytes()).expect("delete");
    }
    // Force everything down to SSTs, then compact by exceeding the threshold.
    engine.flush().expect("flush");
    while engine.stats().expect("stats").sst_count > 1 {
        engine.put(b"filler", b"x").expect("put");
        engine.flush().expect("flush");
    }
    let stats = engine.stats().expect("stats");
    assert_eq!(
        stats.tombstone_count, 0,
        "full-merge compaction drops tombstones entirely (engine.rs compact doc)"
    );
}

#[test]
fn reopen_resets_counters_and_replays_wal_bytes() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let mut engine = Engine::open_with_options(dir.path(), small_options()).expect("open");
        for i in 0..10u32 {
            engine.put(format!("k{i}").as_bytes(), b"value").expect("put");
        }
        // Leave an unflushed tail in the WAL so reopen replays real bytes.
        engine.put(b"tail", b"unflushed").expect("put");
    }
    let engine = Engine::open_with_options(dir.path(), small_options()).expect("reopen");
    let stats = engine.stats().expect("stats");
    assert_eq!(stats.wal_records, 0, "counters are per-open, never persisted");
    assert_eq!(stats.flush_count, 0);
    assert_eq!(stats.bytes_written, 0);
    assert!(stats.bytes_read > 0);
    assert!(
        stats.bytes_read >= stats.wal_bytes,
        "the replayed WAL bytes are always counted, on top of whatever SST metadata was read"
    );
}

#[test]
fn open_reads_only_sst_metadata_not_whole_bodies() {
    // N8.4's core promise (ADR-039 §4/§8.1): reopening a store with real
    // multi-block SSTs must read O(metadata) bytes, never proportional to
    // the SSTs' full on-disk size — the whole reason the block-based format
    // replaced the whole-file reader.
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let mut engine = Engine::open(dir.path()).expect("open"); // default block_size (16 KiB)
        for i in 0..5000u32 {
            engine
                .put(
                    format!("key-{i:06}").as_bytes(),
                    format!("value-{i}-with-some-padding-to-grow-the-file").as_bytes(),
                )
                .expect("put");
        }
        engine.flush().expect("flush");
    }
    let engine = Engine::open(dir.path()).expect("reopen");
    let stats = engine.stats().expect("stats");
    assert!(
        stats.sst_bytes > 100_000,
        "test needs a real multi-block SST, got {} bytes",
        stats.sst_bytes
    );
    assert!(
        stats.bytes_read < stats.sst_bytes / 4,
        "open read {} bytes out of a {}-byte store — should be O(metadata), not proportional to data",
        stats.bytes_read,
        stats.sst_bytes
    );
}

#[test]
fn point_lookup_full_sst_read_stays_zero_on_a_canonical_workload() {
    // ADR-039 §4/§5.5's instrumented invariant: a point lookup must never
    // read more than one data block per SST it consults. Small block_size +
    // a low compaction threshold forces many blocks across several live
    // SSTs; present and absent keys both exercise the bloom filter.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut engine = Engine::open_with_options(dir.path(), small_options()).expect("open");
    for i in 0..2000u32 {
        engine
            .put(format!("key-{i:06}").as_bytes(), format!("value-{i}").as_bytes())
            .expect("put");
    }
    for i in 0..2000u32 {
        assert_eq!(
            engine.get(format!("key-{i:06}").as_bytes()).expect("get").as_deref(),
            Some(format!("value-{i}").as_bytes())
        );
    }
    for i in 0..500u32 {
        assert_eq!(engine.get(format!("absent-{i:06}").as_bytes()).expect("get"), None);
    }
    let stats = engine.stats().expect("stats");
    assert_eq!(
        stats.point_lookup_full_sst_read, 0,
        "a point lookup read more than one data block within some SST"
    );
}

#[test]
fn encrypted_stats_report_on_disk_envelope_sizes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut engine = Engine::open_encrypted_with_options(dir.path(), KEY, small_options()).expect("open");
    for i in 0..4u32 {
        engine.put(format!("k{i}").as_bytes(), b"value").expect("put");
    }
    let stats = engine.stats().expect("stats");
    assert_eq!(stats.flush_count, 1);
    assert_eq!(stats.sst_count, 1);
    let disk_len = std::fs::read_dir(dir.path())
        .expect("read dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("sst"))
        .map(|p| std::fs::metadata(p).expect("sst metadata").len())
        .sum::<u64>();
    assert_eq!(
        stats.sst_bytes, disk_len,
        "sst_bytes is the sealed on-disk size, not the plaintext body size"
    );
}

#[test]
fn block_cache_hits_and_misses_reflect_real_lookups() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut engine = Engine::open(dir.path()).expect("open");
    engine.put(b"k", b"v").expect("put");
    engine.flush().expect("flush");

    // First lookup after a flush: the block is not resident yet — a miss.
    let _ = engine.get(b"k").expect("get");
    let stats = engine.stats().expect("stats");
    assert_eq!(stats.block_cache_hits, 0);
    assert_eq!(stats.block_cache_misses, 1);

    // Repeated lookup of the same key resolves within the same (now
    // cache-resident) block — a hit, no further miss.
    let _ = engine.get(b"k").expect("get");
    let stats = engine.stats().expect("stats");
    assert_eq!(stats.block_cache_hits, 1);
    assert_eq!(stats.block_cache_misses, 1);
}

#[test]
fn block_cache_fields_stay_zero_before_any_sst_lookup() {
    // No SST exists yet (nothing flushed), so `get` resolves entirely from
    // the memtable — the block cache is never even consulted.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut engine = Engine::open(dir.path()).expect("open");
    engine.put(b"k", b"v").expect("put");
    let _ = engine.get(b"k").expect("get");
    let stats = engine.stats().expect("stats");
    assert_eq!(stats.block_cache_hits, 0);
    assert_eq!(stats.block_cache_misses, 0);
}
