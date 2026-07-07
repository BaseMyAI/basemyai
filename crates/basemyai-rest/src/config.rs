// SPDX-License-Identifier: BUSL-1.1
//! Configuration du sidecar REST. Défauts ← `~/.basemyai/config.toml` `[rest]`
//! ← variables d'environnement. Les secrets ne sont jamais logués.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use serde::Deserialize;

use crate::error::RestError;

/// Adresse d'écoute par défaut : boucle locale uniquement.
pub(crate) const DEFAULT_BIND: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
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
    /// Adresse d'écoute (`BASEMYAI_REST_BIND`), boucle locale par défaut.
    pub bind: IpAddr,
    /// Port d'écoute (`BASEMYAI_REST_PORT`).
    pub port: u16,
    /// Chemin du fichier libSQL (`BASEMYAI_REST_DB_PATH` ou `BASEMYAI_DB_PATH`).
    pub db_path: PathBuf,
    /// Clé de chiffrement au repos (`BASEMYAI_REST_DB_KEY` ou `BASEMYAI_DB_KEY`).
    pub db_key: Option<String>,
    /// Chemin d'un modèle d'embedding local (`BASEMYAI_REST_MODEL_PATH` ou `BASEMYAI_MODEL_PATH`).
    pub model_path: Option<PathBuf>,
    /// Consentement explicite au fetch de modèle (`BASEMYAI_REST_FETCH` ou `BASEMYAI_FETCH`).
    pub consent_to_fetch: bool,
    /// Politique d'agent appliquée à toutes les routes métier.
    pub agent_policy: AgentPolicy,
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
            bind: DEFAULT_BIND,
            port: DEFAULT_PORT,
            db_path: default_db_path(),
            db_key: None,
            model_path: None,
            consent_to_fetch: false,
            agent_policy: AgentPolicy::Any,
            api_key: None,
            dev: false,
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            max_result_bytes: DEFAULT_MAX_RESULT_BYTES,
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
        }
    }
}

/// Politique d'agent du sidecar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentPolicy {
    /// Les requêtes peuvent cibler n'importe quel `agent_id`.
    Any,
    /// Toutes les requêtes métier doivent cibler cet agent.
    Fixed(String),
}

impl AgentPolicy {
    /// Construit la politique depuis une valeur de config.
    ///
    /// `any` laisse les requêtes porter leur `agent_id`; toute autre valeur non
    /// vide est traitée comme un agent fixe.
    #[must_use]
    pub fn parse(value: &str) -> Self {
        if value.eq_ignore_ascii_case("any") {
            Self::Any
        } else {
            Self::Fixed(value.to_string())
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
    bind: Option<IpAddr>,
    port: Option<u16>,
    db_path: Option<PathBuf>,
    db_key: Option<String>,
    model_path: Option<PathBuf>,
    consent_to_fetch: Option<bool>,
    agent_policy: Option<String>,
    agent_id: Option<String>,
    api_key: Option<String>,
    dev: Option<bool>,
    timeout_secs: Option<u64>,
    max_result_bytes: Option<usize>,
    max_body_bytes: Option<usize>,
}

impl Config {
    /// Construit la configuration : défauts ← TOML ← environnement.
    ///
    /// # Errors
    /// [`RestError`] interne si une variable numérique est invalide ou le TOML illisible.
    pub fn from_env() -> Result<Self, RestError> {
        let mut cfg = Self::default();

        if let Some(file) = load_file_config()? {
            if let Some(bind) = file.rest.bind {
                cfg.bind = bind;
            }
            if let Some(p) = file.rest.port {
                cfg.port = p;
            }
            if let Some(path) = file.rest.db_path {
                cfg.db_path = expand_home(path);
            }
            if file.rest.db_key.is_some() {
                cfg.db_key = file.rest.db_key;
            }
            if let Some(path) = file.rest.model_path {
                cfg.model_path = Some(expand_home(path));
            }
            if let Some(consent) = file.rest.consent_to_fetch {
                cfg.consent_to_fetch = consent;
            }
            if let Some(policy) = file.rest.agent_policy {
                cfg.agent_policy = AgentPolicy::parse(&policy);
            }
            if let Some(agent_id) = file.rest.agent_id
                && !agent_id.is_empty()
            {
                cfg.agent_policy = AgentPolicy::Fixed(agent_id);
            }
            if file.rest.api_key.is_some() {
                cfg.api_key = file.rest.api_key;
            }
            if let Some(dev) = file.rest.dev {
                cfg.dev = dev;
            }
            if let Some(timeout) = file.rest.timeout_secs {
                cfg.timeout_secs = timeout;
            }
            if let Some(max) = file.rest.max_result_bytes {
                cfg.max_result_bytes = max;
            }
            if let Some(max) = file.rest.max_body_bytes {
                cfg.max_body_bytes = max;
            }
        }

        if let Some(bind) = env_parse::<IpAddr>("BASEMYAI_REST_BIND")? {
            cfg.bind = bind;
        }
        if let Some(p) = env_parse::<u16>("BASEMYAI_REST_PORT")? {
            cfg.port = p;
        }
        if let Some(path) = env_string("BASEMYAI_REST_DB_PATH").or_else(|| env_string("BASEMYAI_DB_PATH")) {
            cfg.db_path = expand_home(PathBuf::from(path));
        }
        if let Some(key) = env_string("BASEMYAI_REST_DB_KEY").or_else(|| env_string("BASEMYAI_DB_KEY")) {
            cfg.db_key = Some(key);
        }
        if let Some(path) = env_string("BASEMYAI_REST_MODEL_PATH").or_else(|| env_string("BASEMYAI_MODEL_PATH")) {
            cfg.model_path = Some(expand_home(PathBuf::from(path)));
        }
        if let Some(consent) = env_bool("BASEMYAI_REST_FETCH").or_else(|| env_bool("BASEMYAI_FETCH")) {
            cfg.consent_to_fetch = consent;
        }
        if let Some(policy) = env_string("BASEMYAI_REST_AGENT_POLICY") {
            cfg.agent_policy = AgentPolicy::parse(&policy);
        }
        if let Some(agent_id) = env_string("BASEMYAI_REST_AGENT_ID").or_else(|| env_string("BASEMYAI_AGENT_ID")) {
            cfg.agent_policy = AgentPolicy::Fixed(agent_id);
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

    /// Adresse socket effective.
    #[must_use]
    pub fn socket_addr(&self) -> SocketAddr {
        SocketAddr::new(self.bind, self.port)
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
    toml::from_str(&raw)
        .map(Some)
        .map_err(|e| RestError::Config(format!("invalid {}: {e}", path.display())))
}

fn default_db_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".basemyai")
        .join("memory.bmai")
}

fn expand_home(path: PathBuf) -> PathBuf {
    let raw = path.to_string_lossy();
    if let Some(stripped) = raw.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(stripped);
    }
    path
}

fn env_string(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn env_bool(name: &str) -> Option<bool> {
    env_string(name).map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

fn env_parse<T>(name: &str) -> Result<Option<T>, RestError>
where
    T: std::str::FromStr,
{
    let Some(value) = env_string(name) else {
        return Ok(None);
    };
    value
        .parse()
        .map(Some)
        .map_err(|_| RestError::Config(format!("{name} has invalid value `{value}`")))
}
