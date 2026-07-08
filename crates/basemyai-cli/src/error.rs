// SPDX-License-Identifier: BUSL-1.1
//! Erreur centralisée du CLI. Chaque variante porte un code stable
//! (`code()`) et un exit code (`exit_code()`, voir `exit.rs`) — même pattern
//! que `RestError::parts()` dans `basemyai-rest`, adapté à un process CLI
//! plutôt qu'à une réponse HTTP. Remplace `Box<dyn std::error::Error>` dans
//! toutes les commandes : un script qui appelle `basemyai` peut brancher sur
//! l'exit code ou sur `{"error":{"code":...}}` sans parser de message libre.

use std::path::PathBuf;

use basemyai::MemoryError;
use basemyai_core::CoreError;
use thiserror::Error;

use crate::exit;

/// Erreur du CLI développeur. `#[non_exhaustive]` : de nouvelles variantes
/// peuvent s'ajouter sans casser le contrat (les exit codes existants, eux,
/// ne changent jamais de sens).
#[derive(Debug, Error)]
#[non_exhaustive]
pub(crate) enum CliError {
    /// `BASEMYAI_DB_KEY` absente (chiffrement obligatoire, ADR-007).
    #[error("BASEMYAI_DB_KEY is required (encryption at rest is mandatory)")]
    MissingKey,

    /// `--db`/`--agent` non résolvable (flag, env, et config tous absents).
    #[error("{0}")]
    NotConfigured(String),

    /// `AgentId::new` a rejeté une valeur vide.
    #[error("agent id must not be empty")]
    InvalidAgent,

    /// `init` sur un chemin déjà existant.
    #[error("'{}' already exists", _0.display())]
    AlreadyExists(PathBuf),

    /// Action destructive refusée sans confirmation explicite (`purge --yes`).
    #[error("{0}")]
    ConfirmationRequired(&'static str),

    /// Flags incompatibles passés ensemble (`recall --hybrid --layer --graph`).
    #[error("{0}")]
    MutuallyExclusive(&'static str),

    /// `verify` : conteneur lisible mais format/version/engine inattendus.
    #[error("verification failed")]
    VerificationFailed,

    /// Modèle d'embedding non provisionné — message + hint vers `setup --fetch`.
    #[error("{0}\nhint: run `basemyai setup --fetch` to provision the baseline model")]
    ModelNotProvisioned(String),

    /// Aucun backend LLM local détecté — message + hint vers `llm detect`.
    #[error("{0}\nhint: no local LLM backend detected — run `basemyai llm detect` to diagnose")]
    LlmNotAvailable(String),

    /// Erreur propagée depuis la couche mémoire (`basemyai`).
    #[error(transparent)]
    Memory(#[from] MemoryError),

    /// Erreur propagée depuis le socle (`basemyai-core`), hors du chemin `Memory`
    /// (ouverture du `Store` brut, chargement de l'embedder).
    #[error(transparent)]
    Core(#[from] CoreError),

    /// Lecture/écriture de `~/.basemyai/config.toml`, ou clé de config inconnue.
    #[error("{0}")]
    Config(String),

    /// Échec IO (lecture d'un fichier `--file`, écriture d'un export `--out`).
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// Erreur SQL bas niveau, pour les rares lectures directes hors
    /// `basemyai::storage::MemoryStore` (métadonnées `bmai_meta`, `list`).
    #[error("storage error: {0}")]
    Sql(#[from] basemyai_core::libsql::Error),
}

impl CliError {
    /// Code stable pour la sortie JSON (`{"error":{"code":...}}`). Documenté
    /// dans `docs/cli.md` — ne renomme jamais une valeur existante.
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::MissingKey => "KEY_REQUIRED",
            Self::NotConfigured(_) => "NOT_CONFIGURED",
            Self::InvalidAgent => "INVALID_AGENT",
            Self::AlreadyExists(_) => "ALREADY_EXISTS",
            Self::ConfirmationRequired(_) => "CONFIRMATION_REQUIRED",
            Self::MutuallyExclusive(_) => "USAGE_ERROR",
            Self::VerificationFailed => "VERIFICATION_FAILED",
            Self::ModelNotProvisioned(_) => "MODEL_NOT_PROVISIONED",
            Self::LlmNotAvailable(_) => "LLM_NOT_AVAILABLE",
            Self::Memory(e) => memory_error_code(e),
            Self::Core(e) => core_error_code(e),
            Self::Config(_) => "CONFIG_ERROR",
            Self::Io(_) => "IO_ERROR",
            Self::Sql(_) => "STORAGE_ERROR",
        }
    }

    /// Exit code du process pour cette erreur (voir `exit.rs`).
    pub(crate) fn exit_code(&self) -> u8 {
        match self {
            Self::MissingKey => exit::KEY_ERROR,
            Self::NotConfigured(_) => exit::NOT_CONFIGURED,
            Self::InvalidAgent => exit::VALIDATION,
            Self::AlreadyExists(_) => exit::ALREADY_EXISTS,
            Self::ConfirmationRequired(_) => exit::CONFIRMATION_REQUIRED,
            Self::MutuallyExclusive(_) => exit::USAGE,
            Self::VerificationFailed => exit::VERIFICATION_FAILED,
            Self::ModelNotProvisioned(_) => exit::MODEL_NOT_PROVISIONED,
            Self::LlmNotAvailable(_) => exit::LLM_NOT_AVAILABLE,
            Self::Memory(e) => memory_error_exit(e),
            Self::Core(e) => core_error_exit(e),
            Self::Config(_) => exit::USAGE,
            Self::Io(_) => exit::GENERIC,
            Self::Sql(_) => exit::GENERIC,
        }
    }
}

fn memory_error_code(e: &MemoryError) -> &'static str {
    match e {
        MemoryError::Core(core) => core_error_code(core),
        MemoryError::EncryptionRequired => "KEY_REQUIRED",
        MemoryError::MissingAgent => "INVALID_AGENT",
        MemoryError::UnknownLayer(_) | MemoryError::Porting(_) | MemoryError::TextTooLong { .. } => "VALIDATION_ERROR",
        MemoryError::Inference(_) | MemoryError::Extraction(_) => "LLM_ERROR",
        _ => "INTERNAL_ERROR",
    }
}

fn memory_error_exit(e: &MemoryError) -> u8 {
    match e {
        MemoryError::Core(core) => core_error_exit(core),
        MemoryError::EncryptionRequired => exit::KEY_ERROR,
        MemoryError::MissingAgent => exit::VALIDATION,
        MemoryError::UnknownLayer(_) | MemoryError::Porting(_) | MemoryError::TextTooLong { .. } => exit::VALIDATION,
        _ => exit::GENERIC,
    }
}

fn core_error_code(e: &CoreError) -> &'static str {
    match e {
        CoreError::Encryption => "KEY_ERROR",
        CoreError::ModelNotProvisioned(_) => "MODEL_NOT_PROVISIONED",
        CoreError::Storage(_) | CoreError::Vector(_) => "STORAGE_ERROR",
        CoreError::Embed(_) => "EMBED_ERROR",
        _ => "INTERNAL_ERROR",
    }
}

fn core_error_exit(e: &CoreError) -> u8 {
    match e {
        CoreError::Encryption => exit::KEY_ERROR,
        CoreError::ModelNotProvisioned(_) => exit::MODEL_NOT_PROVISIONED,
        _ => exit::GENERIC,
    }
}
