//! Shared test-data machinery for the vector-index harnesses
//! (`tests/vector_recall.rs`, `tests/vector_persistence.rs`): a seeded
//! deterministic RNG, the low-intrinsic-dimension dataset generator, and the
//! exact brute-force oracle.
//!
//! ## Why the data has low intrinsic dimension, not iid uniform 384d
//!
//! Measured facts (2026-07-04, the recall harness, seeded):
//! - iid-uniform 384d vectors: recall@10 = 0.988 at N=2 000 but 0.664 at
//!   N=10 000 with default params, and even L=200/R=64 only reaches 0.946
//!   at ~28 ms/insert. Not an implementation bug: pairwise cosines of iid
//!   384d vectors concentrate around 0 with σ ≈ 1/√384 — every point is
//!   nearly equidistant from every other, so there is no neighborhood
//!   structure for *any* graph-ANN (this family or HNSW) to navigate. The
//!   ANN literature benchmarks on real datasets for exactly this reason.
//! - 64 mutually near-orthogonal planted clusters: 1.000 at N=2 000 but
//!   0.282 at N=10 000 — the opposite pathology: all non-target clusters
//!   are equidistant from a query (no gradient between clusters), and once
//!   clusters are larger than R every node's neighbors are intra-cluster,
//!   so the graph fragments into islands.
//!
//! The product's actual data is MiniLM sentence embeddings, whose defining
//! property is **low intrinsic dimensionality** (a continuous semantic
//! manifold inside the 384d ambient space). The generator models exactly
//! that: points are seeded random latents in a [`LATENT_DIM`]-dimensional
//! space, embedded into the ambient space through a fixed seeded random
//! linear map (queries drawn from the same process). The oracle stays exact
//! brute-force over all N ambient vectors, so recall gates measure true
//! recall.

// Each integration-test binary compiles this module independently and not
// every binary uses every helper; and `pub` here only ever means "visible to
// the test binary that compiled me", so `unreachable_pub` is noise.
#![allow(dead_code)]
#![allow(unreachable_pub)]

use basemyai_engine::idx::vector::distance::cosine_distance;

/// Deterministic xorshift64* PRNG — tiny, seedable, good enough for
/// generating test vectors (these are correctness harnesses, not
/// statistical ones).
pub struct XorShift64 {
    state: u64,
}

impl XorShift64 {
    pub fn new(seed: u64) -> Self {
        Self {
            state: seed.max(1), // xorshift must never be seeded with 0
        }
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in [-1, 1).
    pub fn next_f32(&mut self) -> f32 {
        let bits = (self.next_u64() >> 40) as f32; // 24 random bits
        bits / (1u64 << 23) as f32 - 1.0
    }

    pub fn vector(&mut self, dim: usize) -> Vec<f32> {
        (0..dim).map(|_| self.next_f32()).collect()
    }
}

/// Intrinsic (latent) dimensionality of the generated data — the manifold
/// dimension, deliberately much smaller than the ambient space (see the
/// module doc).
pub const LATENT_DIM: usize = 16;

/// Seeded low-intrinsic-dimension dataset (see the module doc): each point
/// is a uniform latent in [-1, 1]^LATENT_DIM pushed through a fixed seeded
/// random linear map into the ambient `dim`-dimensional space.
pub struct LatentData {
    rng: XorShift64,
    /// `LATENT_DIM` ambient basis vectors, fixed for the dataset.
    basis: Vec<Vec<f32>>,
    dim: usize,
}

impl LatentData {
    pub fn new(seed: u64, dim: usize) -> Self {
        let mut rng = XorShift64::new(seed);
        let basis = (0..LATENT_DIM).map(|_| rng.vector(dim)).collect();
        Self { rng, basis, dim }
    }

    pub fn point(&mut self) -> Vec<f32> {
        let latent = self.rng.vector(LATENT_DIM);
        let mut ambient = vec![0.0f32; self.dim];
        for (z, axis) in latent.iter().zip(&self.basis) {
            for (out, &component) in ambient.iter_mut().zip(axis) {
                *out += z * component;
            }
        }
        ambient
    }
}

/// Exact top-k by cosine distance — the oracle.
pub fn brute_force_top_k(vectors: &[Vec<f32>], query: &[f32], k: usize) -> Vec<u64> {
    let mut scored: Vec<(u64, f32)> = vectors
        .iter()
        .enumerate()
        .map(|(id, v)| (id as u64, cosine_distance(query, v)))
        .collect();
    scored.sort_by(|a, b| a.1.total_cmp(&b.1));
    scored.into_iter().take(k).map(|(id, _)| id).collect()
}
