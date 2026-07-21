//! `/health/live`, `/health/ready`, et l'alias de compatibilité `/v1/health`.

use axum::http::StatusCode;
use tower::ServiceExt;

use crate::support::app::app;
use crate::support::client::{get, json_body};

#[tokio::test]
async fn health_live_needs_no_auth_and_sets_headers() {
    let resp = app().oneshot(get("/health/live", false)).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers().contains_key("x-basemyai-version"));
    assert!(resp.headers().contains_key("x-request-id"));
    let body = json_body(resp).await;
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn health_ready_reports_provider_ready() {
    let resp = app().oneshot(get("/health/ready", false)).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["status"], "ok");
    assert_eq!(body["provider_ready"], true);
}

/// `/v1/health` existait avant la restructuration en tranches verticales ;
/// il reste servi avec la même forme que `/health/live`.
#[tokio::test]
async fn v1_health_alias_still_works() {
    let resp = app().oneshot(get("/v1/health", false)).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["status"], "ok");
}
