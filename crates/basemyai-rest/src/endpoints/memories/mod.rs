// SPDX-License-Identifier: BUSL-1.1
//! Domaine `memories` : cycle de vie d'un souvenir individuel — création,
//! recherche, invalidation, suppression. La purge (tout l'agent) vit dans
//! `endpoints::agents`, qui est une opération d'un ordre de grandeur différent.

mod contract;
mod forget;
mod invalidate;
mod recall;
mod remember;
mod remember_batch;

use axum::Router;
use axum::routing::{delete, post};

use crate::context::AppState;

/// Réexporté pour `endpoints::graph::search`, qui renvoie la même forme de
/// résultat qu'un recall classique (un souvenir mentionnant une entité).
pub(crate) use contract::{MemoryDto, RecallResponse};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/remember", post(remember::remember))
        .route("/remember_batch", post(remember_batch::remember_batch))
        .route("/recall", post(recall::recall))
        .route("/recall_hybrid", post(recall::recall_hybrid))
        .route("/memories/{id}/invalidate", post(invalidate::invalidate))
        .route("/memories/{id}", delete(forget::forget))
}
