// SPDX-License-Identifier: BUSL-1.1
//! Erreurs du serveur MCP. Convertibles en [`rmcp::ErrorData`] (le type d'erreur
//! des handlers d'outils MCP) en préservant la catégorie JSON-RPC appropriée.

use thiserror::Error;

/// Erreur du serveur MCP basemyai.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum McpError {
    /// Erreur propagée depuis la couche mémoire.
    #[error("memory error: {0}")]
    Memory(#[from] basemyai::MemoryError),

    /// `agent_id` vide : l'isolation par agent est un invariant (ADR-006).
    #[error("invalid agent_id: must not be empty")]
    InvalidAgentId,

    /// Nom de couche mémoire inconnu fourni par le client.
    #[error("invalid layer: {0}")]
    InvalidLayer(String),

    /// Paramètre hors des bornes documentées (`agent_id`, `text`, `query`,
    /// `k`, `max_depth`, `start`...).
    #[error("validation error: {0}")]
    Validation(String),

    /// Configuration invalide (port, clé API, TOML illisible).
    #[error("config error: {0}")]
    Config(String),

    /// Jeton Bearer absent ou invalide (transport HTTP).
    #[error("unauthorized: missing or invalid Bearer token")]
    Unauthorized,

    /// Erreur de transport (stdio/HTTP).
    #[error("transport error: {0}")]
    Transport(String),

    /// Échec d'une requête de sampling MCP (le serveur a demandé une complétion
    /// au client, qui a refusé, échoué, ou n'a pas la capability `sampling`).
    #[error("sampling error: {0}")]
    Sampling(String),
}

/// Alias de résultat du serveur MCP.
pub type Result<T> = core::result::Result<T, McpError>;

impl From<McpError> for rmcp::ErrorData {
    fn from(e: McpError) -> Self {
        let msg = e.to_string();
        match e {
            // Entrées du client malformées → `invalid_params` (-32602).
            McpError::InvalidAgentId | McpError::InvalidLayer(_) | McpError::Validation(_) => {
                Self::invalid_params(msg, None)
            }
            // Auth → `invalid_request` (-32600).
            McpError::Unauthorized => Self::invalid_request(msg, None),
            // `MemoryError` : mêmes catégories client vs interne que le REST
            // (`http::error::RestError::parts`) — un input client malformé ne
            // doit pas ressortir en `internal_error` côté MCP.
            McpError::Memory(ref inner) => memory_error_data(inner, &msg),
            // MCP-ERR-LEAK (audit adversarial BaseMyAI, 2026-07-22) :
            // défaillances internes (stockage, transport, config, sampling)
            // — jamais `msg`/`e.to_string()` verbatim vers le client (chemins
            // disque, détail moteur interne), même redaction que
            // `RestError::parts` pour les mêmes catégories. Le détail réel
            // est loggué côté serveur, jamais renvoyé.
            McpError::Config(_) => {
                tracing::error!(error = %msg, "config error in MCP handler");
                Self::internal_error("internal error".to_string(), None)
            }
            McpError::Transport(_) => {
                tracing::error!(error = %msg, "transport error in MCP handler");
                Self::internal_error("internal error".to_string(), None)
            }
            McpError::Sampling(_) => {
                tracing::error!(error = %msg, "sampling error in MCP handler");
                Self::internal_error("internal error".to_string(), None)
            }
        }
    }
}

/// Mappe une [`basemyai::MemoryError`] vers la bonne catégorie JSON-RPC.
/// Les variantes causées par une entrée client (agent/layer/bornes de
/// requête) deviennent `invalid_params` — leur message est déjà stable et
/// sans détail interne (agent/layer/bornes), donc renvoyé tel quel, comme
/// côté REST. Le reste (stockage, embedding, chiffrement) devient
/// `internal_error` avec un message générique fixe — jamais `msg` verbatim,
/// qui peut porter un chemin disque ou un détail moteur bas niveau
/// (`CoreError::Storage(String)`/`EngineError` formaté via `#[error(transparent)]`)
/// — le détail réel est loggué côté serveur pour le diagnostic (MCP-ERR-LEAK).
fn memory_error_data(inner: &basemyai::MemoryError, msg: &str) -> rmcp::ErrorData {
    use basemyai::MemoryError as M;
    match inner {
        M::MissingAgent
        | M::UnknownLayer(_)
        | M::Extraction(_)
        | M::InvalidGcPageSize
        | M::InvalidContextTokenBudget
        | M::InvalidContextCandidateLimit { .. }
        | M::InvalidImportance { .. }
        | M::TextTooLong { .. }
        | M::Porting(_) => rmcp::ErrorData::invalid_params(msg.to_string(), None),
        _ => {
            tracing::error!(error = %msg, "internal error in MCP handler");
            rmcp::ErrorData::internal_error("internal error".to_string(), None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MCP-ERR-LEAK regression: a genuine internal storage failure (the
    /// exact scenario the audit demonstrated — a local filesystem path
    /// surfacing in `CoreError::Storage`'s `Display`, via `EngineError`'s
    /// `#[error(transparent)]` chain) must never reach the client verbatim.
    #[test]
    fn storage_error_with_a_local_path_is_redacted_not_forwarded() {
        let secret_path = r"C:\Users\someone\.basemyai\secret-store\wal.log";
        let inner = basemyai_core::CoreError::Storage(format!("io error at {secret_path}: permission denied"));
        let mcp_err = McpError::Memory(basemyai::MemoryError::Core(inner));

        let data: rmcp::ErrorData = mcp_err.into();

        assert!(
            !data.message.contains(secret_path),
            "the local path must not appear in the message sent to the client: {}",
            data.message
        );
        assert_eq!(data.message, "internal error");
    }

    /// Same redaction for the other internal-only variants (config,
    /// transport, sampling) — each carries a free-form `String` that must
    /// never be forwarded verbatim either.
    #[test]
    fn config_transport_sampling_errors_are_all_redacted() {
        for err in [
            McpError::Config("toml parse error at /etc/basemyai/secret.toml line 4".to_string()),
            McpError::Transport("connection reset by peer 10.0.0.7:443".to_string()),
            McpError::Sampling("client refused: internal diagnostic token abc123".to_string()),
        ] {
            let data: rmcp::ErrorData = err.into();
            assert_eq!(data.message, "internal error");
        }
    }

    /// Client-input errors keep their descriptive message — redaction must
    /// not become so aggressive that legitimate validation feedback is lost.
    #[test]
    fn client_input_errors_keep_their_descriptive_message() {
        let data: rmcp::ErrorData = McpError::InvalidLayer("bogus-layer".to_string()).into();
        assert!(data.message.contains("bogus-layer"));

        let inner = basemyai::MemoryError::MissingAgent;
        let data: rmcp::ErrorData = McpError::Memory(inner).into();
        assert!(!data.message.is_empty());
        assert_ne!(data.message, "internal error");
    }
}
