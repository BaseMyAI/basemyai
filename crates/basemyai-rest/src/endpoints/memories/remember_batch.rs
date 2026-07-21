// SPDX-License-Identifier: BUSL-1.1
//! `POST /remember_batch` : ingère un lot de textes en une passe d'embedding
//! et un seul batch atomique (`basemyai::Memory::remember_batch_with`).

use axum::extract::State;
use basemyai::{MemoryLayer, Validity};
use serde::{Deserialize, Serialize};

use super::contract::now_unix;
use crate::context::{AppState, RequestContext};
use crate::http::error::RestError;
use crate::http::extract::{JsonBody, validate_batch, validate_validity};
use crate::http::response::created;

#[derive(Deserialize)]
pub(super) struct RememberBatchRequest {
    pub agent_id: String,
    pub texts: Vec<String>,
    #[serde(default = "default_layer")]
    pub layer: String,
    #[serde(default)]
    pub valid_until: Option<i64>,
}

fn default_layer() -> String {
    "semantic".to_string()
}

#[derive(Serialize)]
pub(super) struct IdsResponse {
    pub ids: Vec<String>,
}

pub(super) async fn remember_batch(
    State(state): State<AppState>,
    JsonBody(req): JsonBody<RememberBatchRequest>,
) -> Result<impl axum::response::IntoResponse, RestError> {
    validate_batch(&req.texts)?;
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
    let ids = mem.remember_batch_with(&req.texts, layer, validity).await?;
    Ok(created(IdsResponse { ids }))
}
