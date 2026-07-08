//! Churn harness for the native vector index (ADR-026 §6, jalon N3, étape
//! deletes): **recall@10 ≥ 0.9 AFTER insert/delete churn** — the exit
//! criterion the ADR itself flags as "le plus susceptible d'échouer en
//! premier", and the very scenario where HNSW degrades (the #1 argument for
//! choosing the DiskANN family).
//!
//! Scenario (both index flavors): N vectors inserted, then several cycles
//! of "delete X% of the live set at random, insert as many fresh vectors",
//! with recall@10 measured against the exact brute-force oracle **over the
//! live vectors only**:
//! - (a) after the churn cycles, tombstones still in place (lazy-repair
//!   regime: tombstones route, results filter them);
//! - (b) after an explicit `consolidate()` (FreshDiskANN repair + physical
//!   purge).
//!
//! Both measurements must clear the 0.9 gate. Deterministic throughout
//! (seeded xorshift64*, see `tests/common/mod.rs`); real numbers are
//! printed, never assumed.
//!
//! The kill/crash side of deletes (confirmed-deleted ids never resurfacing
//! after a real forced kill, consolidation interrupted mid-flight) is
//! covered by the `vector` mode of `tests/crash_consistency.rs`, not here.

mod common;

use basemyai_engine::key::vector_index::node_key;
use basemyai_engine::{Engine, PersistentVectorIndex, VectorIndex, VectorIndexParams};
use common::{LatentData, XorShift64, brute_force_top_k};
use tempfile::tempdir;

const DIM: usize = 384;
const K: usize = 10;
const NUM_QUERIES: usize = 50;

/// The two index flavors behind one churn driver, so the scenario cannot
/// drift between them (mirrors the shared-planner discipline of `graph.rs`).
trait ChurnIndex {
    fn insert(&mut self, id: u64, vector: Vec<f32>);
    /// Returns whether the id was live (idempotence is asserted by the
    /// driver).
    fn delete(&mut self, id: u64) -> bool;
    /// Returns the number of tombstones physically purged.
    fn consolidate(&mut self) -> u64;
    fn search(&mut self, query: &[f32], k: usize) -> Vec<u64>;
    fn len(&self) -> u64;
}

impl ChurnIndex for VectorIndex {
    fn insert(&mut self, id: u64, vector: Vec<f32>) {
        VectorIndex::insert(self, id, vector).expect("insert must succeed");
    }
    fn delete(&mut self, id: u64) -> bool {
        VectorIndex::delete(self, id)
    }
    fn consolidate(&mut self) -> u64 {
        VectorIndex::consolidate(self).expect("consolidate must succeed") as u64
    }
    fn search(&mut self, query: &[f32], k: usize) -> Vec<u64> {
        VectorIndex::search(self, query, k).expect("search must succeed")
    }
    fn len(&self) -> u64 {
        VectorIndex::len(self) as u64
    }
}

struct PersistentChurn {
    engine: Engine,
    index: PersistentVectorIndex,
}

impl ChurnIndex for PersistentChurn {
    fn insert(&mut self, id: u64, vector: Vec<f32>) {
        self.index
            .insert(&mut self.engine, id, vector)
            .expect("insert must succeed");
    }
    fn delete(&mut self, id: u64) -> bool {
        self.index.delete(&mut self.engine, id).expect("delete must succeed")
    }
    fn consolidate(&mut self) -> u64 {
        self.index
            .consolidate(&mut self.engine)
            .expect("consolidate must succeed")
    }
    fn search(&mut self, query: &[f32], k: usize) -> Vec<u64> {
        self.index.search(&self.engine, query, k).expect("search must succeed")
    }
    fn len(&self) -> u64 {
        self.index.len()
    }
}

/// The live set as `(id, vector)` pairs plus the exact oracle over it.
fn oracle_top_k(live: &[(u64, Vec<f32>)], query: &[f32], k: usize) -> Vec<u64> {
    let vectors: Vec<Vec<f32>> = live.iter().map(|(_, v)| v.clone()).collect();
    brute_force_top_k(&vectors, query, k)
        .into_iter()
        .map(|pos| live[pos as usize].0)
        .collect()
}

/// recall@K of `index` against the live-only oracle, also asserting that no
/// deleted id ever surfaces in the results.
fn measure_recall<I: ChurnIndex>(
    index: &mut I,
    live: &[(u64, Vec<f32>)],
    deleted: &[u64],
    queries: &[Vec<f32>],
) -> f64 {
    let mut hits = 0usize;
    for query in queries {
        let expected = oracle_top_k(live, query, K);
        let got = index.search(query, K);
        for id in &got {
            assert!(
                !deleted.contains(id),
                "deleted id {id} surfaced in search results: {got:?}"
            );
        }
        hits += got.iter().filter(|id| expected.contains(id)).count();
    }
    hits as f64 / (queries.len() * K) as f64
}

/// Runs the churn scenario and returns `(recall_after_churn,
/// recall_after_consolidate)`. `label` tags the printed report.
fn run_churn<I: ChurnIndex>(
    index: &mut I,
    label: &str,
    n: usize,
    cycles: usize,
    churn_fraction: f64,
    seed: u64,
) -> (f64, f64) {
    let mut data = LatentData::new(seed, DIM);
    let mut rng = XorShift64::new(seed ^ 0x00C4_A11F_0000_0001);

    // Initial build.
    let mut live: Vec<(u64, Vec<f32>)> = Vec::with_capacity(n);
    let mut next_id: u64 = 0;
    for _ in 0..n {
        let v = data.point();
        index.insert(next_id, v.clone());
        live.push((next_id, v));
        next_id += 1;
    }
    let queries: Vec<Vec<f32>> = (0..NUM_QUERIES).map(|_| data.point()).collect();

    // Churn cycles: delete churn_fraction of the live set at random,
    // re-insert as many fresh vectors under fresh ids.
    let mut deleted: Vec<u64> = Vec::new();
    let per_cycle = (n as f64 * churn_fraction) as usize;
    for _ in 0..cycles {
        for _ in 0..per_cycle {
            let victim = rng.next_u64() as usize % live.len();
            let (id, _) = live.swap_remove(victim);
            assert!(index.delete(id), "delete of live id {id} must report true");
            assert!(!index.delete(id), "second delete of {id} must be a no-op");
            deleted.push(id);
        }
        for _ in 0..per_cycle {
            let v = data.point();
            index.insert(next_id, v.clone());
            live.push((next_id, v));
            next_id += 1;
        }
    }
    assert_eq!(index.len(), live.len() as u64, "live count drifted during churn");

    // (a) Recall with tombstones still in place (no consolidation yet).
    let recall_after_churn = measure_recall(index, &live, &deleted, &queries);

    // (b) Recall after the explicit FreshDiskANN consolidation.
    let purged = index.consolidate();
    assert_eq!(
        purged,
        deleted.len() as u64,
        "consolidate must purge exactly the tombstoned ids"
    );
    assert_eq!(index.len(), live.len() as u64);
    let recall_after_consolidate = measure_recall(index, &live, &deleted, &queries);

    println!(
        "vector_churn[{label}]: n={n} dim={DIM} cycles={cycles} churn={:.0}% \
         (deleted total {} / reinserted {}) queries={NUM_QUERIES} k={K} -> \
         recall@{K} after churn = {recall_after_churn:.4}, after consolidate = \
         {recall_after_consolidate:.4} (purged {purged} tombstones)",
        churn_fraction * 100.0,
        deleted.len(),
        deleted.len(),
    );
    (recall_after_churn, recall_after_consolidate)
}

fn assert_gate(label: &str, recall_after_churn: f64, recall_after_consolidate: f64) {
    assert!(
        recall_after_churn >= 0.9,
        "[{label}] recall@{K} = {recall_after_churn:.4} < 0.9 AFTER CHURN (ADR-026 §6 exit criterion)"
    );
    assert!(
        recall_after_consolidate >= 0.9,
        "[{label}] recall@{K} = {recall_after_consolidate:.4} < 0.9 AFTER CONSOLIDATE (ADR-026 §6 exit criterion)"
    );
}

/// ADR-026 §6 churn gate at N=2 000 on the in-RAM reference index: 3 cycles
/// of 20 % random deletes + as many fresh inserts (60 % of the original
/// index churned, 600 tombstones standing when (a) is measured).
#[test]
fn ram_churn_recall_n2000() {
    let mut index = VectorIndex::new(VectorIndexParams::with_dim(DIM));
    let (a, b) = run_churn(&mut index, "ram", 2_000, 3, 0.20, 0xC4A1_1F00_2026_0705);
    assert_gate("ram", a, b);
}

/// Same churn gate on the KV-persisted flavor (smaller N — the persistent
/// path pays real WAL/SST I/O per insert), plus what only it can prove:
/// after consolidate the tombstoned blocks are physically gone from the
/// store, and a close/reopen is clean (no rebuild) with recall intact.
#[test]
fn persistent_churn_recall_with_reopen() {
    let dir = tempdir().expect("tempdir");
    let n = 600;
    let seed = 0xC4A1_1F00_2026_0706;

    let (deleted_ids, live, queries) = {
        let mut engine = Engine::open(dir.path()).expect("open");
        let index = PersistentVectorIndex::open(&mut engine, VectorIndexParams::with_dim(DIM)).expect("open index");
        let mut churn = PersistentChurn { engine, index };
        let (a, b) = run_churn(&mut churn, "persistent", n, 2, 0.20, seed);
        assert_gate("persistent", a, b);

        // Physical purge is real: no tombstoned block left in the store.
        // Recompute the deleted set exactly as run_churn did (same seeds).
        let mut data = LatentData::new(seed, DIM);
        let mut rng = XorShift64::new(seed ^ 0x00C4_A11F_0000_0001);
        let mut live: Vec<(u64, Vec<f32>)> = Vec::new();
        let mut next_id: u64 = 0;
        for _ in 0..n {
            let v = data.point();
            live.push((next_id, v));
            next_id += 1;
        }
        let queries: Vec<Vec<f32>> = (0..NUM_QUERIES).map(|_| data.point()).collect();
        let mut deleted: Vec<u64> = Vec::new();
        let per_cycle = (n as f64 * 0.20) as usize;
        for _ in 0..2 {
            for _ in 0..per_cycle {
                let victim = rng.next_u64() as usize % live.len();
                let (id, _) = live.swap_remove(victim);
                deleted.push(id);
            }
            for _ in 0..per_cycle {
                let v = data.point();
                live.push((next_id, v));
                next_id += 1;
            }
        }
        for id in &deleted {
            assert!(
                churn.engine.get(node_key(*id).as_bytes()).expect("get").is_none(),
                "tombstoned block {id} still present after consolidate"
            );
        }
        churn.engine.close().expect("close");
        (deleted, live, queries)
    };

    // Reopen: clean (consolidation keeps the metadata consistent at every
    // step), deleted ids still never surface, recall gate still holds.
    let mut engine = Engine::open(dir.path()).expect("reopen");
    let index = PersistentVectorIndex::open(&mut engine, VectorIndexParams::with_dim(DIM)).expect("reopen index");
    assert!(
        !index.rebuilt_on_open(),
        "reopen after churn + consolidate must be clean"
    );
    assert_eq!(index.len(), live.len() as u64);
    let mut churn = PersistentChurn { engine, index };
    let recall = measure_recall(&mut churn, &live, &deleted_ids, &queries);
    println!("vector_churn[persistent]: recall@{K} after reopen = {recall:.4}");
    assert!(recall >= 0.9, "recall@{K} = {recall:.4} < 0.9 after reopen");
}

/// Resurrection semantics on the persistent flavor: update = delete +
/// reinsert works, survives a reopen, and duplicate-live stays an error.
#[test]
fn persistent_resurrection_survives_reopen() {
    let dir = tempdir().expect("tempdir");
    let mut data = LatentData::new(0x2E5B_112E_2026_0705, DIM);
    let old = data.point();
    let new = data.point();
    let other = data.point();

    {
        let mut engine = Engine::open(dir.path()).expect("open");
        let mut index = PersistentVectorIndex::open(&mut engine, VectorIndexParams::with_dim(DIM)).expect("open index");
        index.insert(&mut engine, 1, old.clone()).expect("insert");
        index.insert(&mut engine, 2, other).expect("insert");
        assert!(index.delete(&mut engine, 1).expect("delete"));
        assert!(!index.delete(&mut engine, 1).expect("idempotent delete"));
        // Tombstoned id is excluded from results...
        let results = index.search(&engine, &old, 2).expect("search");
        assert!(!results.contains(&1), "tombstoned id surfaced: {results:?}");
        // ...and resurrectable with a new vector.
        index.insert(&mut engine, 1, new.clone()).expect("resurrection");
        assert_eq!(index.len(), 2);
        let err = index
            .insert(&mut engine, 1, old)
            .expect_err("live duplicate must be rejected");
        assert!(matches!(err, basemyai_engine::EngineError::DuplicateVectorId { id: 1 }));
        engine.close().expect("close");
    }

    let mut engine = Engine::open(dir.path()).expect("reopen");
    let index = PersistentVectorIndex::open(&mut engine, VectorIndexParams::with_dim(DIM)).expect("reopen index");
    assert!(!index.rebuilt_on_open());
    assert_eq!(index.len(), 2);
    let results = index.search(&engine, &new, 1).expect("search");
    assert_eq!(results, vec![1], "resurrected id must be found under its NEW vector");
}

/// Same RAM churn gate at N=10 000 — heavier, for manual runs:
/// `cargo test --release -p basemyai-engine --test vector_churn -- --ignored --nocapture`
#[test]
#[ignore = "heavier run (N=10 000), manual: --release + --ignored --nocapture"]
fn ram_churn_recall_n10000() {
    let mut index = VectorIndex::new(VectorIndexParams::with_dim(DIM));
    let (a, b) = run_churn(&mut index, "ram-10k", 10_000, 3, 0.20, 0xC4A1_1F00_2026_0707);
    assert_gate("ram-10k", a, b);
}
