// SPDX-License-Identifier: BUSL-1.1
//! Configuration du sidecar, séparée en deux profils de durée de vie :
//!
//! - [`StartupConfig`] : consommé une fois au démarrage pour construire le
//!   provider (adresse d'écoute, chemin du conteneur, source de chiffrement,
//!   modèle d'embedding). Jamais relu après boot.
//! - [`RuntimeConfig`] : consulté à chaque requête par les handlers/middlewares
//!   (plafonds, timeout, politique d'agent, clé API). C'est lui que porte
//!   `AppState`.
//!
//! Les deux sont construits en un seul passage TOML + environnement
//! ([`environment::load_raw`]) pour ne pas parser deux fois la même source.

mod environment;
mod validation;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use basemyai_core::EncryptionKey;

use crate::http::RestError;

pub(crate) const DEFAULT_BIND: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
pub(crate) const DEFAULT_PORT: u16 = 7743;
pub(crate) const DEFAULT_TIMEOUT_SECS: u64 = 30;
pub(crate) const DEFAULT_MAX_RESULT_BYTES: usize = 262_144;
pub(crate) const DEFAULT_MAX_BODY_BYTES: usize = 1_048_576;

/// Paramètres consommés une seule fois, à la construction du provider.
#[derive(Debug, Clone)]
pub struct StartupConfig {
    pub bind: IpAddr,
    pub port: u16,
    pub db_path: PathBuf,
    /// Secret de chiffrement au repos. `Debug` masqué par
    /// [`basemyai_core::EncryptionKey`] lui-même — jamais un `String` nu.
    pub db_key: Option<EncryptionKey>,
    pub model_path: Option<PathBuf>,
    pub consent_to_fetch: bool,
}

impl StartupConfig {
    #[must_use]
    pub fn socket_addr(&self) -> SocketAddr {
        SocketAddr::new(self.bind, self.port)
    }
}

impl Default for StartupConfig {
    fn default() -> Self {
        Self {
            bind: DEFAULT_BIND,
            port: DEFAULT_PORT,
            db_path: environment::default_db_path(),
            db_key: None,
            model_path: None,
            consent_to_fetch: false,
        }
    }
}

/// Paramètres consultés à chaque requête. Petit et partagé (`Arc`) via
/// [`crate::context::AppState`].
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub agent_policy: AgentPolicy,
    /// Clé API Bearer. `None` + `dev=false` ⇒ démarrage refusé. `Debug` masqué.
    pub api_key: Option<ApiKey>,
    /// Mode dev (localhost) : désactive l'auth.
    pub dev: bool,
    pub timeout_secs: u64,
    pub max_result_bytes: usize,
    pub max_body_bytes: usize,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
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
    /// Construit la politique depuis une valeur de config : `any` laisse les
    /// requêtes porter leur `agent_id`, toute autre valeur non vide est
    /// traitée comme un agent fixe.
    #[must_use]
    pub fn parse(value: &str) -> Self {
        if value.eq_ignore_ascii_case("any") {
            Self::Any
        } else {
            Self::Fixed(value.to_string())
        }
    }
}

/// Jeton Bearer opaque : `Debug` masqué, jamais logué ni renvoyé au client.
/// Pas de nouvelle dépendance (`secrecy`) pour un seul champ — un wrapper
/// local suffit, sur le même principe que `basemyai_core::EncryptionKey`.
#[derive(Clone)]
pub struct ApiKey(String);

impl ApiKey {
    #[must_use]
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiKey(***)")
    }
}

/// Construit `(StartupConfig, RuntimeConfig)` depuis TOML (`~/.basemyai/config.toml`,
/// section `[rest]`) puis l'environnement (qui a le dernier mot).
///
/// # Errors
/// [`RestError::Config`] si le TOML est illisible ou une variable numérique invalide.
pub fn load() -> Result<(StartupConfig, RuntimeConfig), RestError> {
    environment::load_raw()
}

pub use validation::validate;
