// SPDX-License-Identifier: BUSL-1.1
//! Domaine `maintenance` : passes ponctuelles de GC temporel et d'oubli
//! adaptatif, déclenchables via REST (la même politique tourne aussi en
//! tâche de fond via `basemyai::maintenance::MaintenanceWorker` côté
//! bindings/surfaces qui en font tourner un — ce domaine expose le
//! déclenchement manuel, pas une seconde implémentation).

mod collect_expired;
mod forget_adaptive;

use axum::Router;
use axum::routing::post;

use crate::context::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/maintenance/collect_expired", post(collect_expired::collect_expired))
        .route("/maintenance/forget_adaptive", post(forget_adaptive::forget_adaptive))
}
