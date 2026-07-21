// SPDX-License-Identifier: BUSL-1.1
//! `POST /maintenance/collect_expired` : GC temporel (ADR-038) — supprime
//! physiquement, par pages bornées, les souvenirs d'un agent dont
//! `valid_until` est déjà passé.

use axum::Json;
use axum::extract::State;
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};

use crate::context::{AppState, RequestContext};
use crate::http::error::RestError;
use crate::http::extract::JsonBody;

#[derive(Deserialize)]
pub(super) struct CollectExpiredRequest {
    pub agent_id: String,
    #[serde(default = "default_page_size")]
    pub page_size: usize,
}

fn default_page_size() -> usize {
    basemyai::maintenance::DEFAULT_GC_PAGE_SIZE
}

#[derive(Serialize)]
pub(super) struct CollectExpiredResponse {
    pub examined: usize,
    pub deleted: usize,
    pub pages: usize,
}

pub(super) async fn collect_expired(
    State(state): State<AppState>,
    JsonBody(req): JsonBody<CollectExpiredRequest>,
) -> Result<impl IntoResponse, RestError> {
    let mem = RequestContext::require_agent(&state, &req.agent_id).await?;
    let report = mem.expired_gc(req.page_size).await?;
    Ok(Json(CollectExpiredResponse {
        examined: report.examined,
        deleted: report.deleted,
        pages: report.pages,
    }))
}
