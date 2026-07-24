// SPDX-License-Identifier: BUSL-1.1
//! Auth Bearer en temps constant. Toutes les routes `/v1/*` sauf
//! `/v1/health/*` passent par ce middleware (assemblé dans `server::router`).

use axum::extract::{Request, State};
use axum::http::HeaderMap;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use subtle::ConstantTimeEq;

use crate::context::AppState;
use crate::http::error::RestError;

/// Exige un Bearer valide, sauf en mode `dev` (refusé au démarrage hors
/// boucle locale — voir `config::validation`).
pub async fn require_auth(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if state.runtime().dev {
        return next.run(req).await;
    }
    match state.runtime().api_key.as_ref() {
        Some(key) if bearer_ok(req.headers(), key.expose()) => next.run(req).await,
        _ => RestError::Unauthorized.into_response(),
    }
}

fn bearer_ok(headers: &HeaderMap, api_key: &str) -> bool {
    let Some(value) = headers.get(axum::http::header::AUTHORIZATION) else {
        return false;
    };
    let Ok(text) = value.to_str() else {
        return false;
    };
    let Some(token) = text.strip_prefix("Bearer ") else {
        return false;
    };
    token.as_bytes().ct_eq(api_key.as_bytes()).into()
}
