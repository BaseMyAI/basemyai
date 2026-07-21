// SPDX-License-Identifier: BUSL-1.1
//! Domaine `health` : liveness/readiness, sans auth. Monté à la racine
//! (`/health/*`), pas sous `/v1` — un load balancer ne doit pas avoir besoin
//! de connaître la version d'API pour sonder le processus.

mod live;
mod ready;

use axum::Router;
use axum::routing::get;

use crate::context::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/health/live", get(live::live))
        .route("/health/ready", get(ready::ready))
}

/// Alias de compatibilité : `GET /v1/health` existait avant cette
/// restructuration et reste servi, avec la même forme que `/health/live`.
pub fn v1_alias_router() -> Router<AppState> {
    Router::new().route("/health", get(live::live))
}
