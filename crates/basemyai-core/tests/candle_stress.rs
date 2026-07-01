//! Stress test long de l'[`CandleEmbedder`] reel.
//!
//! Aucun telechargement : ce test est `#[ignore]` et exige un modele LOCAL via
//! `BASEMYAI_MODEL_DIR`, comme `tests/embed.rs`.
//!
//! ```bash
//! BASEMYAI_MODEL_DIR=/chemin/vers/all-MiniLM-L6-v2 \
//! BASEMYAI_CANDLE_STRESS_SECS=3600 \
//!   cargo test -p basemyai-core --features embed --test candle_stress -- --ignored --nocapture
//! ```

#![cfg(feature = "embed")]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use basemyai_core::{CandleEmbedder, Device, Embedder};

const DEFAULT_STRESS_SECS: u64 = 3_600;
const DEFAULT_BATCH_SIZE: usize = 16;

fn model_dir() -> Option<PathBuf> {
    std::env::var_os("BASEMYAI_MODEL_DIR").map(PathBuf::from)
}

fn stress_duration() -> Duration {
    let secs = std::env::var("BASEMYAI_CANDLE_STRESS_SECS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .unwrap_or(DEFAULT_STRESS_SECS);
    Duration::from_secs(secs)
}

fn batch_size() -> usize {
    std::env::var("BASEMYAI_CANDLE_STRESS_BATCH")
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .filter(|size| *size > 0)
        .unwrap_or(DEFAULT_BATCH_SIZE)
}

#[test]
#[ignore = "exige un modele local via BASEMYAI_MODEL_DIR et tourne longtemps"]
fn candle_embed_batch_stress_keeps_baseline_contract() {
    let Some(dir) = model_dir() else {
        eprintln!("BASEMYAI_MODEL_DIR non defini - stress test ignore (aucun telechargement).");
        return;
    };

    let embedder = CandleEmbedder::load(&dir, Device::Cpu).expect("chargement du modele local");
    assert_eq!(embedder.model_id(), "all-MiniLM-L6-v2");
    assert_eq!(embedder.dim(), 384);

    let duration = stress_duration();
    let batch_size = batch_size();
    let texts: Vec<String> = (0..batch_size)
        .map(|i| format!("BaseMyAI Candle stress sample {i}: deterministic local embedding load."))
        .collect();

    let started = Instant::now();
    let mut iterations = 0_u64;
    while started.elapsed() < duration {
        let vectors = embedder.embed_batch(&texts).expect("embed batch");
        assert_eq!(vectors.len(), texts.len());
        for vector in &vectors {
            assert_eq!(vector.len(), 384);
            let norm = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
            assert!((norm - 1.0).abs() < 1e-3, "vecteur normalise L2 (norme={norm})");
        }
        iterations += 1;

        if iterations.is_multiple_of(100) {
            eprintln!(
                "candle stress: iterations={iterations}, elapsed={:?}, batch={batch_size}",
                started.elapsed()
            );
        }
    }

    assert!(iterations > 0, "le stress test doit executer au moins une iteration");
}
