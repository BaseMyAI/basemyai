// SPDX-License-Identifier: BUSL-1.1
//! `POST /recall`, `POST /recall_hybrid` : recherche sémantique et hybride
//! (vecteur + BM25 fusionnés par RRF, ADR-014).

use axum::Json;
use axum::extract::State;
use axum::response::IntoResponse;
use serde::Deserialize;

use basemyai::MemoryLayer;

use super::contract::{MemoryDto, RecallResponse};
use crate::context::{AppState, RequestContext};
use crate::http::error::RestError;
use crate::http::extract::{JsonBody, validate_k, validate_query};
use crate::http::pagination::truncate_to_fit;

#[derive(Deserialize)]
pub(super) struct RecallRequest {
    pub agent_id: String,
    pub query: String,
    #[serde(default = "default_k")]
    pub k: usize,
    #[serde(default)]
    pub layer: Option<String>,
    /// Inclure la couche `procedural` (défaut : `false`, audit memory poisoning).
    #[serde(default)]
    pub include_procedural: bool,
    /// Exclure les souvenirs importés (défaut : `false`, ADR-036).
    #[serde(default)]
    pub exclude_imported: bool,
}

fn default_k() -> usize {
    10
}

pub(super) async fn recall(
    State(state): State<AppState>,
    JsonBody(req): JsonBody<RecallRequest>,
) -> Result<impl IntoResponse, RestError> {
    validate_query(&req.query)?;
    validate_k(req.k)?;
    let mem = RequestContext::require_agent(&state, &req.agent_id).await?;
    let records = match req.layer.as_deref() {
        Some(layer) => {
            let layer = MemoryLayer::from_table(layer)?;
            mem.recall_by_layer(&req.query, layer, req.k).await?
        }
        None => {
            mem.recall_with_options(
                &req.query,
                req.k,
                basemyai::RecallOptions {
                    include_procedural: req.include_procedural,
                    exclude_imported: req.exclude_imported,
                },
            )
            .await?
        }
    };
    let items: Vec<MemoryDto> = records.into_iter().map(MemoryDto::from_vector).collect();
    let (results, truncated) = truncate_to_fit(items, state.runtime().max_result_bytes);
    Ok(Json(RecallResponse { results, truncated }))
}

pub(super) async fn recall_hybrid(
    State(state): State<AppState>,
    JsonBody(req): JsonBody<RecallRequest>,
) -> Result<impl IntoResponse, RestError> {
    validate_query(&req.query)?;
    validate_k(req.k)?;
    if req.layer.is_some() {
        return Err(RestError::Validation(
            "layer is not supported by /recall_hybrid; use /recall for layer-filtered recall".to_string(),
        ));
    }
    let mem = RequestContext::require_agent(&state, &req.agent_id).await?;
    let records = mem
        .recall_hybrid_with_options(
            &req.query,
            req.k,
            basemyai::RecallOptions {
                include_procedural: req.include_procedural,
                exclude_imported: req.exclude_imported,
            },
        )
        .await?;
    let items: Vec<MemoryDto> = records.into_iter().map(MemoryDto::from_hybrid).collect();
    let (results, truncated) = truncate_to_fit(items, state.runtime().max_result_bytes);
    Ok(Json(RecallResponse { results, truncated }))
}
