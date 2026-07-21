// SPDX-License-Identifier: BUSL-1.1
//! Domaine `agents` : opérations à l'échelle d'un agent entier (stats,
//! export/import, purge) — un ordre de grandeur au-dessus de `memories`.

mod export;
mod import;
mod purge;
mod stats;

use axum::Router;
use axum::routing::{delete, get, post};

use crate::context::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/agent/{agent_id}", delete(purge::purge))
        .route("/agent/{agent_id}/stats", get(stats::stats))
        .route("/agent/{agent_id}/export", get(export::export))
        .route("/agent/{agent_id}/import", post(import::import))
}
