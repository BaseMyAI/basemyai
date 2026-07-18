// SPDX-License-Identifier: BUSL-1.1
//! Erreurs du socle. `thiserror` (règle Rust 2026 : structuré et matchable en lib).

use thiserror::Error;

/// Erreur du socle agnostique. `#[non_exhaustive]` pour la forward-compat.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CoreError {
    /// Échec d'ouverture, de migration ou d'accès au store.
    #[error("storage error: {0}")]
    Storage(String),

    /// Échec côté index vectoriel (moteur natif LM-DiskANN).
    #[error("vector index error: {0}")]
    Vector(String),

    /// Échec d'inférence d'embedding (Candle).
    #[error("embedding error: {0}")]
    Embed(String),

    /// Opération chiffrement sur un store non chiffré (ex. `rotate_key`).
    #[error("encryption error")]
    Encryption,

    /// Store chiffré ouvert sans clé.
    #[error("encryption key required")]
    EncryptionKeyRequired,

    /// Clé fournie mais ne déverrouille pas le store (DEK invalide).
    #[error("wrong encryption key")]
    WrongEncryptionKey,

    /// Fichier `crypto.meta` structurellement invalide (pas un cas « mauvaise clé »).
    #[error("corrupt encryption metadata")]
    CorruptEncryptionMetadata,

    /// Le store est déjà détenu par un autre writer.
    #[error("store is locked by another writer")]
    StoreLocked,

    /// Métadonnées de génération structurellement invalides.
    #[error("corrupt store generation metadata")]
    CorruptStoreGenerationMetadata,

    /// Store déjà en clair : impossible d'appliquer une clé a posteriori.
    #[error("plaintext store cannot be encrypted in place")]
    PlaintextStoreEncryptedKeySupplied,

    /// Modèle non provisionné : le setup hardware-aware doit tourner d'abord
    /// (le core ne télécharge jamais — cf. ADR-003 / ADR-010).
    #[error("model not provisioned: {0} (run the hardware-aware setup first)")]
    ModelNotProvisioned(String),
}

/// Alias de résultat du socle.
pub type Result<T> = core::result::Result<T, CoreError>;
