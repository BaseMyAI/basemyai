//! État applicatif partagé : pool de mémoires par agent + provider + config.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, RwLock};

use basemyai::{AgentId, Memory};

use crate::config::{AgentPolicy, Config};
use crate::error::RestError;
use crate::provider::MemoryProvider;

/// Nombre maximal d'appels `remember` autorisés par agent et par fenêtre
/// (déni de service applicatif / saturation de la consolidation LLM).
pub(crate) const REMEMBER_RATE_LIMIT: usize = 60;
/// Largeur de la fenêtre glissante du rate limiter `remember`.
pub(crate) const REMEMBER_RATE_WINDOW: Duration = Duration::from_secs(60);

/// État partagé par tous les handlers (cloné par requête — champs `Arc`).
#[derive(Clone)]
pub struct AppState {
    pool: Arc<RwLock<HashMap<String, Arc<Memory>>>>,
    provider: Arc<dyn MemoryProvider>,
    /// Configuration partagée (auth, plafonds).
    pub config: Arc<Config>,
    /// Fenêtre glissante de timestamps `remember` par agent (Fix 3, ADR audit sécurité).
    remember_hits: Arc<Mutex<HashMap<String, VecDeque<Instant>>>>,
    /// Limite et fenêtre effectives (constantes par défaut, surchageables en test).
    remember_limit: usize,
    remember_window: Duration,
}

impl AppState {
    /// Construit l'état autour d'un provider de mémoire et d'une config.
    #[must_use]
    pub fn new(provider: Arc<dyn MemoryProvider>, config: Config) -> Self {
        Self {
            pool: Arc::new(RwLock::new(HashMap::new())),
            provider,
            config: Arc::new(config),
            remember_hits: Arc::new(Mutex::new(HashMap::new())),
            remember_limit: REMEMBER_RATE_LIMIT,
            remember_window: REMEMBER_RATE_WINDOW,
        }
    }

    /// Variante de [`Self::new`] avec une limite/fenêtre explicites (tests).
    #[must_use]
    #[cfg(feature = "test-util")]
    pub fn with_rate_limit(provider: Arc<dyn MemoryProvider>, config: Config, limit: usize, window: Duration) -> Self {
        Self {
            remember_limit: limit,
            remember_window: window,
            ..Self::new(provider, config)
        }
    }

    /// `true` si `agent_id` est encore sous le quota `remember` de la fenêtre
    /// glissante courante (et enregistre l'appel) ; `false` si la limite est
    /// atteinte (l'appelant doit refuser la requête, pas l'enregistrer).
    pub(crate) async fn check_remember_rate(&self, agent_id: &str) -> bool {
        let now = Instant::now();
        let mut hits = self.remember_hits.lock().await;
        let window = self.remember_window;
        let entry = hits.entry(agent_id.to_string()).or_default();
        while let Some(oldest) = entry.front() {
            if now.duration_since(*oldest) > window {
                entry.pop_front();
            } else {
                break;
            }
        }
        if entry.len() >= self.remember_limit {
            return false;
        }
        entry.push_back(now);
        true
    }

    /// Récupère (ou ouvre puis met en cache) la mémoire de `agent_id`.
    ///
    /// Ouverture **hors verrou** (I/O), insertion sous `write` lock sans `.await`.
    ///
    /// # Errors
    /// [`RestError::InvalidAgent`] si `agent_id` est vide ; [`RestError::Memory`]
    /// si l'ouverture échoue.
    pub async fn memory_for(&self, agent_id: &str) -> Result<Arc<Memory>, RestError> {
        match &self.config.agent_policy {
            AgentPolicy::Any => {}
            AgentPolicy::Fixed(allowed) if allowed == agent_id => {}
            AgentPolicy::Fixed(_) => return Err(RestError::InvalidAgent),
        }

        let agent = AgentId::new(agent_id).ok_or(RestError::InvalidAgent)?;

        if let Some(mem) = self.pool.read().await.get(agent_id) {
            return Ok(Arc::clone(mem));
        }

        let opened = Arc::new(self.provider.open(agent).await?);

        let mut pool = self.pool.write().await;
        Ok(Arc::clone(pool.entry(agent_id.to_string()).or_insert(opened)))
    }
}
