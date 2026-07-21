//! `remember_batch` est tout-ou-rien : si un texte du lot est invalide, rien
//! n'est inséré (pas d'insertion partielle observable via `stats`/`recall`).

use axum::http::StatusCode;
use serde_json::json;
use tower::ServiceExt;

use crate::support::app::app;
use crate::support::client::{get, json_body, post};
use crate::support::fixtures::overlong_text;

#[tokio::test]
async fn a_batch_with_one_oversized_text_inserts_nothing() {
    let app = app();
    let resp = app
        .clone()
        .oneshot(post(
            "/v1/remember_batch",
            &json!({"agent_id": "a", "texts": ["fine", "also fine", overlong_text()]}),
            true,
        ))
        .await
        .expect("remember_batch");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let resp = app.oneshot(get("/v1/agent/a/stats", true)).await.expect("stats");
    let body = json_body(resp).await;
    assert_eq!(body["total"], 0, "no partial insert from a rejected batch");
}

#[tokio::test]
async fn a_valid_batch_inserts_exactly_its_size() {
    let app = app();
    app.clone()
        .oneshot(post(
            "/v1/remember_batch",
            &json!({"agent_id": "a", "texts": ["one", "two", "three", "four"]}),
            true,
        ))
        .await
        .expect("remember_batch");

    let resp = app.oneshot(get("/v1/agent/a/stats", true)).await.expect("stats");
    let body = json_body(resp).await;
    assert_eq!(body["total"], 4);
}
