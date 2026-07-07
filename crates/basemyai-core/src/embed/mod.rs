// SPDX-License-Identifier: BUSL-1.1
//! Embeddings in-process. Le core **ne télécharge jamais** et **ne détecte
//! jamais** le matériel : il reçoit un chemin de modèle et un [`Device`] déjà
//! résolus par le setup hardware-aware du consommateur (ADR-010).

use crate::Result;

#[cfg(feature = "embed")]
mod candle;
#[cfg(feature = "embed")]
pub use candle::CandleEmbedder;

/// Device de calcul, **résolu par le consommateur**, pas par le core.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Device {
    /// CPU — repli universel.
    #[default]
    Cpu,
    /// GPU CUDA, index du device.
    Cuda(usize),
    /// GPU Apple Metal.
    Metal,
}

/// Transforme du texte en vecteurs, en process.
///
/// Object-safe : consommé via `Box<dyn Embedder>` (injection de dépendance).
pub trait Embedder: Send + Sync {
    /// Vectorise un texte.
    fn embed(&self, text: &str) -> Result<Vec<f32>>;

    /// Vectorise un lot (batch) — plus efficace qu'appel par appel.
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;

    /// Identifiant du modèle (ex. `"all-MiniLM-L6-v2"`). Inscrit dans les
    /// métadonnées pour détecter un changement de modèle et régénérer.
    fn model_id(&self) -> &str;

    /// Dimension des vecteurs produits (ex. `384`).
    fn dim(&self) -> usize;
}
