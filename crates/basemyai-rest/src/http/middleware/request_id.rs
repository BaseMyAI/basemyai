// SPDX-License-Identifier: BUSL-1.1
//! `X-Request-Id` : posé par `tower-http` (généré si absent côté client),
//! propagé sur la réponse, et injecté dans le corps JSON des réponses
//! d'erreur pour que logs et client partagent le même identifiant.

use axum::extract::Request;
use axum::http::HeaderName;
use axum::middleware::Next;
use axum::response::Response;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, RequestId, SetRequestIdLayer};

use crate::http::error::inject_request_id;

pub const HEADER: &str = "x-request-id";

/// Pose un `X-Request-Id` (UUID v4) si absent de la requête entrante.
#[must_use]
pub fn set_layer() -> SetRequestIdLayer<MakeRequestUuid> {
    SetRequestIdLayer::new(HeaderName::from_static(HEADER), MakeRequestUuid)
}

/// Propage le `X-Request-Id` de la requête vers la réponse.
#[must_use]
pub fn propagate_layer() -> PropagateRequestIdLayer {
    PropagateRequestIdLayer::new(HeaderName::from_static(HEADER))
}

/// Middleware `from_fn` : après le handler, si la réponse est une erreur JSON
/// (`{"error": {...}}`), y insère `request_id` — y compris pour les erreurs
/// produites par `tower-http` lui-même (timeout, body trop gros), qui ne
/// passent jamais par [`crate::http::error::RestError`].
pub async fn inject(req: Request, next: Next) -> Response {
    let request_id = req
        .extensions()
        .get::<RequestId>()
        .and_then(|id| id.header_value().to_str().ok())
        .map(str::to_string);

    let response = next.run(req).await;
    let Some(request_id) = request_id else {
        return response;
    };
    if !(response.status().is_client_error() || response.status().is_server_error()) {
        return response;
    }

    let (parts, body) = response.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, 1 << 20).await else {
        return Response::from_parts(parts, axum::body::Body::empty());
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Response::from_parts(parts, axum::body::Body::from(bytes));
    };
    let value = inject_request_id(value, &request_id);
    let Ok(rewritten) = serde_json::to_vec(&value) else {
        return Response::from_parts(parts, axum::body::Body::from(bytes));
    };
    Response::from_parts(parts, axum::body::Body::from(rewritten))
}
