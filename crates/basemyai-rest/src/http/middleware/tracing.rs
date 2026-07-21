// SPDX-License-Identifier: BUSL-1.1
//! Télémétrie par requête : méthode, route, statut, durée, `request_id`.
//! Jamais le corps de la requête/réponse (souvenirs, clés) — voir
//! `server::telemetry` pour l'init globale du subscriber.

use std::time::Instant;

use axum::extract::{MatchedPath, Request};
use axum::middleware::Next;
use axum::response::Response;
use tower_http::request_id::RequestId;

/// Logue une ligne par requête : `method`, `route` (pattern matché, jamais
/// l'URL brute qui pourrait porter un `agent_id`/id sensible dans le path),
/// `status`, `duration_ms`, `request_id`. Aucun header, aucun corps.
pub async fn log(req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| "unmatched".to_string());
    let request_id = req
        .extensions()
        .get::<RequestId>()
        .and_then(|id| id.header_value().to_str().ok())
        .map(str::to_string)
        .unwrap_or_default();

    let start = Instant::now();
    let response = next.run(req).await;
    let elapsed = start.elapsed();

    let status = response.status().as_u16();
    if response.status().is_server_error() {
        tracing::error!(%method, %route, status, duration_ms = elapsed.as_millis() as u64, request_id, "request failed");
    } else {
        tracing::info!(%method, %route, status, duration_ms = elapsed.as_millis() as u64, request_id, "request");
    }
    response
}
