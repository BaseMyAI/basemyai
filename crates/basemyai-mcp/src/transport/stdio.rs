// SPDX-License-Identifier: BUSL-1.1
//! Transport stdio : le serveur parle MCP sur stdin/stdout, modèle d'intégration
//! par défaut des agents locaux (Claude Desktop, etc.).
//!
//! Pas d'auth par appel : l'autorité est l'opérateur qui lance le process. Si
//! stdin est un TTY (lancement manuel plutôt que par pipe d'un hôte MCP), on
//! émet un avertissement — c'est presque toujours une erreur de configuration.

use std::io::IsTerminal;

use rmcp::ServiceExt;
use rmcp::transport::stdio;

use crate::error::{McpError, Result};
use crate::server::McpServer;

/// Démarre le serveur sur stdio et attend la fin de la session.
///
/// # Errors
/// [`McpError::Transport`] si l'initialisation ou la boucle de service échoue.
pub async fn run_stdio(server: McpServer) -> Result<()> {
    if std::io::stdin().is_terminal() {
        tracing::warn!(
            "stdio transport started from a terminal, not a pipe: tool calls run with \
             operator authority and there is no per-call authentication"
        );
    }

    let service = server
        .serve(stdio())
        .await
        .map_err(|e| McpError::Transport(e.to_string()))?;
    service
        .waiting()
        .await
        .map_err(|e| McpError::Transport(e.to_string()))?;
    Ok(())
}
