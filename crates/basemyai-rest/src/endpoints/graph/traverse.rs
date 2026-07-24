// SPDX-License-Identifier: BUSL-1.1
//! `POST /recall_graph` : traversée BFS du graphe d'entités d'un agent depuis
//! une entité de départ.

use axum::Json;
use axum::extract::State;
use axum::response::IntoResponse;
use serde::Deserialize;

use super::contract::{EntityDto, GraphResponse};
use crate::context::{AppState, RequestContext};
use crate::http::error::RestError;
use crate::http::extract::{JsonBody, validate_graph_depth, validate_non_empty};
use crate::http::pagination::truncate_to_fit;

#[derive(Deserialize)]
pub(super) struct TraverseRequest {
    pub agent_id: String,
    pub start: String,
    #[serde(default = "default_depth")]
    pub max_depth: u32,
}

fn default_depth() -> u32 {
    3
}

pub(super) async fn traverse(
    State(state): State<AppState>,
    JsonBody(req): JsonBody<TraverseRequest>,
) -> Result<impl IntoResponse, RestError> {
    validate_non_empty("start", &req.start)?;
    validate_graph_depth(req.max_depth)?;
    let mem = RequestContext::require_agent(&state, &req.agent_id).await?;
    let reached = mem.graph().traverse(&req.start, req.max_depth).await?;
    let nodes: Vec<EntityDto> = reached.into_iter().map(EntityDto::from).collect();
    let (nodes, truncated) = truncate_to_fit(nodes, state.runtime().max_result_bytes);
    Ok(Json(GraphResponse { nodes, truncated }))
}
