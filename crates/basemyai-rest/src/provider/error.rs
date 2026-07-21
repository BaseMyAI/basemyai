// SPDX-License-Identifier: BUSL-1.1
//! Erreurs de construction du provider (résolution de clé, provisioning de
//! l'embedder) : les seules étapes du bootstrap qui ne remontent pas déjà une
//! [`basemyai::MemoryError`] typée. Tout le reste (ouverture du store)
//! traverse ce type via `#[from]`.

use std::path::PathBuf;

/// Erreur de construction du [`super::MemoryProvider`] de production.
/// `#[non_exhaustive]` : de nouvelles étapes de bootstrap (rotation au
/// démarrage, multi-store) pourront ajouter des variantes sans casser les
/// `match` externes.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProviderError {
    /// Aucune clé de chiffrement résoluble (ADR-034 : ni argument explicite,
    /// ni `BASEMYAI_DB_KEY`/`_FILE`, ni `~/.basemyai/key`, ni secret Docker).
    #[error("failed to resolve encryption key: {0}")]
    KeyResolution(String),

    /// Le répertoire parent du conteneur `.bmai` n'a pas pu être créé.
    #[error("failed to prepare data directory {path}: {source}")]
    DataDirectory { path: PathBuf, source: std::io::Error },

    /// Provisioning hardware-aware de l'embedder baseline (ADR-010) échoué —
    /// modèle absent et consentement de fetch non donné, ou fetch échoué.
    #[error("failed to provision the embedding model: {0}")]
    Provisioning(String),

    /// Chargement d'un modèle Candle local (`--model-path` explicite) échoué.
    #[error("failed to load the embedding model: {0}")]
    ModelLoad(String),

    /// Ouverture du store natif (recovery WAL, index, mauvaise clé...).
    #[error(transparent)]
    Memory(#[from] basemyai::MemoryError),
}
