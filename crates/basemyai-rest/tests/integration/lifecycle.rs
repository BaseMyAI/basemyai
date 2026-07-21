//! Cycle de vie complet d'un souvenir : remember → recall → stats →
//! invalidate → forget, plus les domaines annexes (graph, maintenance,
//! export/import, purge) qui n'existaient pas avant cette restructuration.

use axum::http::StatusCode;
use serde_json::json;
use tower::ServiceExt;

use crate::support::app::app;
use crate::support::client::{delete, get, json_body, post};

#[tokio::test]
async fn remember_recall_stats_forget_roundtrip() {
    let app = app();

    let resp = app
        .clone()
        .oneshot(post(
            "/v1/remember",
            &json!({"agent_id": "a", "text": "the sky is blue", "layer": "semantic"}),
            true,
        ))
        .await
        .expect("remember");
    assert_eq!(resp.status(), StatusCode::CREATED);
    let id = json_body(resp).await["id"].as_str().expect("id").to_string();
    assert!(!id.is_empty());

    let resp = app
        .clone()
        .oneshot(post(
            "/v1/recall",
            &json!({"agent_id": "a", "query": "the sky is blue"}),
            true,
        ))
        .await
        .expect("recall");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["truncated"], false);
    let results = body["results"].as_array().expect("results");
    assert!(results.iter().any(|r| r["id"] == id));
    let score = results[0]["score"].as_f64().expect("score");
    assert!((0.0..=1.0).contains(&score), "score normalisé dans [0,1] : {score}");

    let resp = app
        .clone()
        .oneshot(get("/v1/agent/a/stats", true))
        .await
        .expect("stats");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["semantic"], 1);
    assert_eq!(body["total"], 1);

    let resp = app
        .clone()
        .oneshot(delete(&format!("/v1/memories/{id}?agent_id=a"), true))
        .await
        .expect("forget");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let resp = app
        .oneshot(post(
            "/v1/recall",
            &json!({"agent_id": "a", "query": "the sky is blue"}),
            true,
        ))
        .await
        .expect("recall2");
    let body = json_body(resp).await;
    assert!(body["results"].as_array().expect("results").is_empty());
}

#[tokio::test]
async fn invalidate_hides_a_memory_without_deleting_it() {
    let app = app();
    let resp = app
        .clone()
        .oneshot(post(
            "/v1/remember",
            &json!({"agent_id": "a", "text": "soft-deletable fact"}),
            true,
        ))
        .await
        .expect("remember");
    let id = json_body(resp).await["id"].as_str().expect("id").to_string();

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

    let resp = app
        .oneshot(post(
            "/v1/recall",
            &json!({"agent_id": "a", "query": "soft-deletable fact"}),
            true,
        ))
        .await
        .expect("recall after invalidate");
    let body = json_body(resp).await;
    assert!(
        body["results"]
            .as_array()
            .expect("results")
            .iter()
            .all(|r| r["id"] != id)
    );
}

#[tokio::test]
async fn remember_batch_creates_all_ids_in_one_call() {
    let app = app();
    let resp = app
        .oneshot(post(
            "/v1/remember_batch",
            &json!({"agent_id": "a", "texts": ["fact one", "fact two", "fact three"]}),
            true,
        ))
        .await
        .expect("remember_batch");
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = json_body(resp).await;
    let ids = body["ids"].as_array().expect("ids");
    assert_eq!(ids.len(), 3);
}

#[tokio::test]
async fn graph_add_entity_edge_and_traverse_roundtrip() {
    let app = app();
    let resp = app
        .clone()
        .oneshot(post(
            "/v1/graph/entities",
            &json!({"agent_id": "a", "id": "alice", "kind": "person", "label": "Alice"}),
            true,
        ))
        .await
        .expect("add entity alice");
    assert_eq!(resp.status(), StatusCode::CREATED);

    app.clone()
        .oneshot(post(
            "/v1/graph/entities",
            &json!({"agent_id": "a", "id": "bob", "kind": "person", "label": "Bob"}),
            true,
        ))
        .await
        .expect("add entity bob");

    let resp = app
        .clone()
        .oneshot(post(
            "/v1/graph/relations",
            &json!({"agent_id": "a", "src": "alice", "relation": "knows", "dst": "bob"}),
            true,
        ))
        .await
        .expect("add relation");
    assert_eq!(resp.status(), StatusCode::CREATED);

    let resp = app
        .oneshot(post(
            "/v1/recall_graph",
            &json!({"agent_id": "a", "start": "alice", "max_depth": 2}),
            true,
        ))
        .await
        .expect("traverse");
    let body = json_body(resp).await;
    let nodes = body["nodes"].as_array().expect("nodes");
    assert!(nodes.iter().any(|n| n["id"] == "bob"));
}

#[tokio::test]
async fn maintenance_collect_expired_and_forget_adaptive_are_noops_on_empty_agent() {
    let app = app();
    let resp = app
        .clone()
        .oneshot(post("/v1/maintenance/collect_expired", &json!({"agent_id": "a"}), true))
        .await
        .expect("collect_expired");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["deleted"], 0);

    let resp = app
        .oneshot(post(
            "/v1/maintenance/forget_adaptive",
            &json!({"agent_id": "a", "capacity": 1000}),
            true,
        ))
        .await
        .expect("forget_adaptive");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["evicted"], 0);
}

#[tokio::test]
async fn agent_export_then_import_into_a_fresh_agent_restores_memories() {
    let app = app();
    app.clone()
        .oneshot(post(
            "/v1/remember",
            &json!({"agent_id": "source", "text": "exported fact"}),
            true,
        ))
        .await
        .expect("remember");

    let resp = app
        .clone()
        .oneshot(get("/v1/agent/source/export", true))
        .await
        .expect("export");
    assert_eq!(resp.status(), StatusCode::OK);
    let jsonl = String::from_utf8(
        axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .expect("bytes")
            .to_vec(),
    )
    .expect("utf8");
    assert!(jsonl.contains("exported fact"));

    let resp = app
        .oneshot(post("/v1/agent/destination/import", &json!({"jsonl": jsonl}), true))
        .await
        .expect("import");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert_eq!(body["memories"], 1);
}

#[tokio::test]
async fn forget_agent_requires_matching_confirmation() {
    let app = app();
    app.clone()
        .oneshot(post("/v1/remember", &json!({"agent_id": "a", "text": "x"}), true))
        .await
        .expect("remember");

    let resp = app
        .clone()
        .oneshot(delete("/v1/agent/a", true))
        .await
        .expect("delete without confirm");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let resp = app
        .clone()
        .oneshot(delete("/v1/agent/a?confirm=wrong-agent", true))
        .await
        .expect("delete with wrong confirm");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let resp = app
        .oneshot(delete("/v1/agent/a?confirm=a", true))
        .await
        .expect("delete with matching confirm");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}
