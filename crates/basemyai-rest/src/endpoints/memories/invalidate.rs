// SPDX-License-Identifier: BUSL-1.1
//! `POST /memories/{id}/invalidate` : invalide (soft-delete, `valid_until = now`)
//! un souvenir — il cesse d'apparaître dans les recalls futurs mais reste
//! physiquement présent (contrairement à `forget`).

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;

use crate::context::{AppState, RequestContext};
use crate::http::error::RestError;

#[derive(Deserialize)]
pub(super) struct AgentQuery {
    pub agent_id: String,
}

pub(super) async fn invalidate(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<AgentQuery>,
) -> Result<impl IntoResponse, RestError> {
    let mem = RequestContext::require_agent(&state, &q.agent_id).await?;
    mem.invalidate(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}
