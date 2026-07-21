// SPDX-License-Identifier: BUSL-1.1
//! `POST /agent/{agent_id}/import` : réimporte un export JSONL
//! (`basemyai::Memory::import_jsonl_with_options`, ré-embedding, idempotent —
//! les lignes déjà présentes par id sont laissées intactes).

use axum::Json;
use axum::extract::{Path, State};
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};

use crate::context::{AppState, RequestContext};
use crate::http::error::RestError;
use crate::http::extract::{JsonBody, validate_import_size};

#[derive(Deserialize)]
pub(super) struct ImportRequest {
    pub jsonl: String,
    /// Autorise l'import de souvenirs `procedural` (audit memory poisoning,
    /// ADR-035) — refusés par défaut.
    #[serde(default)]
    pub trusted: bool,
}

#[derive(Serialize)]
pub(super) struct ImportResponse {
    pub memories: usize,
    pub memories_skipped: usize,
    pub entities: usize,
    pub entities_skipped: usize,
    pub edges: usize,
    pub edges_skipped: usize,
}

pub(super) async fn import(
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
    JsonBody(req): JsonBody<ImportRequest>,
) -> Result<impl IntoResponse, RestError> {
    validate_import_size(&req.jsonl)?;
    let mem = RequestContext::require_agent(&state, &agent_id).await?;
    let report = mem.import_jsonl_with_options(&req.jsonl, req.trusted).await?;
    Ok(Json(ImportResponse {
        memories: report.memories,
        memories_skipped: report.memories_skipped,
        entities: report.entities,
        entities_skipped: report.entities_skipped,
        edges: report.edges,
        edges_skipped: report.edges_skipped,
    }))
}
