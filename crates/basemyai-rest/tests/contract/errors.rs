//! Forme stable du modèle d'erreur (`{"error":{"code","message"}}`) et des
//! codes HTTP associés — indépendante de l'endpoint qui la déclenche.

use axum::http::StatusCode;
use serde_json::json;
use tower::ServiceExt;

use crate::support::app::app;
use crate::support::client::{get, json_body, post};

#[tokio::test]
async fn missing_bearer_is_unauthorized() {
    let req = post("/v1/remember", &json!({"agent_id": "a", "text": "x"}), false);
    let resp = app().oneshot(req).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let body = json_body(resp).await;
    assert_eq!(body["error"]["code"], "unauthorized");
}

#[tokio::test]
async fn unknown_layer_is_bad_request_with_stable_code() {
    let req = post(
        "/v1/remember",
        &json!({"agent_id": "a", "text": "x", "layer": "bogus"}),
        true,
    );
    let resp = app().oneshot(req).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = json_body(resp).await;
    assert_eq!(body["error"]["code"], "invalid_layer");
}

#[tokio::test]
async fn validation_error_has_stable_shape() {
    let req = post("/v1/recall", &json!({"agent_id": "", "query": "q"}), true);
    let resp = app().oneshot(req).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = json_body(resp).await;
    assert_eq!(body["error"]["code"], "invalid_agent_id");
    assert!(body["error"]["message"].is_string());
}

/// Toute erreur (y compris celles produites par `tower-http`, pas seulement
/// `RestError`) porte le même `request_id` que le header de réponse.
#[tokio::test]
async fn error_body_carries_the_same_request_id_as_the_header() {
    let req = post("/v1/remember", &json!({"agent_id": "", "text": "x"}), true);
    let resp = app().oneshot(req).await.expect("oneshot");
    let header_id = resp
        .headers()
        .get("x-request-id")
        .expect("request id header")
        .to_str()
        .expect("ascii")
        .to_string();
    let body = json_body(resp).await;
    assert_eq!(body["error"]["request_id"], header_id);
}

#[tokio::test]
async fn not_found_route_is_a_plain_404() {
    let resp = app().oneshot(get("/v1/does-not-exist", true)).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
