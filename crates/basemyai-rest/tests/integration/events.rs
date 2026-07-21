//! `GET /watch` (SSE) : relais des événements mémoire, isolé par agent.

use std::time::Duration;

use axum::http::StatusCode;
use futures_util::StreamExt;
use serde_json::json;
use tower::ServiceExt;

use crate::support::app::app;
use crate::support::client::{get, post};

/// Lit le flux SSE jusqu'à ce que `predicate` matche le texte accumulé, ou
/// jusqu'au timeout — évite un test qui bloque indéfiniment si l'événement
/// attendu n'arrive jamais.
async fn read_sse_until(body: axum::body::Body, predicate: impl Fn(&str) -> bool, timeout: Duration) -> String {
    let mut stream = body.into_data_stream();
    let mut acc = String::new();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            panic!("SSE predicate never matched within timeout; accumulated: {acc}");
        }
        match tokio::time::timeout(remaining, stream.next()).await {
            Ok(Some(Ok(chunk))) => {
                acc.push_str(&String::from_utf8_lossy(&chunk));
                if predicate(&acc) {
                    return acc;
                }
            }
            Ok(Some(Err(e))) => panic!("SSE stream error: {e}"),
            Ok(None) => panic!("SSE stream ended before predicate matched; accumulated: {acc}"),
            Err(_) => panic!("timed out waiting for SSE predicate; accumulated: {acc}"),
        }
    }
}

#[tokio::test]
async fn watch_delivers_remembered_event_for_same_agent() {
    let app = app();
    let resp = app
        .clone()
        .oneshot(get("/v1/watch?agent_id=a", true))
        .await
        .expect("watch");
    assert_eq!(resp.status(), StatusCode::OK);

    let watch_body = resp.into_body();
    let remember = tokio::spawn(app.oneshot(post(
        "/v1/remember",
        &json!({"agent_id": "a", "text": "watched fact"}),
        true,
    )));

    let acc = read_sse_until(
        watch_body,
        |s| s.contains("\"kind\":\"remembered\""),
        Duration::from_secs(5),
    )
    .await;
    assert!(acc.contains("\"agent_id\":\"a\""));
    remember.await.expect("join").expect("remember request");
}

#[tokio::test]
async fn watch_isolates_events_from_other_agents() {
    let app = app();
    let resp = app
        .clone()
        .oneshot(get("/v1/watch?agent_id=a", true))
        .await
        .expect("watch a");
    let watch_body = resp.into_body();

    app.clone()
        .oneshot(post(
            "/v1/remember",
            &json!({"agent_id": "b", "text": "not for a"}),
            true,
        ))
        .await
        .expect("remember for b");
    app.oneshot(post("/v1/remember", &json!({"agent_id": "a", "text": "for a"}), true))
        .await
        .expect("remember for a");

    let acc = read_sse_until(
        watch_body,
        |s| s.contains("\"kind\":\"remembered\""),
        Duration::from_secs(5),
    )
    .await;
    assert!(acc.contains("\"agent_id\":\"a\""));
    assert!(!acc.contains("not for a"));
}
