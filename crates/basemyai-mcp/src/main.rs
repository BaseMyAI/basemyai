// SPDX-License-Identifier: BUSL-1.1
//! Binaire `basemyai-mcp` : serveur MCP de production.
//!
//! Wiring de prod : provisioning hardware-aware de l'embedder (Candle) → provider
//! natif **chiffré** (ADR-032) → serveur MCP sur **stdio** (défaut, intégration
//! agent local) ou **HTTP** local (`BASEMYAI_MCP_TRANSPORT=http`).
//!
//! ## Plug-and-play (le cas d'usage principal)
//!
//! Branché dans un client MCP (Claude Code, Claude Desktop, Cursor, Windsurf,
//! ChatGPT Desktop…), ce binaire donne à l'agent une **mémoire persistante** et,
//! via l'outil `consolidate`, une consolidation qui **emprunte le LLM du client**
//! par sampling MCP (ADR-017) — aucun LLM externe requis. Voir `docs/mcp-install.md`.
//!
//! ## Variables d'environnement
//!
//! - `BASEMYAI_DB_KEY` (**requis**) : clé de chiffrement de la base (ADR-007).
//! - `BASEMYAI_FETCH=1` : consent au téléchargement du modèle baseline au 1ᵉʳ run
//!   (sinon, le modèle doit déjà être provisionné — zéro download silencieux, ADR-010).
//! - `BASEMYAI_MCP_TRANSPORT` : `stdio` (défaut) ou `http`.
//! - `BASEMYAI_MCP_API_KEY` : clé Bearer (requise pour le transport HTTP).
//! - `BASEMYAI_MCP_PORT` / `_TIMEOUT_SECS` / `_MAX_RESULT_BYTES` : voir [`Config`].
//!
//! **Privacy-first** : HTTP écoute sur `127.0.0.1` uniquement ; le seul réseau
//! sortant possible est le fetch explicite du modèle au setup.

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    run().await
}

#[cfg(feature = "embed")]
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    use std::sync::Arc;

    use basemyai_core::{CandleEmbedder, Embedder, EncryptionKey, KeyResolveError};
    use basemyai_mcp::{Config, FileProvider, McpServer, run_http, run_stdio};

    // Logs sur STDERR uniquement : en stdio, STDOUT est le canal MCP — y écrire
    // corromprait le protocole. `with_ansi(false)` : sortie propre dans les logs d'hôte.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let config = Config::from_env()?;
    let transport = std::env::var("BASEMYAI_MCP_TRANSPORT").unwrap_or_else(|_| "stdio".to_string());

    // Clé de chiffrement de la base (obligatoire — chiffrement au repos, ADR-007 ;
    // le backend natif chiffre sans CMake, ADR-030).
    let db_key = EncryptionKey::resolve(None).map_err(|e| match e {
        KeyResolveError::Missing(msg) => msg,
        other => other.to_string(),
    })?;

    // Chemin de la base partagée : ~/.basemyai/memory.bmai (isolation par agent
    // structurelle sous préfixe de clé, ADR-006/ADR-027).
    let home = dirs::home_dir().ok_or("cannot resolve home directory")?;
    let db_path = home.join(".basemyai").join("memory.bmai");

    // Embedder : provisioning hardware-aware. Fetch du modèle SEULEMENT si consenti.
    let consent = std::env::var("BASEMYAI_FETCH").map(|v| v == "1").unwrap_or(false);
    let mp = basemyai::provision(consent).await?;
    let embedder: Arc<dyn Embedder> = Arc::new(CandleEmbedder::load(&mp.model_path, mp.device)?);

    let provider = Arc::new(FileProvider::open(db_path, db_key.expose().to_string(), embedder).await?);
    let server = McpServer::new(provider, config.clone());

    match transport.as_str() {
        "stdio" => {
            tracing::info!("basemyai-mcp: stdio transport (model={})", mp.model_id);
            run_stdio(server).await?;
        }
        "http" => {
            tracing::info!(port = config.port, "basemyai-mcp: HTTP transport on 127.0.0.1");
            run_http(server, Arc::new(config)).await?;
        }
        other => {
            return Err(format!("unknown BASEMYAI_MCP_TRANSPORT '{other}' (expected 'stdio' or 'http')").into());
        }
    }

    Ok(())
}

#[cfg(not(feature = "embed"))]
async fn run() -> Result<(), Box<dyn std::error::Error>> {
    Err(
        "basemyai-mcp must be built with the `embed` feature for the production server \
         (it is in the default feature set)"
            .into(),
    )
}
