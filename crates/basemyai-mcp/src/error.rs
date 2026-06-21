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
            // Défaillances internes (stockage, embedding, transport, config, sampling).
            McpError::Memory(_) | McpError::Config(_) | McpError::Transport(_) | McpError::Sampling(_) => {
                Self::internal_error(msg, None)
            }
        }
    }
}
