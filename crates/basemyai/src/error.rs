// SPDX-License-Identifier: BUSL-1.1
//! Erreurs de la couche mémoire. Enveloppe les erreurs du core via `#[from]`.

use thiserror::Error;

/// Erreur de la couche sémantique mémoire.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum MemoryError {
    /// Erreur propagée depuis le socle.
    #[error(transparent)]
    Core(#[from] basemyai_core::CoreError),

    /// Tentative d'ouvrir une mémoire sans clé : le chiffrement est obligatoire
    /// dans `basemyai` (ADR-007).
    #[error("encryption key is required (mandatory in basemyai)")]
    EncryptionRequired,

    /// Requête sans `agent_id` valide : l'isolation est un invariant (ADR-006).
    #[error("a valid agent_id is required (per-agent isolation invariant)")]
    MissingAgent,

    /// Couche mémoire inconnue.
    #[error("unknown memory layer: {0}")]
    UnknownLayer(String),

    /// Échec d'un appel à la couche d'inférence model-agnostic (consolidation).
    /// Le *mécanisme* d'appel LLM est neutre ; cette erreur en remonte l'échec.
    #[error("inference failure: {0}")]
    Inference(String),

    /// Sortie d'extraction (consolidation) inexploitable : JSON malformé ou hors
    /// schéma attendu.
    #[error("extraction parse error: {0}")]
    Extraction(String),

    /// Export/import de mémoire invalide : en-tête absent ou incompatible,
    /// ligne JSONL malformée.
    #[error("porting error: {0}")]
    Porting(String),

    /// Métadonnées d'embedding invalides dans le conteneur `.bmai`.
    #[error("embedding metadata invalid: {0}")]
    EmbeddingMetadata(String),

    /// L'embedder fourni ne correspond pas aux vecteurs déjà stockés.
    #[error(
        "embedding model mismatch: store uses {stored_model} ({stored_dim}d), embedder is {embedder_model} ({embedder_dim}d); export/import to re-index with the new model"
    )]
    EmbeddingModelMismatch {
        /// Modèle enregistré dans le conteneur.
        stored_model: String,
        /// Dimension enregistrée dans le conteneur.
        stored_dim: usize,
        /// Modèle de l'embedder fourni.
        embedder_model: String,
        /// Dimension de l'embedder fourni.
        embedder_dim: usize,
    },

    /// Texte à mémoriser au-delà de la limite de taille (DoS de contexte :
    /// un item démesuré saturerait le prompt de consolidation, qui ne borne
    /// que le *nombre* d'épisodes, pas leur taille individuelle).
    #[error("text too long: {len} bytes (max {max})")]
    TextTooLong {
        /// Taille en octets du texte rejeté.
        len: usize,
        /// Limite autorisée en octets (`Memory::MAX_TEXT_LEN`).
        max: usize,
    },

    /// `page_size == 0` passé à une passe de GC temporel (ADR-038) : une page
    /// vide ne progresserait jamais — rejeté explicitement plutôt que de
    /// renvoyer silencieusement un rapport à zéro qui se lirait comme "rien
    /// n'était expiré".
    #[error("gc page_size must be greater than zero")]
    InvalidGcPageSize,
}

/// Alias de résultat de la couche mémoire.
pub type Result<T> = core::result::Result<T, MemoryError>;
