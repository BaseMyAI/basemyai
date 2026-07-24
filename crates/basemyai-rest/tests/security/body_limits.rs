//! Limites de taille : corps de requête global (`RuntimeConfig::max_body_bytes`)
//! et réponses de recherche (`max_result_bytes`, best-effort troncature).

use axum::http::StatusCode;
use serde_json::json;
use tower::ServiceExt;

use crate::support::app::{app_with_max_body_bytes, app_with_max_result_bytes};
use crate::support::client::{json_body, post};

#[tokio::test]
async fn oversized_request_body_is_rejected_with_413() {
    let app = app_with_max_body_bytes(1024);
    let huge_text = "x".repeat(4096);
    let req = post("/v1/remember", &json!({"agent_id": "a", "text": huge_text}), true);
    let resp = app.oneshot(req).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn recall_response_is_truncated_under_a_tight_result_budget() {
    let app = app_with_max_result_bytes(64);
    for i in 0..20 {
        app.clone()
            .oneshot(post(
                "/v1/remember_batch",
                &json!({"agent_id": "a", "texts": [format!("fact number {i} with enough padding to matter")]}),
                true,
            ))
            .await
            .expect("seed");
    }

    let resp = app
        .oneshot(post(
            "/v1/recall",
            &json!({"agent_id": "a", "query": "fact", "k": 20}),
            true,
        ))
        .await
        .expect("recall");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["truncated"], true);
}
