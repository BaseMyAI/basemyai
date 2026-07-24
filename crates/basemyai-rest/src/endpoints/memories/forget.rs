// SPDX-License-Identifier: BUSL-1.1
//! `DELETE /memories/{id}` : suppression physique d'un souvenir (droit à
//! l'effacement). Irréversible, contrairement à `invalidate`.

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

pub(super) async fn forget(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<AgentQuery>,
) -> Result<impl IntoResponse, RestError> {
    let mem = RequestContext::require_agent(&state, &q.agent_id).await?;
    mem.forget(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}
