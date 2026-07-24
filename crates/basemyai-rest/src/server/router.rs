// SPDX-License-Identifier: BUSL-1.1
//! Assemblage du routeur : chaque domaine (`endpoints::*`) fournit son
//! `router()`, merged ici sous `/v1` (sauf `health`, monté aussi à la racine),
//! avec l'auth, les plafonds et la télémétrie appliqués une seule fois.

use std::time::Duration;

use axum::Router;
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::middleware;
use tower::ServiceBuilder;
use tower_http::timeout::TimeoutLayer;

use crate::context::AppState;
use crate::endpoints::{agents, context, events, graph, health, maintenance, memories};
use crate::http::middleware::{auth, body_limit, request_id, tracing as http_tracing};

/// Construit l'application axum complète (middlewares + routes), sans jamais
/// ouvrir de socket réseau — testable directement via `tower::ServiceExt::oneshot`.
pub fn build(state: AppState) -> Router {
    let protected = Router::new()
        .merge(memories::router())
        .merge(graph::router())
        .merge(context::router())
        .merge(maintenance::router())
        .merge(agents::router())
        .merge(events::router())
        .route_layer(middleware::from_fn_with_state(state.clone(), auth::require_auth));

    // `/v1/health` : alias de compatibilité, sans auth (même que `/health/live`).
    let public_under_v1 = health::v1_alias_router();

    let timeout = Duration::from_secs(state.runtime().timeout_secs);
    let max_body = state.runtime().max_body_bytes;

    // `route_layer` s'applique *après* le matching de route (donc uniquement
    // aux requêtes qui touchent une route réelle, jamais un 404) — c'est ce
    // qui rend `MatchedPath` disponible à `http_tracing::log` pour loguer le
    // pattern de route plutôt que l'URL brute (qui porterait `agent_id`).
    let router = Router::new()
        .merge(health::router()) // `/health/live`, `/health/ready` — racine, sans `/v1`, sans auth.
        .nest("/v1", protected.merge(public_under_v1))
        .route_layer(middleware::from_fn(http_tracing::log));

    let mw = ServiceBuilder::new()
        .layer(request_id::set_layer())
        .layer(request_id::propagate_layer())
        .layer(middleware::from_fn(request_id::inject))
        .layer(tower_http::set_header::SetResponseHeaderLayer::overriding(
            HeaderName::from_static("x-basemyai-version"),
            HeaderValue::from_static(env!("CARGO_PKG_VERSION")),
        ))
        .layer(body_limit::layer(max_body))
        .layer(TimeoutLayer::with_status_code(StatusCode::GATEWAY_TIMEOUT, timeout));

    router.with_state(state).layer(mw)
}
