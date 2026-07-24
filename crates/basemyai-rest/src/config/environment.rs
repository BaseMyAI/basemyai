// SPDX-License-Identifier: BUSL-1.1
//! Chargement effectif : défauts ← `~/.basemyai/config.toml` `[rest]` ←
//! variables d'environnement. Un seul passage, qui alimente à la fois
//! [`super::StartupConfig`] et [`super::RuntimeConfig`].

use std::net::IpAddr;
use std::path::PathBuf;

use basemyai_core::EncryptionKey;
use serde::Deserialize;

use super::{AgentPolicy, ApiKey, RuntimeConfig, StartupConfig};
use crate::http::RestError;

pub(super) fn default_db_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".basemyai")
        .join("memory.bmai")
}

pub(super) fn load_raw() -> Result<(StartupConfig, RuntimeConfig), RestError> {
    let mut startup = StartupConfig::default();
    let mut runtime = RuntimeConfig::default();
    let mut db_key_raw: Option<String> = None;

    if let Some(file) = load_file_config()? {
        if let Some(bind) = file.rest.bind {
            startup.bind = bind;
        }
        if let Some(p) = file.rest.port {
            startup.port = p;
        }
        if let Some(path) = file.rest.db_path {
            startup.db_path = expand_home(path);
        }
        if file.rest.db_key.is_some() {
            eprintln!(
                "warning: [rest].db_key in {} is ignored (ADR-034); use BASEMYAI_DB_KEY, \
                 BASEMYAI_DB_KEY_FILE, ~/.basemyai/key, or /run/secrets/basemyai_db_key",
                config_file_path().display()
            );
        }
        if let Some(path) = file.rest.model_path {
            startup.model_path = Some(expand_home(path));
        }
        if let Some(consent) = file.rest.consent_to_fetch {
            startup.consent_to_fetch = consent;
        }
        if let Some(policy) = file.rest.agent_policy {
            runtime.agent_policy = AgentPolicy::parse(&policy);
        }
        if let Some(agent_id) = file.rest.agent_id
            && !agent_id.is_empty()
        {
            runtime.agent_policy = AgentPolicy::Fixed(agent_id);
        }
        if file.rest.api_key.is_some() {
            eprintln!(
                "warning: [rest].api_key in {} is ignored — set BASEMYAI_REST_API_KEY",
                config_file_path().display()
            );
        }
        if let Some(dev) = file.rest.dev {
            runtime.dev = dev;
        }
        if let Some(timeout) = file.rest.timeout_secs {
            runtime.timeout_secs = timeout;
        }
        if let Some(max) = file.rest.max_result_bytes {
            runtime.max_result_bytes = max;
        }
        if let Some(max) = file.rest.max_body_bytes {
            runtime.max_body_bytes = max;
        }
    }

    if let Some(bind) = env_parse::<IpAddr>("BASEMYAI_REST_BIND")? {
        startup.bind = bind;
    }
    if let Some(p) = env_parse::<u16>("BASEMYAI_REST_PORT")? {
        startup.port = p;
    }
    if let Some(path) = env_string("BASEMYAI_REST_DB_PATH").or_else(|| env_string("BASEMYAI_DB_PATH")) {
        startup.db_path = expand_home(PathBuf::from(path));
    }
    if let Some(key) = env_string("BASEMYAI_REST_DB_KEY").or_else(|| env_string("BASEMYAI_DB_KEY")) {
        db_key_raw = Some(key);
    }
    if let Some(path) = env_string("BASEMYAI_REST_MODEL_PATH").or_else(|| env_string("BASEMYAI_MODEL_PATH")) {
        startup.model_path = Some(expand_home(PathBuf::from(path)));
    }
    if let Some(consent) = env_bool("BASEMYAI_REST_FETCH").or_else(|| env_bool("BASEMYAI_FETCH")) {
        startup.consent_to_fetch = consent;
    }
    if let Some(policy) = env_string("BASEMYAI_REST_AGENT_POLICY") {
        runtime.agent_policy = AgentPolicy::parse(&policy);
    }
    if let Some(agent_id) = env_string("BASEMYAI_REST_AGENT_ID").or_else(|| env_string("BASEMYAI_AGENT_ID")) {
        runtime.agent_policy = AgentPolicy::Fixed(agent_id);
    }
    if let Ok(key) = std::env::var("BASEMYAI_REST_API_KEY")
        && !key.is_empty()
    {
        runtime.api_key = Some(ApiKey(key));
    }
    if let Ok(v) = std::env::var("BASEMYAI_REST_DEV") {
        runtime.dev = v == "1" || v.eq_ignore_ascii_case("true");
    }

    // Le matériau de clé brut ne survit jamais au-delà de cette fonction sous
    // forme de `String` nue : il est immédiatement enveloppé (ADR-034 — le
    // mode passphrase se choisit ailleurs, la résolution centralisée
    // `EncryptionKey::resolve` gère déjà tous les cas ; ici on ne wrap que ce
    // que l'utilisateur a explicitement fourni au sidecar).
    startup.db_key = db_key_raw.map(EncryptionKey::raw);

    Ok((startup, runtime))
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

fn load_file_config() -> Result<Option<FileConfig>, RestError> {
    let path = config_file_path();
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Ok(None),
    };
    toml::from_str(&raw)
        .map(Some)
        .map_err(|e| RestError::Config(format!("invalid {}: {e}", path.display())))
}

fn config_file_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".basemyai")
        .join("config.toml")
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
