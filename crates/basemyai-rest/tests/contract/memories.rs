//! Bornes de validation et formes de réponse du domaine `memories`.

use axum::http::StatusCode;
use serde_json::json;
use tower::ServiceExt;

use crate::support::app::app;
use crate::support::client::{json_body, post};
use crate::support::fixtures::{overlong_agent_id, overlong_text};

#[tokio::test]
async fn recall_rejects_k_out_of_bounds() {
    let req = post("/v1/recall", &json!({"agent_id": "a", "query": "q", "k": 0}), true);
    let resp = app().oneshot(req).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = json_body(resp).await;
    assert!(body["error"]["message"].as_str().expect("message").contains('k'));
}

#[tokio::test]
async fn recall_hybrid_rejects_layer_filter() {
    let req = post(
        "/v1/recall_hybrid",
        &json!({"agent_id": "a", "query": "q", "layer": "semantic"}),
        true,
    );
    let resp = app().oneshot(req).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn remember_rejects_text_too_long() {
    let req = post("/v1/remember", &json!({"agent_id": "a", "text": overlong_text()}), true);
    let resp = app().oneshot(req).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn remember_rejects_agent_id_too_long() {
    let req = post(
        "/v1/remember",
        &json!({"agent_id": overlong_agent_id(), "text": "x"}),
        true,
    );
    let resp = app().oneshot(req).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn remember_batch_rejects_empty_batch() {
    let req = post("/v1/remember_batch", &json!({"agent_id": "a", "texts": []}), true);
    let resp = app().oneshot(req).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn valid_recall_request_still_passes_validation() {
    let req = post("/v1/recall", &json!({"agent_id": "a", "query": "q", "k": 5}), true);
    let resp = app().oneshot(req).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["results"], json!([]));
    assert_eq!(body["truncated"], false);
}
