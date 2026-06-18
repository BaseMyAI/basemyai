//! Utilitaires de test (feature `test-util`) : un embedder **déterministe et
//! sans dépendance** pour valider les bindings (Python/Node) et les spikes sans
//! Candle, sans fichier modèle et sans CMake.
//!
//! Réservé aux tests : les vecteurs produits ne sont **pas sémantiques**. Ne
//! jamais l'activer en production.

use basemyai_core::{Embedder, Result};

use crate::EMBEDDING_DIM;

/// Embedder déterministe : projette un texte sur un vecteur stable de dimension
/// [`EMBEDDING_DIM`]. Deux textes identiques produisent le même vecteur (distance
/// cosine nulle) — suffisant pour un roundtrip remember/recall hors-ligne.
pub struct HashEmbedder;

impl HashEmbedder {
    /// Construit l'embedder de test.
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Projette un texte sur un vecteur déterministe non nul.
    fn vec_for(text: &str) -> Vec<f32> {
        let mut v = vec![0.0_f32; EMBEDDING_DIM];
        for (i, b) in text.bytes().enumerate() {
            v[i % EMBEDDING_DIM] += f32::from(b) + 1.0;
        }
        // Garantit un vecteur non nul (cosine indéfini sur le vecteur nul).
        v[0] += 1.0;
        v
    }
}

impl Default for HashEmbedder {
    fn default() -> Self {
        Self::new()
    }
}

impl Embedder for HashEmbedder {
    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        Ok(Self::vec_for(text))
    }

    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| Self::vec_for(t)).collect())
    }

    fn model_id(&self) -> &str {
        "hash-deterministic-testutil"
    }

    fn dim(&self) -> usize {
        EMBEDDING_DIM
    }
}
