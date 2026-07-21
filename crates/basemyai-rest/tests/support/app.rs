//! Construction d'une application de test : `InMemoryProvider` (ni CMake ni
//! Candle), sans jamais ouvrir de socket réseau (`build_app` retourne un
//! `Router` directement exercé via `tower::ServiceExt::oneshot`).

use std::sync::Arc;
use std::time::Duration;

use axum::Router;

use basemyai_rest::{AgentPolicy, ApiKey, AppState, InMemoryProvider, RuntimeConfig, build_app};

pub(crate) const KEY: &str = "test-secret-key";

/// Application par défaut : agent policy `Any`, clé API fixe, aucune limite
/// resserrée.
pub(crate) fn app() -> Router {
    let runtime = RuntimeConfig {
        api_key: Some(ApiKey::new(KEY)),
        ..RuntimeConfig::default()
    };
    build_app(AppState::for_test(Arc::new(InMemoryProvider::new()), runtime))
}

/// Application en mode `dev` (pas d'auth).
pub(crate) fn app_dev_mode() -> Router {
    let runtime = RuntimeConfig {
        dev: true,
        ..RuntimeConfig::default()
    };
    build_app(AppState::for_test(Arc::new(InMemoryProvider::new()), runtime))
}

/// Application avec une politique d'agent fixe.
pub(crate) fn app_with_fixed_agent(agent_id: &str) -> Router {
    let runtime = RuntimeConfig {
        api_key: Some(ApiKey::new(KEY)),
        agent_policy: AgentPolicy::Fixed(agent_id.to_string()),
        ..RuntimeConfig::default()
    };
    build_app(AppState::for_test(Arc::new(InMemoryProvider::new()), runtime))
}

/// Application avec un plafond de réponse resserré (tronque vite).
pub(crate) fn app_with_max_result_bytes(max_result_bytes: usize) -> Router {
    let runtime = RuntimeConfig {
        api_key: Some(ApiKey::new(KEY)),
        max_result_bytes,
        ..RuntimeConfig::default()
    };
    build_app(AppState::for_test(Arc::new(InMemoryProvider::new()), runtime))
}

/// Application avec un plafond de corps de requête resserré.
pub(crate) fn app_with_max_body_bytes(max_body_bytes: usize) -> Router {
    let runtime = RuntimeConfig {
        api_key: Some(ApiKey::new(KEY)),
        max_body_bytes,
        ..RuntimeConfig::default()
    };
    build_app(AppState::for_test(Arc::new(InMemoryProvider::new()), runtime))
}

/// Application avec un rate-limit `remember` resserré.
pub(crate) fn app_with_remember_rate_limit(limit: usize, window: Duration) -> Router {
    let runtime = RuntimeConfig {
        api_key: Some(ApiKey::new(KEY)),
        ..RuntimeConfig::default()
    };
    build_app(AppState::for_test_with_rate_limit(
        Arc::new(InMemoryProvider::new()),
        runtime,
        limit,
        window,
    ))
}
