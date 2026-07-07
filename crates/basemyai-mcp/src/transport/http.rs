// SPDX-License-Identifier: BUSL-1.1
//! Transport HTTP (MCP Streamable HTTP) monté sur axum, avec auth Bearer
//! obligatoire et timeout par requête.
//!
//! **Privacy-first** : écoute sur `127.0.0.1` uniquement (boucle locale), jamais
//! `0.0.0.0`. L'exposition réseau est une décision explicite de l'opérateur
//! (reverse-proxy), pas un défaut.

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::http::StatusCode;
use rmcp::transport::streamable_http_server::StreamableHttpService;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use tokio::net::TcpListener;
use tower_http::timeout::TimeoutLayer;

use crate::auth::BearerAuthLayer;
use crate::config::Config;
use crate::error::{McpError, Result};
use crate::server::McpServer;

/// Démarre le serveur MCP sur HTTP (`/mcp`), protégé par auth Bearer.
///
/// # Errors
/// - [`McpError::Config`] si aucune clé API n'est configurée (le HTTP l'exige).
/// - [`McpError::Transport`] si le bind ou la boucle de service échoue.
pub async fn run_http(server: McpServer, config: Arc<Config>) -> Result<()> {
    let api_key = config.api_key.clone().ok_or_else(|| {
        McpError::Config(
            "HTTP transport requires an API key (set [mcp].api_key in config.toml or \
             BASEMYAI_MCP_API_KEY)"
                .to_string(),
        )
    })?;

    let mcp = StreamableHttpService::new(
        move || Ok(server.clone()),
        Arc::new(LocalSessionManager::default()),
        Default::default(),
    );

    let app = Router::new()
        .nest_service("/mcp", mcp)
        .layer(BearerAuthLayer::new(api_key))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            Duration::from_secs(config.timeout_secs),
        ));

    let listener = TcpListener::bind(("127.0.0.1", config.port))
        .await
        .map_err(|e| McpError::Transport(e.to_string()))?;
    tracing::info!(port = config.port, "basemyai-mcp HTTP listening on 127.0.0.1");

    axum::serve(listener, app)
        .await
        .map_err(|e| McpError::Transport(e.to_string()))?;
    Ok(())
}
