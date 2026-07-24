// SPDX-License-Identifier: BUSL-1.1
//! `POST /graph/relations` : crée ou met à jour une relation orientée entre
//! deux entités du graphe pour un agent.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;

use crate::context::{AppState, RequestContext};
use crate::http::error::RestError;
use crate::http::extract::{JsonBody, validate_non_empty};

#[derive(Deserialize)]
pub(super) struct AddRelationRequest {
    pub agent_id: String,
    pub src: String,
    pub relation: String,
    pub dst: String,
    #[serde(default = "default_weight")]
    pub weight: f64,
}

fn default_weight() -> f64 {
    1.0
}

pub(super) async fn add_relation(
    State(state): State<AppState>,
    JsonBody(req): JsonBody<AddRelationRequest>,
) -> Result<impl IntoResponse, RestError> {
    validate_non_empty("src", &req.src)?;
    validate_non_empty("relation", &req.relation)?;
    validate_non_empty("dst", &req.dst)?;
    if !req.weight.is_finite() {
        return Err(RestError::Validation(format!(
            "weight must be a finite number, got {}",
            req.weight
        )));
    }
    let mem = RequestContext::require_agent(&state, &req.agent_id).await?;
    mem.graph()
        .add_edge(&req.src, &req.relation, &req.dst, req.weight)
        .await?;
    Ok(StatusCode::CREATED)
}
