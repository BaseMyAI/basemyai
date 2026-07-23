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
use std::sync::atomic::{AtomicU64, Ordering};
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

/// Cardinalité maximale du registre (REG-GROWTH, audit adversarial BaseMyAI,
/// 2026-07-22) : sous `AgentPolicy::Any`, un appelant authentifié pouvait
/// forcer une croissance non bornée de `pool`/`remember_hits` — un
/// `agent_id` distinct par requête, jamais évincé — en pratique dominée par
/// le canal `tokio::sync::broadcast` que chaque `Memory` alloue
/// (`DEFAULT_EVENT_CAPACITY`, `basemyai::memory::event`), pas par
/// l'embedder (partagé via `Arc`, confirmé lors de l'audit). Généreuse par
/// défaut — ce registre sert un déploiement de confiance mono-token
/// multi-agent, pas une limite produit — mais désormais **finie** : au-delà,
/// l'agent le moins récemment utilisé est évincé (LRU) plutôt que refusé, ce
/// qui ne casse jamais une opération en cours (un appelant qui tient déjà un
/// `Arc<Memory>` issu d'une résolution antérieure n'est pas affecté par le
/// retrait de l'entrée du registre) et rouvre simplement le même store
/// physique partagé au prochain accès.
pub(super) const DEFAULT_MAX_AGENTS: usize = 10_000;

/// Une [`Memory`] en cache, avec l'horodatage de son dernier accès —
/// `AtomicU64` (microsecondes depuis l'ouverture du registre) plutôt qu'un
/// champ simple : mis à jour sous le verrou de **lecture** partagé de
/// [`MemoryRegistry::pool`] au chemin chaud (`resolve` hit), sans jamais
/// forcer un tour de verrou d'écriture juste pour rafraîchir la récence.
struct CachedMemory {
    memory: Arc<Memory>,
    last_used_micros: AtomicU64,
}

/// Microsecond (not millisecond) resolution deliberately — a registry
/// touched many times per millisecond (a hot agent under load, or several
/// `resolve` calls back-to-back in a fast test) must still order distinct
/// touches distinctly; a coarser tick would make LRU eviction pick
/// essentially arbitrarily among same-tick entries.
fn now_micros(epoch: Instant) -> u64 {
    u64::try_from(Instant::now().saturating_duration_since(epoch).as_micros()).unwrap_or(u64::MAX)
}

/// Registre des mémoires ouvertes, une par agent. `Send + Sync`, partagé
/// (`Arc`) par tous les handlers.
pub struct MemoryRegistry {
    pool: RwLock<HashMap<String, CachedMemory>>,
    provider: Arc<dyn MemoryProvider>,
    agent_policy: AgentPolicy,
    remember_hits: Mutex<HashMap<String, VecDeque<Instant>>>,
    remember_limit: usize,
    remember_window: Duration,
    max_agents: usize,
    /// Origine commune des horodatages `last_used_micros` — un `Instant`
    /// fixé à la création du registre, jamais republié : seule la distance
    /// relative entre agents importe pour l'éviction LRU.
    epoch: Instant,
    /// Compteur d'évictions LRU depuis l'ouverture (observabilité — pas
    /// affiché par une route HTTP dédiée aujourd'hui, mais lisible en test
    /// et disponible pour un futur endpoint de stats).
    evictions: AtomicU64,
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
            max_agents: DEFAULT_MAX_AGENTS,
            epoch: Instant::now(),
            evictions: AtomicU64::new(0),
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

    /// Variante avec cardinalité maximale explicite (tests — exercer
    /// l'éviction LRU sans construire 10 000 agents).
    #[must_use]
    #[cfg(feature = "test-util")]
    pub fn with_max_agents(provider: Arc<dyn MemoryProvider>, agent_policy: AgentPolicy, max_agents: usize) -> Self {
        Self {
            max_agents,
            ..Self::new(provider, agent_policy)
        }
    }

    /// Nombre d'agents actuellement résidents dans le registre.
    #[must_use]
    #[cfg(feature = "test-util")]
    pub async fn pool_len(&self) -> usize {
        self.pool.read().await.len()
    }

    /// Nombre d'évictions LRU effectuées depuis l'ouverture du registre.
    #[must_use]
    pub fn eviction_count(&self) -> u64 {
        self.evictions.load(Ordering::Relaxed)
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

        // Hot path: read lock only, touch the atomic recency stamp (no write
        // lock needed to keep LRU order accurate — REG-GROWTH fix).
        if let Some(cached) = self.pool.read().await.get(agent_id) {
            cached.last_used_micros.store(now_micros(self.epoch), Ordering::Relaxed);
            return Ok(Arc::clone(&cached.memory));
        }

        let opened = Arc::new(self.provider.open(agent).await?);

        let mut pool = self.pool.write().await;
        // Re-check under the write lock: a concurrent resolver may have
        // already inserted this agent while `provider.open` ran unlocked —
        // same "redundant open, never inconsistency" posture the original
        // code already documented, now also true for the eviction path
        // below (never evict to make room for an insert that turns out to
        // be a no-op).
        if let Some(cached) = pool.get(agent_id) {
            cached.last_used_micros.store(now_micros(self.epoch), Ordering::Relaxed);
            return Ok(Arc::clone(&cached.memory));
        }

        if pool.len() >= self.max_agents
            && let Some(lru_key) = pool
                .iter()
                .min_by_key(|(_, cached)| cached.last_used_micros.load(Ordering::Relaxed))
                .map(|(key, _)| key.clone())
        {
            pool.remove(&lru_key);
            self.evictions.fetch_add(1, Ordering::Relaxed);
            // `remember_hits` tracks the same key space — drop its entry
            // together so a long-idle, now-evicted agent doesn't also leave
            // a rate-limit entry behind forever (the other half of
            // REG-GROWTH: `check_remember_rate` used to only ever trim the
            // *contents* of each entry's `VecDeque`, never the entry itself).
            self.remember_hits.lock().await.remove(&lru_key);
        }

        let cached = CachedMemory {
            memory: Arc::clone(&opened),
            last_used_micros: AtomicU64::new(now_micros(self.epoch)),
        };
        pool.insert(agent_id.to_string(), cached);
        Ok(opened)
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

#[cfg(all(test, feature = "test-util"))]
mod tests {
    use super::*;
    use crate::provider::InMemoryProvider;

    fn registry_with_cap(max_agents: usize) -> MemoryRegistry {
        MemoryRegistry::with_max_agents(Arc::new(InMemoryProvider::new()), AgentPolicy::Any, max_agents)
    }

    /// REG-GROWTH regression: resolving more distinct agents than
    /// `max_agents` must never let the pool grow past that cap — the
    /// least-recently-used agent is evicted, not refused.
    #[tokio::test]
    async fn pool_never_grows_past_max_agents() {
        let registry = registry_with_cap(3);
        for i in 0..10 {
            registry.resolve(&format!("agent-{i}")).await.expect("resolve");
            assert!(
                registry.pool_len().await <= 3,
                "pool exceeded max_agents after resolving agent-{i}"
            );
        }
        assert_eq!(registry.pool_len().await, 3);
        assert!(
            registry.eviction_count() >= 7,
            "expected at least 7 evictions for 10 inserts into a cap of 3"
        );
    }

    /// Evicting the least-recently-used agent must not evict an agent that
    /// was resolved more recently — otherwise "LRU" is really just "random".
    #[tokio::test]
    async fn eviction_prefers_the_least_recently_used_agent() {
        let registry = registry_with_cap(2);
        registry.resolve("old").await.expect("resolve old");
        registry.resolve("mid").await.expect("resolve mid");
        // Touch "old" again so it becomes the most-recently-used of the two
        // — "mid" is now the LRU one and must be evicted, not "old".
        registry.resolve("old").await.expect("re-resolve old");
        registry
            .resolve("new")
            .await
            .expect("resolve new — forces one eviction");

        assert_eq!(registry.pool_len().await, 2);
        // "old" (touched last among the original pair) must have survived;
        // "mid" (never touched again) must be the one evicted.
        registry
            .resolve("old")
            .await
            .expect("old must still be cheap to resolve");
        assert_eq!(
            registry.eviction_count(),
            1,
            "resolving the still-resident \"old\" agent must not itself evict anything"
        );
    }

    /// Evicting an agent from `pool` must also drop its `remember_hits`
    /// entry — the other half of REG-GROWTH (an evicted agent's rate-limit
    /// bookkeeping must not persist forever independent of the pool).
    #[tokio::test]
    async fn eviction_also_drops_the_evicted_agents_remember_hits_entry() {
        let registry = registry_with_cap(1);
        registry.resolve("agent-a").await.expect("resolve a");
        assert!(
            registry.check_remember_rate("agent-a").await,
            "agent-a's first remember"
        );
        assert!(
            registry.remember_hits.lock().await.contains_key("agent-a"),
            "remember_hits must record agent-a's hit"
        );

        registry
            .resolve("agent-b")
            .await
            .expect("resolve b — evicts agent-a (cap 1)");

        assert!(
            !registry.remember_hits.lock().await.contains_key("agent-a"),
            "agent-a's remember_hits entry must be dropped together with its pool eviction"
        );
    }
}
