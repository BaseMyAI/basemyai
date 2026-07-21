//! Isolation stricte entre agents : un souvenir/entité d'un agent n'est
//! jamais visible depuis un autre — recall, graphe et politique d'agent fixe.

use axum::http::StatusCode;
use serde_json::json;
use tower::ServiceExt;

use crate::support::app::{app, app_with_fixed_agent};
use crate::support::client::{json_body, post};

#[tokio::test]
async fn recall_isolation_between_agents() {
    let app = app();
    app.clone()
        .oneshot(post(
            "/v1/remember",
            &json!({"agent_id": "a", "text": "secret of A"}),
            true,
        ))
        .await
        .expect("remember for a");

    let resp = app
        .oneshot(post(
            "/v1/recall",
            &json!({"agent_id": "b", "query": "secret of A"}),
            true,
        ))
        .await
        .expect("recall for b");
    let body = json_body(resp).await;
    assert_eq!(body["results"], json!([]));
}

#[tokio::test]
async fn graph_isolation_between_agents() {
    let app = app();
    app.clone()
        .oneshot(post(
            "/v1/graph/entities",
            &json!({"agent_id": "a", "id": "alice", "kind": "person", "label": "Alice"}),
            true,
        ))
        .await
        .expect("add entity for a");

    let resp = app
        .oneshot(post(
            "/v1/recall_graph",
            &json!({"agent_id": "b", "start": "alice", "max_depth": 2}),
            true,
        ))
        .await
        .expect("traverse for b");
    let body = json_body(resp).await;
    assert_eq!(body["nodes"], json!([]));
}

#[tokio::test]
async fn fixed_agent_policy_rejects_other_agents() {
    let app = app_with_fixed_agent("only-this-agent");

    let resp = app
        .clone()
        .oneshot(post(
            "/v1/remember",
            &json!({"agent_id": "someone-else", "text": "x"}),
            true,
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let resp = app
        .oneshot(post(
            "/v1/remember",
            &json!({"agent_id": "only-this-agent", "text": "x"}),
            true,
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::CREATED);
}
