//! Invariants d'isolation temporelle : `valid_until` doit être postérieur à
//! `valid_from` (rejeté sinon, §12), et un souvenir dont la fenêtre est
//! toujours ouverte reste rappelable.

use axum::http::StatusCode;
use serde_json::json;
use tower::ServiceExt;

use crate::support::app::app;
use crate::support::client::{json_body, post};

fn now_unix() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).expect("clock").as_secs() as i64
}

#[tokio::test]
async fn remember_rejects_valid_until_not_after_valid_from() {
    let app = app();
    let past = now_unix() - 3600;
    let resp = app
        .oneshot(post(
            "/v1/remember",
            &json!({"agent_id": "a", "text": "already expired fact", "valid_until": past}),
            true,
        ))
        .await
        .expect("remember");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = json_body(resp).await;
    assert_eq!(body["error"]["code"], "invalid_request");
}

#[tokio::test]
async fn future_valid_until_still_recalls_today() {
    let app = app();
    let future = now_unix() + 3600;
    let resp = app
        .clone()
        .oneshot(post(
            "/v1/remember",
            &json!({"agent_id": "a", "text": "still valid fact", "valid_until": future}),
            true,
        ))
        .await
        .expect("remember");
    assert_eq!(resp.status(), StatusCode::CREATED);

    let resp = app
        .oneshot(post(
            "/v1/recall",
            &json!({"agent_id": "a", "query": "still valid fact"}),
            true,
        ))
        .await
        .expect("recall");
    let body = json_body(resp).await;
    assert!(!body["results"].as_array().expect("results").is_empty());
}

/// Une invalidation explicite (`POST .../invalidate`) doit avoir le même
/// effet observable qu'une expiration temporelle : disparaître du recall
/// tout en restant un no-op idempotent si rejouée.
#[tokio::test]
async fn invalidate_is_idempotent() {
    let app = app();
    let resp = app
        .clone()
        .oneshot(post(
            "/v1/remember",
            &json!({"agent_id": "a", "text": "to be invalidated twice"}),
            true,
        ))
        .await
        .expect("remember");
    let id = json_body(resp).await["id"].as_str().expect("id").to_string();

    for _ in 0..2 {
        let resp = app
            .clone()
            .oneshot(post(
                &format!("/v1/memories/{id}/invalidate?agent_id=a"),
                &json!({}),
                true,
            ))
            .await
            .expect("invalidate");
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }
}
