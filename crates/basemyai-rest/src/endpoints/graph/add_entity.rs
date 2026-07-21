// SPDX-License-Identifier: BUSL-1.1
//! `POST /graph/entities` : insère ou met à jour une entité du graphe pour un agent.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;

use crate::context::{AppState, RequestContext};
use crate::http::error::RestError;
use crate::http::extract::{JsonBody, validate_non_empty};

#[derive(Deserialize)]
pub(super) struct AddEntityRequest {
    pub agent_id: String,
    pub id: String,
    pub kind: String,
    pub label: String,
}

pub(super) async fn add_entity(
    State(state): State<AppState>,
    JsonBody(req): JsonBody<AddEntityRequest>,
) -> Result<impl IntoResponse, RestError> {
    validate_non_empty("id", &req.id)?;
    validate_non_empty("kind", &req.kind)?;
    let mem = RequestContext::require_agent(&state, &req.agent_id).await?;
    mem.graph().add_entity(&req.id, &req.kind, &req.label).await?;
    Ok(StatusCode::CREATED)
}
