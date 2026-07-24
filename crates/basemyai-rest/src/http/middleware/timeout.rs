// SPDX-License-Identifier: BUSL-1.1
//! Timeout global de requête (`RuntimeConfig::timeout_secs`). Un client SSE
//! (`GET /v1/events`) reste ouvert indéfiniment via `Sse::keep_alive`, qui
//! n'est pas concerné : le timeout borne la latence d'une requête normale,
//! pas la durée d'un flux déjà établi.

use std::time::Duration;

use axum::http::StatusCode;
use tower_http::timeout::TimeoutLayer;

#[must_use]
pub fn layer(timeout_secs: u64) -> TimeoutLayer {
    TimeoutLayer::with_status_code(StatusCode::GATEWAY_TIMEOUT, Duration::from_secs(timeout_secs))
}
