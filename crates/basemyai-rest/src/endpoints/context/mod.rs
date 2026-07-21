// SPDX-License-Identifier: BUSL-1.1
//! Domaine `context` : Context Engine (`basemyai::context`).

mod compile_context;

use axum::Router;
use axum::routing::post;

use crate::context::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/compile_context", post(compile_context::compile_context))
}
