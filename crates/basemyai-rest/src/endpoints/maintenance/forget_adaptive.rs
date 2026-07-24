// SPDX-License-Identifier: BUSL-1.1
//! `POST /maintenance/forget_adaptive` : oubli adaptatif (ADR-037) — évince
//! physiquement les souvenirs actifs les moins bien notés au-delà d'une
//! capacité par agent.

use axum::Json;
use axum::extract::State;
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};

use basemyai::maintenance::AdaptiveForgettingPolicy;

use crate::context::{AppState, RequestContext};
use crate::http::error::RestError;
use crate::http::extract::JsonBody;

#[derive(Deserialize)]
pub(super) struct ForgetAdaptiveRequest {
    pub agent_id: String,
    pub capacity: usize,
    #[serde(default = "default_half_life_secs")]
    pub recency_half_life_secs: i64,
}

fn default_half_life_secs() -> i64 {
    86_400
}

#[derive(Serialize)]
pub(super) struct ForgetAdaptiveResponse {
    pub scanned: usize,
    pub evicted: usize,
}

pub(super) async fn forget_adaptive(
    State(state): State<AppState>,
    JsonBody(req): JsonBody<ForgetAdaptiveRequest>,
) -> Result<impl IntoResponse, RestError> {
    let mem = RequestContext::require_agent(&state, &req.agent_id).await?;
    let policy = AdaptiveForgettingPolicy {
        capacity: req.capacity,
        recency_half_life_secs: req.recency_half_life_secs,
    };
    let report = mem.adaptive_forget(policy).await?;
    Ok(Json(ForgetAdaptiveResponse {
        scanned: report.scanned,
        evicted: report.evicted,
    }))
}
