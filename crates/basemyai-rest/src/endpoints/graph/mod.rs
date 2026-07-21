// SPDX-License-Identifier: BUSL-1.1
//! Domaine `graph` : entités/relations et leur traversée, pour un agent.

mod add_entity;
mod add_relation;
pub(crate) mod contract;
mod search;
mod traverse;

use axum::Router;
use axum::routing::post;

use crate::context::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/recall_graph", post(traverse::traverse))
        .route("/graph/entities", post(add_entity::add_entity))
        .route("/graph/relations", post(add_relation::add_relation))
        .route("/graph/search", post(search::search))
}
