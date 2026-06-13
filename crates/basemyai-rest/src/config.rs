//! Configuration du sidecar REST. Défauts ← `~/.basemyai/config.toml` `[rest]`
//! ← variables d'environnement. La clé API n'est jamais loguée.

use serde::Deserialize;

use crate::error::RestError;

/// Port HTTP par défaut du sidecar (distinct du MCP : 7744).
pub(crate) const DEFAULT_PORT: u16 = 7743;
/// Timeout par défaut d'une requête (secondes).
pub(crate) const DEFAULT_TIMEOUT_SECS: u64 = 30;
/// Plafond de taille d'une réponse `recall`/`recall_graph` sérialisée (256 KiB).
pub(crate) const DEFAULT_MAX_RESULT_BYTES: usize = 262_144;
/// Plafond de taille du corps de requête (1 MiB).
pub(crate) const DEFAULT_MAX_BODY_BYTES: usize = 1_048_576;

/// Configuration effective.
#[derive(Debug, Clone)]
pub struct Config {
    /// Port d'écoute (`BASEMYAI_REST_PORT`).
    pub port: u16,
    /// Clé API Bearer. `None` + `dev=false` ⇒ démarrage refusé.
    pub api_key: Option<String>,
    /// Mode dev (localhost) : désactive l'auth (`BASEMYAI_REST_DEV=1`).
    pub dev: bool,
    /// Timeout d'une requête.
    pub timeout_secs: u64,
    /// Plafond de réponse sérialisée.
    pub max_result_bytes: usize,
    /// Plafond du corps de requête.
    pub max_body_bytes: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            api_key: None,
            dev: false,
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            max_result_bytes: DEFAULT_MAX_RESULT_BYTES,
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    #[serde(default)]
    rest: RestSection,
}

#[derive(Debug, Default, Deserialize)]
struct RestSection {
    port: Option<u16>,
    api_key: Option<String>,
}

impl Config {
    /// Construit la configuration : défauts ← TOML ← environnement.
    ///
    /// # Errors
    /// [`RestError`] interne si une variable numérique est invalide ou le TOML illisible.
    pub fn from_env() -> Result<Self, RestError> {
        let mut cfg = Self::default();

        if let Some(file) = load_file_config()? {
            if let Some(p) = file.rest.port {
                cfg.port = p;
            }
            if file.rest.api_key.is_some() {
                cfg.api_key = file.rest.api_key;
            }
        }

        if let Ok(p) = std::env::var("BASEMYAI_REST_PORT")
            && let Ok(p) = p.parse()
        {
            cfg.port = p;
        }
        if let Ok(key) = std::env::var("BASEMYAI_REST_API_KEY")
            && !key.is_empty()
        {
            cfg.api_key = Some(key);
        }
        if let Ok(v) = std::env::var("BASEMYAI_REST_DEV") {
            cfg.dev = v == "1" || v.eq_ignore_ascii_case("true");
        }

        Ok(cfg)
    }
}

fn load_file_config() -> Result<Option<FileConfig>, RestError> {
    let Some(home) = dirs::home_dir() else {
        return Ok(None);
    };
    let path = home.join(".basemyai").join("config.toml");
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Ok(None),
    };
    Ok(toml::from_str(&raw).ok())
}
