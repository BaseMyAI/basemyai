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

/// EMBED-TRUNC (audit adversarial BaseMyAI, 2026-07-22) : un texte largement
/// au-delà de la fenêtre `max_position_embeddings` du modèle (512 tokens pour
/// `all-MiniLM-L6-v2`) — mais bien en deçà de `MAX_TEXT_LEN` (65 536
/// caractères) côté REST/MCP, qui ne borne que les octets, jamais les tokens
/// — doit être tronqué silencieusement par le tokenizer plutôt que de
/// paniquer ou de produire un comportement indéfini dans `forward` de
/// `candle-transformers`. Avant la correction (`with_truncation` absent),
/// ce même test aurait soit paniqué à l'intérieur de `BertModel::forward`,
/// soit dépassé la fenêtre de position sans erreur claire — comportement
/// non exercé jusqu'ici faute de test dédié.
#[test]
#[ignore = "exige un modèle local via BASEMYAI_MODEL_DIR (aucun téléchargement)"]
fn text_far_beyond_the_model_position_window_is_truncated_not_panicking() {
    let Some(dir) = model_dir() else {
        eprintln!("BASEMYAI_MODEL_DIR non défini — test ignoré (aucun téléchargement).");
        return;
    };

    let embedder = CandleEmbedder::load(&dir, Device::Cpu).expect("chargement du modèle local");

    // ~5000 mots distincts — largement plus que les 512 tokens WordPiece de
    // la fenêtre du modèle, tout en restant loin de MAX_TEXT_LEN.
    let huge_text = (0..5000).map(|i| format!("word{i}")).collect::<Vec<_>>().join(" ");

    let vector = embedder
        .embed(&huge_text)
        .expect("l'embedding doit réussir malgré le texte surdimensionné");
    assert_eq!(vector.len(), 384, "la sortie reste 384d même pour un texte tronqué");
    let norm: f32 = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
    assert!(
        (norm - 1.0).abs() < 1e-3,
        "vecteur toujours normalisé L2 (norme={norm})"
    );

    // Un lot mixte (un texte normal, un texte surdimensionné) ne doit pas
    // échouer au `stack_u32` — la troncature s'applique par entrée, avant le
    // padding commun du lot, donc les deux lignes restent de même longueur
    // après tokenisation.
    let batch = vec!["short text".to_string(), huge_text];
    let rows = embedder
        .embed_batch(&batch)
        .expect("embed_batch doit réussir sur un lot mixte");
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|v| v.len() == 384));
}
