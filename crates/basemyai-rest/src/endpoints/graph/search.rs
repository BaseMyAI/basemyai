// SPDX-License-Identifier: BUSL-1.1
//! `POST /graph/search` : recall vectoriel limité aux souvenirs mentionnant
//! une entité du graphe (`basemyai::Memory::search_graph`).

use axum::Json;
use axum::extract::State;
use axum::response::IntoResponse;
use serde::Deserialize;

use crate::context::{AppState, RequestContext};
use crate::endpoints::memories::{MemoryDto, RecallResponse};
use crate::http::error::RestError;
use crate::http::extract::{JsonBody, validate_k, validate_query};
use crate::http::pagination::truncate_to_fit;

#[derive(Deserialize)]
pub(super) struct GraphSearchRequest {
    pub agent_id: String,
    pub query: String,
    #[serde(default = "default_k")]
    pub k: usize,
}

fn default_k() -> usize {
    10
}

pub(super) async fn search(
    State(state): State<AppState>,
    JsonBody(req): JsonBody<GraphSearchRequest>,
) -> Result<impl IntoResponse, RestError> {
    validate_query(&req.query)?;
    validate_k(req.k)?;
    let mem = RequestContext::require_agent(&state, &req.agent_id).await?;
    let records = mem.search_graph(&req.query, req.k).await?;
    let items: Vec<MemoryDto> = records.into_iter().map(MemoryDto::from_vector).collect();
    let (results, truncated) = truncate_to_fit(items, state.runtime().max_result_bytes);
    Ok(Json(RecallResponse { results, truncated }))
}
