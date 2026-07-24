// SPDX-License-Identifier: BUSL-1.1
//! `GET /health/live` : le processus tourne et répond. Ne vérifie aucune
//! dépendance — c'est le rôle de `ready`.

use axum::Json;
use axum::response::IntoResponse;
use serde::Serialize;

#[derive(Serialize)]
pub(super) struct LiveResponse {
    pub status: &'static str,
    pub version: &'static str,
}

pub(super) async fn live() -> impl IntoResponse {
    Json(LiveResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}
