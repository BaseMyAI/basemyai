// SPDX-License-Identifier: BUSL-1.1
//! Cosine distance over `f32` slices (ADR-026 — same metric as the M6
//! parity benchmarks and the libSQL backend's `vector_distance_cos`).
//!
//! Dimension is parametric (the product default is 384, `all-MiniLM-L6-v2`,
//! see `idx::vector::meta::DEFAULT_DIM`). The hot loop is a single
//! contiguous fold over both slices — deliberately boring: LLVM
//! autovectorizes this shape well, and no manual chunking/SIMD is attempted
//! here (correctness harness first; ADR-026 promises no performance number).

/// Cosine *distance* in `[0, 2]`: `1 - cos(a, b)`. Lower is closer;
/// `0` means identical direction.
///
/// If either vector has (near-)zero norm the direction is undefined and the
/// distance is conventionally `1.0` (orthogonal-equivalent), never `NaN` —
/// callers sort by this value and a `NaN` would poison the ordering.
///
/// Both slices must have the same length; in debug builds a mismatch panics,
/// in release the shorter length wins (via `zip`). The index (`graph.rs`)
/// enforces the dimension invariant at its API boundary so this stays a
/// branch-free hot path.
#[must_use]
pub fn cosine_distance(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len(), "cosine_distance: dimension mismatch");
    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;
    for (&x, &y) in a.iter().zip(b) {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    let denom = (norm_a * norm_b).sqrt();
    if denom <= f32::EPSILON {
        return 1.0;
    }
    1.0 - dot / denom
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_vectors_have_zero_distance() {
        let v = vec![0.3, -1.2, 4.5, 0.0];
        assert!(cosine_distance(&v, &v).abs() < 1e-6);
    }

    #[test]
    fn scaled_vectors_have_zero_distance() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![2.0, 4.0, 6.0];
        assert!(cosine_distance(&a, &b).abs() < 1e-6);
    }

    #[test]
    fn orthogonal_vectors_have_distance_one() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        assert!((cosine_distance(&a, &b) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn opposite_vectors_have_distance_two() {
        let a = vec![1.0, 0.0];
        let b = vec![-1.0, 0.0];
        assert!((cosine_distance(&a, &b) - 2.0).abs() < 1e-6);
    }

    #[test]
    fn zero_norm_yields_one_not_nan() {
        let a = vec![0.0, 0.0, 0.0];
        let b = vec![1.0, 2.0, 3.0];
        let d = cosine_distance(&a, &b);
        assert!(!d.is_nan());
        assert!((d - 1.0).abs() < 1e-6);
    }

    #[test]
    fn works_at_384_dimensions() {
        let a: Vec<f32> = (0..384).map(|i| (i as f32).sin()).collect();
        let b: Vec<f32> = (0..384).map(|i| (i as f32).cos()).collect();
        let d = cosine_distance(&a, &b);
        assert!(d.is_finite());
        assert!((0.0..=2.0).contains(&d));
    }
}
