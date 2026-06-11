//! Test d'intégration de l'[`CandleEmbedder`] réel.
//!
//! **Aucun téléchargement** : ce test est `#[ignore]` et exige un modèle LOCAL
//! déjà provisionné. Pour le lancer, pointe `BASEMYAI_MODEL_DIR` vers un dossier
//! contenant `config.json`, `tokenizer.json` et `model.safetensors` du modèle
//! `all-MiniLM-L6-v2`, puis :
//!
//! ```bash
//! BASEMYAI_MODEL_DIR=/chemin/vers/all-MiniLM-L6-v2 \
//!   cargo test -p basemyai-core --features embed --test embed -- --ignored
//! ```
//!
//! En l'absence du modèle, le test est ignoré : il ne télécharge ni n'échoue.

#![cfg(feature = "embed")]

use std::path::PathBuf;

use basemyai_core::{CandleEmbedder, Device, Embedder};

/// Résout le dossier modèle depuis l'environnement (jamais de fetch réseau).
fn model_dir() -> Option<PathBuf> {
    std::env::var_os("BASEMYAI_MODEL_DIR").map(PathBuf::from)
}

#[test]
#[ignore = "exige un modèle local via BASEMYAI_MODEL_DIR (aucun téléchargement)"]
fn loads_and_produces_384_dim_stable_vectors() {
    let Some(dir) = model_dir() else {
        eprintln!("BASEMYAI_MODEL_DIR non défini — test ignoré (aucun téléchargement).");
        return;
    };

    let embedder = CandleEmbedder::load(&dir, Device::Cpu).expect("chargement du modèle local");

    // Contrat de dimension du baseline V1.
    assert_eq!(embedder.dim(), 384, "le baseline produit des vecteurs 384d");
    assert_eq!(embedder.model_id(), "all-MiniLM-L6-v2");

    // Déterminisme : deux textes identiques → vecteurs ~identiques.
    let a = embedder.embed("the sky is blue").expect("embed a");
    let b = embedder.embed("the sky is blue").expect("embed b");
    assert_eq!(a.len(), 384);
    assert_eq!(b.len(), 384);

    let cosine: f32 = a.iter().zip(&b).map(|(x, y)| x * y).sum();
    assert!(
        (cosine - 1.0).abs() < 1e-4,
        "deux textes identiques doivent donner des vecteurs ~identiques (cosine={cosine})"
    );

    // Les vecteurs sont normalisés L2 (norme ≈ 1).
    let norm: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!((norm - 1.0).abs() < 1e-4, "vecteur normalisé L2 (norme={norm})");
}
