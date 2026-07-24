// SPDX-License-Identifier: BUSL-1.1
//! `GET /agent/{agent_id}/stats` : compte des souvenirs valides par couche.

use axum::Json;
use axum::extract::{Path, State};
use axum::response::IntoResponse;
use serde::Serialize;

use crate::context::{AppState, RequestContext};
use crate::http::error::RestError;

#[derive(Serialize)]
pub(super) struct StatsResponse {
    pub short_term: usize,
    pub episodic: usize,
    pub procedural: usize,
    pub semantic: usize,
    pub total: usize,
}

pub(super) async fn stats(
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
) -> Result<impl IntoResponse, RestError> {
    let mem = RequestContext::require_agent(&state, &agent_id).await?;
    let s = mem.stats().await?;
    Ok(Json(StatsResponse {
        short_term: s.short_term,
        episodic: s.episodic,
        procedural: s.procedural,
        semantic: s.semantic,
        total: s.total(),
    }))
}
