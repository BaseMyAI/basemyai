// SPDX-License-Identifier: BUSL-1.1
//! `DELETE /agent/{agent_id}` : purge **toutes** les données de l'agent
//! (mémoire + graphe). Irréversible — exige `confirm=<agent_id>` exact pour
//! éviter une suppression accidentelle par simple faute de frappe d'URL.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;

use crate::context::AppState;
use crate::http::error::RestError;
use crate::http::extract::validate_agent_id;

#[derive(Deserialize)]
pub(super) struct DeleteAgentQuery {
    #[serde(default)]
    pub confirm: Option<String>,
}

pub(super) async fn purge(
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
    Query(q): Query<DeleteAgentQuery>,
) -> Result<impl IntoResponse, RestError> {
    validate_agent_id(&agent_id)?;
    if q.confirm.as_deref() != Some(agent_id.as_str()) {
        return Err(RestError::Validation(
            "confirm must exactly match agent_id for destructive agent deletion".to_string(),
        ));
    }
    let mem = state.memories().resolve(&agent_id).await?;
    mem.purge_agent().await?;
    Ok(StatusCode::NO_CONTENT)
}
