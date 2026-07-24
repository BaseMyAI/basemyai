// SPDX-License-Identifier: BUSL-1.1
//! Domaine `events` : abonnement SSE aux événements mémoire d'un agent.

mod subscribe;

use axum::Router;
use axum::routing::get;

use crate::context::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/watch", get(subscribe::subscribe))
        // Alias : même handler, nom de route aligné sur le domaine `events`.
        .route("/events", get(subscribe::subscribe))
}
