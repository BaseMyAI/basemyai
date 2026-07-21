// SPDX-License-Identifier: BUSL-1.1
//! `POST /remember` : mémorise un texte dans une couche pour un agent.

use axum::extract::State;
use basemyai::{MemoryLayer, Validity};
use serde::{Deserialize, Serialize};

use super::contract::now_unix;
use crate::context::{AppState, RequestContext};
use crate::http::error::RestError;
use crate::http::extract::{JsonBody, validate_text, validate_validity};
use crate::http::response::created;

#[derive(Deserialize)]
pub(super) struct RememberRequest {
    pub agent_id: String,
    pub text: String,
    #[serde(default = "default_layer")]
    pub layer: String,
    #[serde(default)]
    pub valid_until: Option<i64>,
}

fn default_layer() -> String {
    "semantic".to_string()
}

#[derive(Serialize)]
pub(super) struct IdResponse {
    pub id: String,
}

pub(super) async fn remember(
    State(state): State<AppState>,
    JsonBody(req): JsonBody<RememberRequest>,
) -> Result<impl axum::response::IntoResponse, RestError> {
    validate_text(&req.text)?;
    if !state.memories().check_remember_rate(&req.agent_id).await {
        return Err(RestError::RateLimited);
    }
    let mem = RequestContext::require_agent(&state, &req.agent_id).await?;
    let layer = MemoryLayer::from_table(&req.layer)?;
    let validity = Validity {
        valid_from: now_unix(),
        valid_until: req.valid_until,
    };
    validate_validity(&validity)?;
    let id = mem.remember_with(&req.text, layer, validity).await?;
    Ok(created(IdResponse { id }))
}
