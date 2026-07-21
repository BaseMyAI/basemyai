// SPDX-License-Identifier: BUSL-1.1
//! `POST /agent/{agent_id}/export` : export JSONL versionné de la mémoire
//! d'un agent (backup/migration, `basemyai::Memory::export_jsonl`).

use axum::extract::{Path, State};
use axum::response::IntoResponse;

use crate::context::{AppState, RequestContext};
use crate::http::error::RestError;

pub(super) async fn export(
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
) -> Result<impl IntoResponse, RestError> {
    let mem = RequestContext::require_agent(&state, &agent_id).await?;
    let jsonl = mem.export_jsonl().await?;
    Ok(([("content-type", "application/x-ndjson")], jsonl))
}
