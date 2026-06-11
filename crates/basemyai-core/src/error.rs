//! Erreurs du socle. `thiserror` (règle Rust 2026 : structuré et matchable en lib).

use thiserror::Error;

/// Erreur du socle agnostique. `#[non_exhaustive]` pour la forward-compat.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CoreError {
    /// Échec d'ouverture, de migration ou d'accès au store.
    #[error("storage error: {0}")]
    Storage(String),

    /// Échec côté index vectoriel (sqlite-vec).
    #[error("vector index error: {0}")]
    Vector(String),

    /// Échec d'inférence d'embedding (Candle).
    #[error("embedding error: {0}")]
    Embed(String),

    /// Clé absente/invalide, ou base chiffrée illisible.
    #[error("encryption error")]
    Encryption,

    /// Modèle non provisionné : le setup hardware-aware doit tourner d'abord
    /// (le core ne télécharge jamais — cf. ADR-003 / ADR-010).
    #[error("model not provisioned: {0} (run the hardware-aware setup first)")]
    ModelNotProvisioned(String),
}

/// Alias de résultat du socle.
pub type Result<T> = core::result::Result<T, CoreError>;
