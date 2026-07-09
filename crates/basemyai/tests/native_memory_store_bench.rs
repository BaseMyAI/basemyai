//! KNN bench via the **full** `MemoryStore::recall_vector` path (N5.5,
//! `docs/TODO-NATIVE-ENGINE.md` — "barre hardening M6"), not the bare index:
//! `basemyai-engine`'s own `vector_bench`/`vector_recall` already measure
//! `PersistentVectorIndex` directly (insert/search on raw vectors); this
//! file measures `NativeMemoryStore::put_memory`/`recall_vector` as a real
//! `remember`/`recall` call actually walks them — oversampling ×8
//! (ADR-012), resolving the vector-id → `(agent, id)` mapping, hydrating the
//! memory record, the agent/validity/layer post-filter, and the
//! `last_access` touch. Same "manual harness, print real numbers, no
//! statistics layer" convention as `vector_recall.rs`/`vector_bench.rs`: a
//! fast N=2 000 variant runs in the default gate (`cargo xtask test`), a
//! heavier N=10 000 variant is `#[ignore]`d for manual runs:
//!
//! ```text
//! cargo test --release -p basemyai --features test-util,engine-native \
//!     --test native_memory_store_bench -- --ignored --nocapture
//! ```

#![cfg(feature = "test-util")]

use basemyai::storage::{MemoryStore, NativeMemoryStore};
use basemyai::temporal::Validity;
use basemyai::{AgentId, MemoryLayer};
use basemyai_core::Metric;

/// Product-default embedding dimension (parity with the M6 bench shape,
/// `docs/benchmarks/m6-knn-results-2026-07-01.md`).
const DIM: usize = 384;

/// Deterministic `dim`-dimensional vector from `seed` alone (xorshift64*,
/// same technique as `basemyai_engine::harness::expected_vector` — no `rand`
/// dependency, reproducible across runs). Components in `[-1, 1)`; the
/// first is nudged away from zero so cosine distance always has a
/// well-defined direction.
fn vec_for(seed: u64, dim: usize) -> Vec<f32> {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1).max(1);
    let mut next = move || {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let bits = (state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40) as f32;
        bits / (1u64 << 23) as f32 - 1.0
    };
    let mut v: Vec<f32> = (0..dim).map(|_| next()).collect();
    if v[0].abs() < 0.25 {
        v[0] = if v[0] < 0.0 { -1.0 } else { 1.0 };
    }
    v
}

/// Inserts `n` memories one at a time via `MemoryStore::put_memory` (the
/// real per-item `remember` path, not a bulk shortcut), then times
/// `num_queries` `recall_vector` calls at `k`. Prints real, unaveraged
/// totals plus a derived per-op figure — never a claim beyond what a single
/// process/run on this machine actually measured.
async fn bench_recall_vector(n: usize, num_queries: usize, k: usize) {
    let store = NativeMemoryStore::open_ephemeral().expect("open ephemeral native store");
    let agent = AgentId::new("native-memory-store-bench").expect("agent id");

    let insert_start = std::time::Instant::now();
    for i in 0..n {
        store
            .put_memory(
                &format!("m{i}"),
                &agent,
                MemoryLayer::Episodic,
                &format!("bench document number {i}"),
                Validity::since(0),
                &vec_for(i as u64, DIM),
                "bench",
            )
            .await
            .expect("put_memory");
    }
    let insert_elapsed = insert_start.elapsed();

    let query_start = std::time::Instant::now();
    for q in 0..num_queries {
        // Query vectors from a disjoint seed range so they never collide
        // with an inserted id's exact vector.
        let query = vec_for(1_000_000_000 + q as u64, DIM);
        let got = store
            .recall_vector(&agent, &query, k, None, Metric::Cosine, 0, true)
            .await
            .expect("recall_vector");
        assert_eq!(got.len(), k.min(n), "recall_vector must return k results once n >= k");
    }
    let query_elapsed = query_start.elapsed();

    println!(
        "native_memory_store_bench: n={n} dim={DIM} queries={num_queries} k={k} -> \
         put_memory total {insert_elapsed:?} (~{:.3} ms/put, full MemoryStore path) ; \
         recall_vector total {query_elapsed:?} (~{:.3} ms/query, oversample×8 + hydrate + touch)",
        insert_elapsed.as_secs_f64() * 1000.0 / n as f64,
        query_elapsed.as_secs_f64() * 1000.0 / num_queries as f64,
    );
}

/// Fast enough for the default gate (`cargo xtask test` / CI) — a
/// regression-catching timing signal on every run, not just a manual tool.
#[tokio::test]
async fn recall_vector_bench_n2000() {
    bench_recall_vector(2_000, 50, 10).await;
}

/// Heavier N=10 000 run — manual only, mirrors
/// `vector_recall::recall_at_10_beats_adr_threshold_n10000`'s scale.
#[tokio::test]
#[ignore = "heavier run (N=10 000), manual: --release + --ignored --nocapture"]
async fn recall_vector_bench_n10000() {
    bench_recall_vector(10_000, 50, 10).await;
}
