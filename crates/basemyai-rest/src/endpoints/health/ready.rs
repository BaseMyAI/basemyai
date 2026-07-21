// SPDX-License-Identifier: BUSL-1.1
//! `GET /health/ready` : les dépendances nécessaires sont prêtes.
//!
//! Ne fait volontairement **aucune** I/O par appel — le provider (store +
//! embedder) est construit une fois avant que le serveur ne commence à
//! écouter (`server::bootstrap::run`) ; s'il avait échoué, le processus ne
//! serait jamais arrivé jusqu'ici. La readiness rapporte donc un fait déjà
//! établi, pas une sonde active (§14 : jamais une opération lourde par appel).

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Serialize;

use crate::context::AppState;

#[derive(Serialize)]
pub(super) struct ReadyResponse {
    pub status: &'static str,
    pub provider_ready: bool,
}

pub(super) async fn ready(State(state): State<AppState>) -> impl IntoResponse {
    let provider_ready = state.memories().is_provider_reachable().await;
    let status_code = if provider_ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status_code,
        Json(ReadyResponse {
            status: if provider_ready { "ok" } else { "not_ready" },
            provider_ready,
        }),
    )
}
