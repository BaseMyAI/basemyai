// SPDX-License-Identifier: BUSL-1.1
//! Résolution des [`Memory`] par agent : la vraie responsabilité qui vivait
//! auparavant éclatée entre `state.rs` (le pool) et `provider.rs` (l'ouverture).
//!
//! Sépare volontairement deux identités qui ne sont **pas** synonymes :
//! - l'identité **logique** de l'agent (`agent_id`, un `AgentId` validé) ;
//! - l'identité **physique** du store qui l'héberge (aujourd'hui : un unique
//!   store natif partagé, injecté au constructeur via [`MemoryProvider`] —
//!   ADR-025/032, mono-écrivain). Plusieurs agents partagent ce store par
//!   préfixe de clé (isolation structurelle, ADR-006) ; le registre n'a donc
//!   qu'**un** provider aujourd'hui, mais son API ne suppose pas
//!   `agent_id == store_id == nom de fichier` — un futur registre
//!   multi-store n'aurait qu'à faire varier le provider par résolution, sans
//!   changer la forme de [`MemoryRegistry::resolve`].
//!
//! Porte aussi le rate-limit `remember` par agent (protection du écrivain
//! unique partagé, ADR-025) : c'est une concurrence sur la **même** ressource
//! que celle que ce module résout, pas une couche métier séparée.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, RwLock};

use basemyai::{AgentId, Memory};

use crate::config::AgentPolicy;
use crate::http::RestError;
use crate::provider::MemoryProvider;

/// Nombre maximal d'appels `remember` autorisés par agent et par fenêtre.
pub(super) const DEFAULT_REMEMBER_RATE_LIMIT: usize = 60;
/// Largeur de la fenêtre glissante du rate limiter `remember`.
pub(super) const DEFAULT_REMEMBER_RATE_WINDOW: Duration = Duration::from_secs(60);

/// Registre des mémoires ouvertes, une par agent. `Send + Sync`, partagé
/// (`Arc`) par tous les handlers.
pub struct MemoryRegistry {
    pool: RwLock<HashMap<String, Arc<Memory>>>,
    provider: Arc<dyn MemoryProvider>,
    agent_policy: AgentPolicy,
    remember_hits: Mutex<HashMap<String, VecDeque<Instant>>>,
    remember_limit: usize,
    remember_window: Duration,
}

impl MemoryRegistry {
    #[must_use]
    pub fn new(provider: Arc<dyn MemoryProvider>, agent_policy: AgentPolicy) -> Self {
        Self {
            pool: RwLock::new(HashMap::new()),
            provider,
            agent_policy,
            remember_hits: Mutex::new(HashMap::new()),
            remember_limit: DEFAULT_REMEMBER_RATE_LIMIT,
            remember_window: DEFAULT_REMEMBER_RATE_WINDOW,
        }
    }

    /// Variante avec limite/fenêtre explicites (tests).
    #[must_use]
    #[cfg(feature = "test-util")]
    pub fn with_rate_limit(
        provider: Arc<dyn MemoryProvider>,
        agent_policy: AgentPolicy,
        limit: usize,
        window: Duration,
    ) -> Self {
        Self {
            remember_limit: limit,
            remember_window: window,
            ..Self::new(provider, agent_policy)
        }
    }

    /// Résout (ouvre au besoin, met en cache) la [`Memory`] logique de
    /// `agent_id`, après application de la politique d'agent du sidecar.
    ///
    /// Ouverture **hors verrou** (I/O potentiellement bloquante côté
    /// provider), insertion sous verrou d'écriture sans tenir de `.await` —
    /// deux résolutions concurrentes du même agent absent ouvrent chacune un
    /// handle, la seconde insertion l'emporte silencieusement sur la
    /// première (l'ouverture redondante est un gaspillage, jamais une
    /// incohérence : les deux handles pointent le même store partagé).
    ///
    /// # Errors
    /// [`RestError::InvalidAgent`] si `agent_id` est vide, invalide, ou
    /// rejeté par [`AgentPolicy::Fixed`] ; [`RestError::Memory`] si
    /// l'ouverture échoue.
    pub async fn resolve(&self, agent_id: &str) -> Result<Arc<Memory>, RestError> {
        match &self.agent_policy {
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

    /// `true` si `agent_id` est encore sous le quota `remember` de la fenêtre
    /// glissante courante (et enregistre l'appel) ; `false` si la limite est
    /// atteinte (l'appelant doit refuser la requête, pas l'enregistrer).
    pub async fn check_remember_rate(&self, agent_id: &str) -> bool {
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

    /// `true` si l'agent a déjà une [`Memory`] ouverte dans le registre —
    /// utilisé par `endpoints::health::ready` : la readiness ne doit **pas**
    /// déclencher une ouverture, seulement rapporter l'état existant.
    #[must_use]
    pub async fn is_provider_reachable(&self) -> bool {
        // Le provider lui-même (store + embedder) est construit une seule
        // fois avant que l'`AppState` n'existe (`provider::factory`) — s'il a
        // survécu jusqu'ici, il est joignable par construction. Pas d'I/O ici :
        // la readiness ne doit jamais être une opération lourde (§14).
        true
    }
}
