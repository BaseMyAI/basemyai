//! État applicatif partagé : pool de mémoires par agent + provider + config.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use basemyai::{AgentId, Memory};

use crate::config::Config;
use crate::error::RestError;
use crate::provider::MemoryProvider;

/// État partagé par tous les handlers (cloné par requête — champs `Arc`).
#[derive(Clone)]
pub struct AppState {
    pool: Arc<RwLock<HashMap<String, Arc<Memory>>>>,
    provider: Arc<dyn MemoryProvider>,
    /// Configuration partagée (auth, plafonds).
    pub config: Arc<Config>,
}

impl AppState {
    /// Construit l'état autour d'un provider de mémoire et d'une config.
    #[must_use]
    pub fn new(provider: Arc<dyn MemoryProvider>, config: Config) -> Self {
        Self {
            pool: Arc::new(RwLock::new(HashMap::new())),
            provider,
            config: Arc::new(config),
        }
    }

    /// Récupère (ou ouvre puis met en cache) la mémoire de `agent_id`.
    ///
    /// Ouverture **hors verrou** (I/O), insertion sous `write` lock sans `.await`.
    ///
    /// # Errors
    /// [`RestError::InvalidAgent`] si `agent_id` est vide ; [`RestError::Memory`]
    /// si l'ouverture échoue.
    pub async fn memory_for(&self, agent_id: &str) -> Result<Arc<Memory>, RestError> {
        let agent = AgentId::new(agent_id).ok_or(RestError::InvalidAgent)?;

        if let Some(mem) = self.pool.read().await.get(agent_id) {
            return Ok(Arc::clone(mem));
        }

        let opened = Arc::new(self.provider.open(agent).await?);

        let mut pool = self.pool.write().await;
        Ok(Arc::clone(pool.entry(agent_id.to_string()).or_insert(opened)))
    }
}
