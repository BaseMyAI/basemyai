//! Configuration du serveur MCP. Résolue par précédence : variables
//! d'environnement (prioritaires) → `~/.basemyai/config.toml` section `[mcp]` →
//! valeurs par défaut.
//!
//! La clé API n'est **jamais** loguée. Le transport HTTP l'exige (voir
//! `transport::http`) ; stdio s'en passe (l'autorité est l'opérateur du process).

use std::path::PathBuf;

use serde::Deserialize;

use crate::error::{McpError, Result};

/// Port HTTP par défaut du serveur MCP.
pub(crate) const DEFAULT_PORT: u16 = 7744;
/// Timeout par défaut d'un appel d'outil (secondes).
pub(crate) const DEFAULT_TIMEOUT_SECS: u64 = 60;
/// Plafond par défaut de la taille d'un résultat sérialisé (256 KiB).
pub(crate) const DEFAULT_MAX_RESULT_BYTES: usize = 262_144;

/// Configuration effective du serveur.
#[derive(Debug, Clone)]
pub struct Config {
    /// Port d'écoute HTTP (`BASEMYAI_MCP_PORT`).
    pub port: u16,
    /// Clé API Bearer (HTTP). `None` ⇒ transport HTTP refusé au démarrage.
    pub api_key: Option<String>,
    /// Timeout d'un appel d'outil (`BASEMYAI_MCP_TIMEOUT_SECS`).
    pub timeout_secs: u64,
    /// Plafond de taille d'un résultat sérialisé (`BASEMYAI_MCP_MAX_RESULT_BYTES`).
    pub max_result_bytes: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            api_key: None,
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            max_result_bytes: DEFAULT_MAX_RESULT_BYTES,
        }
    }
}

/// Reflet TOML de la section `[mcp]`.
#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    #[serde(default)]
    mcp: McpSection,
}

#[derive(Debug, Default, Deserialize)]
struct McpSection {
    port: Option<u16>,
    api_key: Option<String>,
    timeout_secs: Option<u64>,
    max_result_bytes: Option<usize>,
}

impl Config {
    /// Construit la configuration : défauts ← fichier TOML ← environnement.
    ///
    /// # Errors
    /// [`McpError::Config`] si une variable d'environnement numérique est
    /// présente mais non parsable, ou si le TOML est illisible.
    pub fn from_env() -> Result<Self> {
        let mut cfg = Self::default();

        if let Some(file) = load_file_config()? {
            if let Some(p) = file.mcp.port {
                cfg.port = p;
            }
            if file.mcp.api_key.is_some() {
                cfg.api_key = file.mcp.api_key;
            }
            if let Some(t) = file.mcp.timeout_secs {
                cfg.timeout_secs = t;
            }
            if let Some(m) = file.mcp.max_result_bytes {
                cfg.max_result_bytes = m;
            }
        }

        if let Some(v) = env_parse("BASEMYAI_MCP_PORT")? {
            cfg.port = v;
        }
        if let Ok(key) = std::env::var("BASEMYAI_MCP_API_KEY")
            && !key.is_empty()
        {
            cfg.api_key = Some(key);
        }
        if let Some(v) = env_parse("BASEMYAI_MCP_TIMEOUT_SECS")? {
            cfg.timeout_secs = v;
        }
        if let Some(v) = env_parse("BASEMYAI_MCP_MAX_RESULT_BYTES")? {
            cfg.max_result_bytes = v;
        }

        Ok(cfg)
    }

    /// Chemin standard du fichier de config (`~/.basemyai/config.toml`).
    #[must_use]
    pub fn default_path() -> Option<PathBuf> {
        dirs::home_dir().map(|h| h.join(".basemyai").join("config.toml"))
    }
}

/// Lit et parse `~/.basemyai/config.toml` s'il existe (absence ⇒ `None`).
fn load_file_config() -> Result<Option<FileConfig>> {
    let Some(path) = Config::default_path() else {
        return Ok(None);
    };
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(McpError::Config(format!("reading {}: {e}", path.display()))),
    };
    toml::from_str(&raw)
        .map(Some)
        .map_err(|e| McpError::Config(format!("parsing {}: {e}", path.display())))
}

/// Parse une variable d'environnement numérique optionnelle.
fn env_parse<T: std::str::FromStr>(key: &str) -> Result<Option<T>>
where
    T::Err: std::fmt::Display,
{
    match std::env::var(key) {
        Ok(s) => s
            .parse::<T>()
            .map(Some)
            .map_err(|e| McpError::Config(format!("{key}: {e}"))),
        Err(_) => Ok(None),
    }
}
