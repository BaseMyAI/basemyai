//! Bornes de validation et formes de réponse du domaine `graph`.

use axum::http::StatusCode;
use serde_json::json;
use tower::ServiceExt;

use crate::support::app::app;
use crate::support::client::{json_body, post};

#[tokio::test]
async fn traverse_rejects_max_depth_out_of_bounds() {
    let req = post(
        "/v1/recall_graph",
        &json!({"agent_id": "a", "start": "alice", "max_depth": 0}),
        true,
    );
    let resp = app().oneshot(req).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn traverse_on_empty_graph_is_ok() {
    let req = post("/v1/recall_graph", &json!({"agent_id": "a", "start": "alice"}), true);
    let resp = app().oneshot(req).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["nodes"], json!([]));
    assert_eq!(body["truncated"], false);
}

#[tokio::test]
async fn add_entity_rejects_empty_id() {
    let req = post(
        "/v1/graph/entities",
        &json!({"agent_id": "a", "id": "", "kind": "person", "label": "Alice"}),
        true,
    );
    let resp = app().oneshot(req).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn add_relation_rejects_non_finite_weight() {
    let req = post(
        "/v1/graph/relations",
        &json!({"agent_id": "a", "src": "alice", "relation": "knows", "dst": "bob", "weight": f64::NAN}),
        true,
    );
    let resp = app().oneshot(req).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}
