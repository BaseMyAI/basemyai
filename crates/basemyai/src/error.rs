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
}

/// Alias de résultat de la couche mémoire.
pub type Result<T> = core::result::Result<T, MemoryError>;
