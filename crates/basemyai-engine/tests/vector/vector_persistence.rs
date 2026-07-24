//! Persistence harness for the KV-backed vector index (ADR-026, jalon N3,
//! étape 3): round-trip across close/reopen (both the WAL-replay and the
//! flushed-SST paths), rebuild from the raw vectors when the index metadata
//! is corrupt or missing (data = single source of truth, ADR-026 §3), and
//! coexistence with non-index keys in the same store.
//!
//! Quality gate throughout: recall@10 ≥ 0.9 against the exact brute-force
//! oracle (ADR-026 §6) — persistence must never cost recall. Deterministic:
//! seeded data (see `tests/common/mod.rs`), no `rand`.
//!
//! The kill/crash side of persistence (atomicity of insert batches under a
//! real forced kill) is covered by the `vector` mode of
//! `tests/crash_consistency.rs`, not here.

#[path = "../common/mod.rs"]
mod common;

use basemyai_engine::key::vector_index::{META_KEY, meta_key, node_key};
use basemyai_engine::{Engine, EngineError, PersistentVectorIndex, VectorIndexParams};
use common::{LatentData, brute_force_top_k};
use tempfile::tempdir;

const DIM: usize = 384;
const K: usize = 10;

/// Seeded dataset + queries shared by every scenario in this file.
struct Fixture {
    vectors: Vec<Vec<f32>>,
    queries: Vec<Vec<f32>>,
}

impl Fixture {
    fn new(n: usize, num_queries: usize, seed: u64) -> Self {
        let mut data = LatentData::new(seed, DIM);
        let vectors: Vec<Vec<f32>> = (0..n).map(|_| data.point()).collect();
        let queries: Vec<Vec<f32>> = (0..num_queries).map(|_| data.point()).collect();
        Self { vectors, queries }
    }

    fn insert_all(&self, engine: &mut Engine, index: &mut PersistentVectorIndex) {
        for (id, v) in self.vectors.iter().enumerate() {
            index.insert(engine, id as u64, v.clone()).expect("insert must succeed");
        }
    }

    fn search_all(&self, engine: &Engine, index: &mut PersistentVectorIndex) -> Vec<Vec<u64>> {
        self.queries
            .iter()
            .map(|q| index.search(engine, q, K).expect("search must succeed"))
            .collect()
    }

    fn recall(&self, results: &[Vec<u64>]) -> f64 {
        let mut hits = 0usize;
        for (query, got) in self.queries.iter().zip(results) {
            let expected = brute_force_top_k(&self.vectors, query, K);
            hits += got.iter().filter(|id| expected.contains(id)).count();
        }
        hits as f64 / (self.queries.len() * K) as f64
    }
}

/// Round-trip: results after reopen must equal results before, through BOTH
/// recovery paths — drop-without-close (WAL replay) and close (SST load) —
/// with the ADR-026 §6 recall gate holding throughout, and never a rebuild.
#[test]
fn round_trip_survives_wal_replay_and_sst_reopen() {
    let dir = tempdir().expect("tempdir");
    let fixture = Fixture::new(400, 20, 0x9E51_57E4_2026_0704);

    let results_before = {
        let mut engine = Engine::open(dir.path()).expect("open");
        let mut index = PersistentVectorIndex::open(&mut engine, VectorIndexParams::with_dim(DIM)).expect("open index");
        assert!(!index.rebuilt_on_open());
        assert!(index.is_empty());

        fixture.insert_all(&mut engine, &mut index);
        assert_eq!(index.len(), fixture.vectors.len() as u64);

        let results = fixture.search_all(&engine, &mut index);
        let recall = fixture.recall(&results);
        println!("vector_persistence round-trip: recall@{K} before reopen = {recall:.4}");
        assert!(recall >= 0.9, "recall@{K} = {recall:.4} < 0.9 before reopen");
        results
        // Engine dropped WITHOUT close: reopen must recover via WAL replay.
    };

    {
        let mut engine = Engine::open(dir.path()).expect("reopen (wal replay)");
        let mut index =
            PersistentVectorIndex::open(&mut engine, VectorIndexParams::with_dim(DIM)).expect("reopen index");
        assert!(
            !index.rebuilt_on_open(),
            "clean WAL-replay reopen must not need a rebuild"
        );
        assert_eq!(index.len(), fixture.vectors.len() as u64);
        let results = fixture.search_all(&engine, &mut index);
        assert_eq!(
            results, results_before,
            "search results diverged across a WAL-replay reopen"
        );
        engine.close().expect("close");
        // `close` flushed everything to SSTs: next reopen reads the SST path.
    }

    let mut engine = Engine::open(dir.path()).expect("reopen (sst)");
    let mut index = PersistentVectorIndex::open(&mut engine, VectorIndexParams::with_dim(DIM)).expect("reopen index");
    assert!(!index.rebuilt_on_open(), "clean SST reopen must not need a rebuild");
    let results = fixture.search_all(&engine, &mut index);
    assert_eq!(results, results_before, "search results diverged across an SST reopen");
    let recall = fixture.recall(&results);
    assert!(recall >= 0.9, "recall@{K} = {recall:.4} < 0.9 after SST reopen");
}

/// Data is the single source of truth: with the metadata record corrupted
/// (and separately: deleted), `open` must fall back to a rebuild from the
/// vectors stored in the node blocks, bump the epoch, and come back with
/// recall intact. The subsequent reopen must then be clean again.
#[test]
fn rebuild_recovers_from_corrupt_or_missing_meta() {
    for scenario in ["corrupt", "missing"] {
        let dir = tempdir().expect("tempdir");
        let fixture = Fixture::new(300, 20, 0x2EB1_11D0_2026_0705 ^ scenario.len() as u64);

        let mut engine = Engine::open(dir.path()).expect("open");
        let mut index = PersistentVectorIndex::open(&mut engine, VectorIndexParams::with_dim(DIM)).expect("open index");
        fixture.insert_all(&mut engine, &mut index);
        let epoch_before = index.epoch();
        drop(index);

        match scenario {
            "corrupt" => engine.put(META_KEY, b"not a valid meta record").expect("corrupt meta"),
            _ => engine.delete(META_KEY).expect("delete meta"),
        }

        let mut index = PersistentVectorIndex::open(&mut engine, VectorIndexParams::with_dim(DIM))
            .unwrap_or_else(|e| panic!("[{scenario}] open should rebuild, not fail: {e}"));
        assert!(
            index.rebuilt_on_open(),
            "[{scenario}] open must report the rebuild it performed"
        );
        assert_eq!(
            index.len(),
            fixture.vectors.len() as u64,
            "[{scenario}] rebuild lost vectors"
        );
        assert!(
            index.epoch() > epoch_before,
            "[{scenario}] rebuild must bump the epoch ({} !> {epoch_before})",
            index.epoch()
        );

        let results = fixture.search_all(&engine, &mut index);
        let recall = fixture.recall(&results);
        println!("vector_persistence rebuild[{scenario}]: recall@{K} after rebuild = {recall:.4}");
        assert!(
            recall >= 0.9,
            "[{scenario}] recall@{K} = {recall:.4} < 0.9 after rebuild"
        );

        // The rebuild wrote fresh, consistent metadata: reopening now must
        // be clean, with identical results.
        drop(index);
        engine.close().expect("close");
        let mut engine = Engine::open(dir.path()).expect("reopen after rebuild");
        let mut index =
            PersistentVectorIndex::open(&mut engine, VectorIndexParams::with_dim(DIM)).expect("clean reopen");
        assert!(
            !index.rebuilt_on_open(),
            "[{scenario}] reopen after a completed rebuild must be clean"
        );
        assert_eq!(
            fixture.search_all(&engine, &mut index),
            results,
            "[{scenario}] results diverged across the post-rebuild reopen"
        );
    }
}

/// The explicit `rebuild` entry point (the maintenance-driven escape hatch)
/// behaves like the automatic one: epoch bump, nothing lost, recall intact.
#[test]
fn explicit_rebuild_preserves_count_and_recall() {
    let dir = tempdir().expect("tempdir");
    let fixture = Fixture::new(300, 20, 0xF1A7_D15C_2026_0705);

    let mut engine = Engine::open(dir.path()).expect("open");
    let mut index = PersistentVectorIndex::open(&mut engine, VectorIndexParams::with_dim(DIM)).expect("open index");
    fixture.insert_all(&mut engine, &mut index);
    let epoch_before = index.epoch();

    index.rebuild(&mut engine).expect("rebuild");
    assert_eq!(index.epoch(), epoch_before + 1);
    assert_eq!(index.len(), fixture.vectors.len() as u64);
    let recall = fixture.recall(&fixture.search_all(&engine, &mut index));
    assert!(recall >= 0.9, "recall@{K} = {recall:.4} < 0.9 after explicit rebuild");
}

/// Opening with a different dimension than the stored graph must fail loudly
/// (the caller is about to feed wrong-shaped vectors), never rebuild or
/// silently adopt.
#[test]
fn open_with_mismatched_dimension_is_rejected() {
    let dir = tempdir().expect("tempdir");
    let mut engine = Engine::open(dir.path()).expect("open");
    let mut index = PersistentVectorIndex::open(&mut engine, VectorIndexParams::with_dim(16)).expect("open index");
    index.insert(&mut engine, 0, vec![0.5; 16]).expect("insert");
    drop(index);

    let err = PersistentVectorIndex::open(&mut engine, VectorIndexParams::with_dim(32))
        .expect_err("dimension mismatch must be rejected");
    assert!(matches!(
        err,
        EngineError::VectorDimensionMismatch {
            expected: 16,
            found: 32
        }
    ));
}

/// The reserved `idx/vector/` keyspace coexists with arbitrary consumer keys
/// in the same store: index writes never clobber them, and they never
/// confuse the index.
#[test]
fn index_coexists_with_unrelated_keys() {
    let dir = tempdir().expect("tempdir");
    let mut engine = Engine::open(dir.path()).expect("open");
    engine.put(b"user/profile/1", b"alice").expect("put");
    engine.put(b"idw/adjacent-prefix", b"not index data").expect("put");

    let mut index = PersistentVectorIndex::open(&mut engine, VectorIndexParams::with_dim(16)).expect("open index");
    assert!(!index.rebuilt_on_open());
    for id in 0..20u64 {
        let v: Vec<f32> = (0..16).map(|c| ((id * 31 + c) % 7) as f32 - 3.0).collect();
        index.insert(&mut engine, id, v).expect("insert");
    }

    assert_eq!(engine.get(b"user/profile/1").expect("get"), Some(b"alice".to_vec()));
    assert_eq!(
        engine.get(b"idw/adjacent-prefix").expect("get"),
        Some(b"not index data".to_vec())
    );
    // And the index's own records are where the key module says they are.
    assert!(engine.get(meta_key().as_bytes()).expect("get meta").is_some());
    assert!(engine.get(node_key(0).as_bytes()).expect("get node 0").is_some());
}

/// `delete_many_with` (ADR-041 §7.4): several tombstones + ONE refreshed
/// metadata record + the caller's companion batch, all in one atomic group —
/// with absent, duplicate and already-tombstoned ids skipped (never an
/// error), and the `delete_with` asymmetry preserved: a no-op tombstone pass
/// still applies a non-empty `extra`.
#[test]
fn delete_many_with_tombstones_a_group_atomically_and_applies_extra_on_noop() {
    use basemyai_engine::Batch;

    let dir = tempdir().expect("tempdir");
    let mut engine = Engine::open(dir.path()).expect("open");
    let mut index = PersistentVectorIndex::open(&mut engine, VectorIndexParams::with_dim(16)).expect("open index");
    for id in 0..6u64 {
        let v: Vec<f32> = (0..16).map(|c| ((id * 31 + c) % 7) as f32 - 3.0).collect();
        index.insert(&mut engine, id, v).expect("insert");
    }
    assert_eq!(index.len(), 6);

    // Group delete: {1, 3} live, 3 duplicated, 99 absent — plus a companion op.
    let mut extra = Batch::new();
    extra.put(b"user/marker-1", b"rode the same batch");
    let removed = index
        .delete_many_with(&mut engine, &[1, 3, 3, 99], &extra)
        .expect("delete_many_with");
    assert_eq!(removed, 2, "only the two live ids count");
    assert_eq!(index.len(), 4);
    assert_eq!(
        engine.get(b"user/marker-1").expect("get"),
        Some(b"rode the same batch".to_vec()),
        "the companion op must ride the same atomic batch"
    );

    // Tombstoned ids never surface in results again.
    let query: Vec<f32> = (0..16).map(|c| ((31 + c) % 7) as f32 - 3.0).collect();
    let hits = index.search(&engine, &query, 6).expect("search");
    assert!(!hits.contains(&1) && !hits.contains(&3));

    // Re-deleting the same group is a no-op tombstone pass — but a non-empty
    // extra must still be applied (leftover companion deletes of an
    // interrupted earlier attempt must not survive, same as delete_with).
    let mut extra2 = Batch::new();
    extra2.put(b"user/marker-2", b"applied on noop");
    let removed = index
        .delete_many_with(&mut engine, &[1, 3], &extra2)
        .expect("re-delete");
    assert_eq!(removed, 0);
    assert_eq!(index.len(), 4, "count must not double-decrement");
    assert_eq!(
        engine.get(b"user/marker-2").expect("get"),
        Some(b"applied on noop".to_vec())
    );

    // Nothing tombstoned AND empty extra: writes nothing, still Ok(0).
    assert_eq!(
        index
            .delete_many_with(&mut engine, &[99], &Batch::new())
            .expect("absent-only"),
        0
    );

    // Metadata written by the group survives a reopen without a rebuild.
    engine.close().expect("close");
    let mut engine = Engine::open(dir.path()).expect("reopen");
    let index = PersistentVectorIndex::open(&mut engine, VectorIndexParams::with_dim(16)).expect("reopen index");
    assert!(!index.rebuilt_on_open(), "clean reopen after a group delete");
    assert_eq!(index.len(), 4);
}
