// SPDX-License-Identifier: BUSL-1.1
//! Couche d'inférence **model-agnostic** (VISION §5.5). La consolidation
//! (épisodes → faits) suppose un LLM, mais BaseMyAI ne s'y couple pas : ce trait
//! abstrait le fournisseur, exactement comme l'[`Embedder`](basemyai_core::Embedder)
//! abstrait le modèle d'embedding.
//!
//! **Mécanisme au consommateur du sens, choix du modèle à l'utilisateur** (esprit
//! ADR-003/010) : le *mécanisme* d'appel est neutre ici ; le *choix* du modèle
//! (local de préférence, *privacy-first* ; distant en option explicite) est une
//! décision produit, fournie par injection via `Box<dyn LlmInference>`.
//!
//! Aucune implémentation concrète n'est livrée en V1 : un fournisseur local
//! (llama.cpp, Ollama…) se branche derrière ce trait sans toucher au pipeline.

use crate::Result;

/// Fournisseur d'inférence textuelle, injecté dans la consolidation.
///
/// Object-safe : consommé via `&dyn LlmInference`.
#[async_trait::async_trait]
pub trait LlmInference: Send + Sync {
    /// Complète un `prompt` et renvoie la réponse brute du modèle.
    ///
    /// # Errors
    /// [`MemoryError::Inference`](crate::MemoryError::Inference) si l'appel échoue.
    async fn complete(&self, prompt: &str) -> Result<String>;

    /// Identifiant du modèle (ex. `"llama-3.1-8b-instruct"`) — provenance/debug.
    fn model_id(&self) -> &str;
}
