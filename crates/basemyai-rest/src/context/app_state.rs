// SPDX-License-Identifier: BUSL-1.1
//! État applicatif partagé par tous les handlers. Volontairement réduit à
//! deux composants : la résolution de mémoire et la configuration
//! d'exécution — rien de bas niveau (connexion, clé en clair, embedder) n'y
//! transite, ces détails restent internes à `provider`/`memory_registry`.

use std::sync::Arc;

use crate::config::RuntimeConfig;

use super::memory_registry::MemoryRegistry;

#[cfg(feature = "test-util")]
use crate::provider::MemoryProvider;

/// État partagé (cloné par requête — les deux champs sont des `Arc`).
#[derive(Clone)]
pub struct AppState {
    memories: Arc<MemoryRegistry>,
    runtime: Arc<RuntimeConfig>,
}

impl AppState {
    #[must_use]
    pub fn new(memories: Arc<MemoryRegistry>, runtime: Arc<RuntimeConfig>) -> Self {
        Self { memories, runtime }
    }

    #[must_use]
    pub fn memories(&self) -> &MemoryRegistry {
        &self.memories
    }

    #[must_use]
    pub fn runtime(&self) -> &RuntimeConfig {
        &self.runtime
    }

    /// Construit un état de test directement depuis un provider + une config
    /// d'exécution, sans passer par `provider::factory` (pas de fichier, pas
    /// de Candle). Réservé aux tests.
    #[must_use]
    #[cfg(feature = "test-util")]
    pub fn for_test(provider: Arc<dyn MemoryProvider>, runtime: RuntimeConfig) -> Self {
        let registry = MemoryRegistry::new(provider, runtime.agent_policy.clone());
        Self::new(Arc::new(registry), Arc::new(runtime))
    }

    /// Variante de [`Self::for_test`] avec un rate-limit `remember` explicite.
    #[must_use]
    #[cfg(feature = "test-util")]
    pub fn for_test_with_rate_limit(
        provider: Arc<dyn MemoryProvider>,
        runtime: RuntimeConfig,
        limit: usize,
        window: std::time::Duration,
    ) -> Self {
        let registry = MemoryRegistry::with_rate_limit(provider, runtime.agent_policy.clone(), limit, window);
        Self::new(Arc::new(registry), Arc::new(runtime))
    }
}
