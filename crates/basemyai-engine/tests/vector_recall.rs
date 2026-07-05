//! Correctness harness for the native vector index (ADR-026, jalon N3):
//! recall@10 of the in-RAM Vamana graph against an exact brute-force oracle.
//!
//! Repo discipline: the harness first, the engine second — this test is the
//! judge of `idx::vector::graph`, with the ADR-026 §6 quality gate
//! **recall@10 ≥ 0.9**. Deterministic throughout: a seeded xorshift64* RNG
//! (no `rand` dependency — this crate doesn't pull it and doesn't need to),
//! so a recall regression is reproducible, never flaky.
//!
//! The brute-force oracle is exactly the "no index" alternative ADR-026
//! rejects for production but keeps "comme référence de mesure du recall".
//!
//! Dataset shape (low intrinsic dimension, and why iid uniform 384d or
//! planted clusters would both be wrong): see `tests/common/mod.rs`.

mod common;

use basemyai_engine::{VectorIndex, VectorIndexParams};
use common::{LatentData, brute_force_top_k};

/// Builds an index over `n` seeded low-intrinsic-dimension vectors, runs
/// `num_queries` seeded queries drawn from the same process, and returns
/// the measured recall@k against the exact oracle.
fn measure_recall(n: usize, dim: usize, num_queries: usize, k: usize, seed: u64) -> f64 {
    let params = VectorIndexParams::with_dim(dim);
    let mut data = LatentData::new(seed, dim);
    let vectors: Vec<Vec<f32>> = (0..n).map(|_| data.point()).collect();

    let mut index = VectorIndex::new(params);
    let insert_start = std::time::Instant::now();
    for (id, v) in vectors.iter().enumerate() {
        index.insert(id as u64, v.clone()).expect("insert must succeed");
    }
    let insert_elapsed = insert_start.elapsed();

    let mut hits = 0usize;
    let query_start = std::time::Instant::now();
    for _ in 0..num_queries {
        let query = data.point();
        let expected = brute_force_top_k(&vectors, &query, k);
        let got = index.search(&query, k).expect("search must succeed");
        assert_eq!(got.len(), k, "index returned fewer than k results at n={n}");
        hits += got.iter().filter(|id| expected.contains(id)).count();
    }
    let query_elapsed = query_start.elapsed();

    let recall = hits as f64 / (num_queries * k) as f64;
    println!(
        "vector_recall: n={n} dim={dim} queries={num_queries} k={k} -> recall@{k} = {recall:.4} \
         (insert total {insert_elapsed:?}, ~{:.2} ms/insert; query total {query_elapsed:?} \
         incl. brute-force oracle)",
        insert_elapsed.as_secs_f64() * 1000.0 / n as f64,
    );
    recall
}

/// ADR-026 §6 quality gate at N=2 000 — fast enough for the default gate
/// (`cargo xtask test` / CI Test step).
#[test]
fn recall_at_10_beats_adr_threshold_n2000() {
    let recall = measure_recall(2_000, 384, 50, 10, 0xBA5E_A126_2026_0704);
    assert!(
        recall >= 0.9,
        "recall@10 = {recall:.4} < 0.9 (ADR-026 §6 exit criterion) at N=2000"
    );
}

/// Same gate at N=10 000 — heavier, for manual runs:
/// `cargo test --release -p basemyai-engine --test vector_recall -- --ignored --nocapture`
#[test]
#[ignore = "heavier run (N=10 000), manual: --release + --ignored --nocapture"]
fn recall_at_10_beats_adr_threshold_n10000() {
    let recall = measure_recall(10_000, 384, 50, 10, 0xD15C_A44A_2026_0704);
    assert!(
        recall >= 0.9,
        "recall@10 = {recall:.4} < 0.9 (ADR-026 §6 exit criterion) at N=10000"
    );
}
