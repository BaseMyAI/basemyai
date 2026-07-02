//! Tests d'intégration du sidecar via `oneshot` (pas de socket réseau) et
//! l'`InMemoryProvider` (ni CMake ni Candle). Couvre auth, round-trip, headers,
//! erreurs et purge.
#![cfg(feature = "test-util")]

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use basemyai_rest::{AgentPolicy, AppState, Config, InMemoryProvider, build_app};

const KEY: &str = "test-secret-key";

fn app() -> Router {
    let config = Config {
        api_key: Some(KEY.to_string()),
        ..Config::default()
    };
    build_app(AppState::new(Arc::new(InMemoryProvider::new()), config))
}

fn post(uri: &str, body: &Value, auth: bool) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    if auth {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {KEY}"));
    }
    builder.body(Body::from(body.to_string())).expect("request")
}

fn get(uri: &str, auth: bool) -> Request<Body> {
    let mut builder = Request::builder().method("GET").uri(uri);
    if auth {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {KEY}"));
    }
    builder.body(Body::empty()).expect("request")
}

async fn json_body(resp: axum::response::Response) -> Value {
    let bytes = resp.into_body().collect().await.expect("collect").to_bytes();
    serde_json::from_slice(&bytes).expect("json")
}

#[tokio::test]
async fn health_needs_no_auth_and_sets_headers() {
    let resp = app().oneshot(get("/v1/health", false)).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers().contains_key("x-basemyai-version"));
    assert!(resp.headers().contains_key("x-request-id"));
    let body = json_body(resp).await;
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn missing_bearer_is_unauthorized() {
    let req = post("/v1/remember", &json!({"agent_id": "a", "text": "x"}), false);
    let resp = app().oneshot(req).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let body = json_body(resp).await;
    assert_eq!(body["error"]["code"], "UNAUTHORIZED");
}

#[tokio::test]
async fn dev_mode_allows_requests_without_bearer() {
    let config = Config {
        dev: true,
        ..Config::default()
    };
    let app = build_app(AppState::new(Arc::new(InMemoryProvider::new()), config));

    let resp = app
        .oneshot(post(
            "/v1/remember",
            &json!({"agent_id": "a", "text": "dev mode fact"}),
            false,
        ))
        .await
        .expect("oneshot");

    assert_eq!(resp.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn fixed_agent_policy_rejects_other_agents() {
    let config = Config {
        api_key: Some(KEY.to_string()),
        agent_policy: AgentPolicy::Fixed("allowed".to_string()),
        ..Config::default()
    };
    let app = build_app(AppState::new(Arc::new(InMemoryProvider::new()), config));

    let resp = app
        .clone()
        .oneshot(post(
            "/v1/remember",
            &json!({"agent_id": "other", "text": "should fail"}),
            true,
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let resp = app
        .oneshot(post(
            "/v1/remember",
            &json!({"agent_id": "allowed", "text": "should pass"}),
            true,
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::CREATED);
}

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
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/v1/memories/{id}?agent_id=a"))
                .header(header::AUTHORIZATION, format!("Bearer {KEY}"))
                .body(Body::empty())
                .expect("req"),
        )
        .await
        .expect("forget");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let resp = app
        .clone()
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
async fn recall_hybrid_surfaces_exact_term() {
    let app = app();
    app.clone()
        .oneshot(post(
            "/v1/remember",
            &json!({"agent_id": "a", "text": "invoice ACME-42 reference number", "layer": "semantic"}),
            true,
        ))
        .await
        .expect("remember");

    let resp = app
        .oneshot(post(
            "/v1/recall_hybrid",
            &json!({"agent_id": "a", "query": "ACME-42"}),
            true,
        ))
        .await
        .expect("recall_hybrid");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    let results = body["results"].as_array().expect("results");
    assert!(
        results
            .iter()
            .any(|r| r["text"].as_str().unwrap_or_default().contains("ACME-42")),
        "le recall hybride doit faire remonter le terme exact via BM25"
    );
}

#[tokio::test]
async fn recall_hybrid_rejects_layer_filter() {
    let resp = app()
        .oneshot(post(
            "/v1/recall_hybrid",
            &json!({"agent_id": "a", "query": "ACME-42", "layer": "semantic"}),
            true,
        ))
        .await
        .expect("recall_hybrid");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = json_body(resp).await;
    assert_eq!(body["error"]["code"], "VALIDATION_ERROR");
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("layer is not supported"))
    );
}

#[tokio::test]
async fn unknown_layer_is_bad_request() {
    let req = post(
        "/v1/remember",
        &json!({"agent_id": "a", "text": "x", "layer": "bogus"}),
        true,
    );
    let resp = app().oneshot(req).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = json_body(resp).await;
    assert_eq!(body["error"]["code"], "UNKNOWN_LAYER");
}

#[tokio::test]
async fn recall_graph_returns_empty_nodes() {
    let app = app();
    app.clone()
        .oneshot(post(
            "/v1/remember",
            &json!({"agent_id": "a", "text": "seed", "layer": "episodic"}),
            true,
        ))
        .await
        .expect("seed");
    let resp = app
        .oneshot(post(
            "/v1/recall_graph",
            &json!({"agent_id": "a", "start": "alice"}),
            true,
        ))
        .await
        .expect("recall_graph");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = json_body(resp).await;
    assert!(body["nodes"].as_array().expect("nodes").is_empty());
    assert_eq!(body["truncated"], false);
}

#[tokio::test]
async fn forget_agent_requires_matching_confirmation() {
    let app = app();
    app.clone()
        .oneshot(post(
            "/v1/remember",
            &json!({"agent_id": "a", "text": "fact", "layer": "semantic"}),
            true,
        ))
        .await
        .expect("remember");

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/v1/agent/a")
                .header(header::AUTHORIZATION, format!("Bearer {KEY}"))
                .body(Body::empty())
                .expect("req"),
        )
        .await
        .expect("forget agent");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = json_body(resp).await;
    assert_eq!(body["error"]["code"], "VALIDATION_ERROR");

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/v1/agent/a?confirm=b")
                .header(header::AUTHORIZATION, format!("Bearer {KEY}"))
                .body(Body::empty())
                .expect("req"),
        )
        .await
        .expect("forget agent");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = json_body(resp).await;
    assert_eq!(body["error"]["code"], "VALIDATION_ERROR");

    let resp = app
        .clone()
        .oneshot(get("/v1/agent/a/stats", true))
        .await
        .expect("stats");
    let body = json_body(resp).await;
    assert_eq!(body["total"], 1);
}

#[tokio::test]
async fn forget_agent_purges_all_with_matching_confirmation() {
    let app = app();
    app.clone()
        .oneshot(post(
            "/v1/remember",
            &json!({"agent_id": "a", "text": "fact", "layer": "semantic"}),
            true,
        ))
        .await
        .expect("remember");

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/v1/agent/a?confirm=a")
                .header(header::AUTHORIZATION, format!("Bearer {KEY}"))
                .body(Body::empty())
                .expect("req"),
        )
        .await
        .expect("forget agent");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let resp = app.oneshot(get("/v1/agent/a/stats", true)).await.expect("stats");
    let body = json_body(resp).await;
    assert_eq!(body["total"], 0);
}

#[tokio::test]
async fn recall_rejects_k_out_of_bounds() {
    let app = app();
    let resp = app
        .oneshot(post(
            "/v1/recall",
            &json!({"agent_id": "a", "query": "anything", "k": 2_000_000_000_u64}),
            true,
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = json_body(resp).await;
    assert_eq!(body["error"]["code"], "VALIDATION_ERROR");
}

#[tokio::test]
async fn recall_graph_rejects_max_depth_out_of_bounds() {
    let app = app();
    let resp = app
        .oneshot(post(
            "/v1/recall_graph",
            &json!({"agent_id": "a", "start": "alice", "max_depth": 100_000}),
            true,
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = json_body(resp).await;
    assert_eq!(body["error"]["code"], "VALIDATION_ERROR");
}

#[tokio::test]
async fn remember_rejects_text_too_long() {
    let app = app();
    let text = "x".repeat(65_537);
    let resp = app
        .oneshot(post("/v1/remember", &json!({"agent_id": "a", "text": text}), true))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = json_body(resp).await;
    assert_eq!(body["error"]["code"], "VALIDATION_ERROR");
}

#[tokio::test]
async fn remember_rejects_agent_id_too_long() {
    let app = app();
    let agent_id = "a".repeat(129);
    let resp = app
        .oneshot(post("/v1/remember", &json!({"agent_id": agent_id, "text": "x"}), true))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = json_body(resp).await;
    assert_eq!(body["error"]["code"], "VALIDATION_ERROR");
}

#[tokio::test]
async fn agent_stats_rejects_agent_id_too_long() {
    let app = app();
    let agent_id = "a".repeat(129);
    let resp = app
        .oneshot(get(&format!("/v1/agent/{agent_id}/stats"), true))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = json_body(resp).await;
    assert_eq!(body["error"]["code"], "VALIDATION_ERROR");
}

#[tokio::test]
async fn forget_agent_rejects_agent_id_too_long() {
    let app = app();
    let agent_id = "a".repeat(129);
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/v1/agent/{agent_id}?confirm={agent_id}"))
                .header(header::AUTHORIZATION, format!("Bearer {KEY}"))
                .body(Body::empty())
                .expect("req"),
        )
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = json_body(resp).await;
    assert_eq!(body["error"]["code"], "VALIDATION_ERROR");
}

#[tokio::test]
async fn valid_recall_request_still_passes_validation() {
    let app = app();
    app.clone()
        .oneshot(post(
            "/v1/remember",
            &json!({"agent_id": "a", "text": "the sky is blue", "layer": "semantic"}),
            true,
        ))
        .await
        .expect("remember");

    let resp = app
        .oneshot(post(
            "/v1/recall",
            &json!({"agent_id": "a", "query": "the sky is blue", "k": 5}),
            true,
        ))
        .await
        .expect("recall");
    assert_eq!(resp.status(), StatusCode::OK);
}

/// Lit les frames du corps SSE jusqu'à ce que `predicate` matche le texte
/// accumulé, ou jusqu'au timeout (le test échoue explicitement plutôt que de
/// bloquer indéfiniment si l'événement n'arrive jamais).
async fn read_sse_until(body: &mut axum::body::Body, predicate: impl Fn(&str) -> bool, timeout: Duration) -> String {
    let mut acc = String::new();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return acc;
        }
        match tokio::time::timeout(remaining, body.frame()).await {
            Ok(Some(Ok(frame))) => {
                if let Ok(data) = frame.into_data() {
                    acc.push_str(&String::from_utf8_lossy(&data));
                    if predicate(&acc) {
                        return acc;
                    }
                }
            }
            // Fin de flux ou timeout : on renvoie ce qui a été accumulé (vide si rien).
            Ok(Some(Err(_)) | None) | Err(_) => return acc,
        }
    }
}

#[tokio::test]
async fn watch_delivers_remembered_event_for_same_agent() {
    let app = app();

    let watch_resp = app
        .clone()
        .oneshot(get("/v1/watch?agent_id=a", true))
        .await
        .expect("watch");
    assert_eq!(watch_resp.status(), StatusCode::OK);
    assert_eq!(
        watch_resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("text/event-stream")
    );
    let mut body = watch_resp.into_body();

    let remember_resp = app
        .clone()
        .oneshot(post(
            "/v1/remember",
            &json!({"agent_id": "a", "text": "sse fact", "layer": "semantic"}),
            true,
        ))
        .await
        .expect("remember");
    assert_eq!(remember_resp.status(), StatusCode::CREATED);
    let id = json_body(remember_resp).await["id"].as_str().expect("id").to_string();

    let received = read_sse_until(
        &mut body,
        |acc| acc.contains("\"remembered\""),
        Duration::from_secs(5),
    )
    .await;

    assert!(
        received.contains("\"remembered\""),
        "expected a remembered event, got: {received:?}"
    );
    assert!(received.contains(&id), "event should carry the memory id: {received:?}");
    assert!(received.contains("\"agent_id\":\"a\""), "event should carry agent_id: {received:?}");
}

#[tokio::test]
async fn watch_isolates_events_from_other_agents() {
    let app = app();

    let watch_resp = app
        .clone()
        .oneshot(get("/v1/watch?agent_id=a", true))
        .await
        .expect("watch");
    assert_eq!(watch_resp.status(), StatusCode::OK);
    let mut body = watch_resp.into_body();

    // Rafale d'écritures au nom de l'agent B : rien ne doit atteindre A.
    for i in 0..5 {
        let resp = app
            .clone()
            .oneshot(post(
                "/v1/remember",
                &json!({"agent_id": "b", "text": format!("other agent fact {i}")}),
                true,
            ))
            .await
            .expect("remember for b");
        assert_eq!(resp.status(), StatusCode::CREATED);
    }

    // Aucun événement ne doit jamais arriver sur le flux de A : on attend un
    // court délai fixe et on vérifie qu'il ne s'est rien passé (pas de course
    // possible puisque rien n'est censé arriver, quel que soit le délai).
    let received = read_sse_until(&mut body, |_| false, Duration::from_millis(300)).await;
    assert!(
        received.is_empty(),
        "agent a's stream must not receive agent b's events: {received:?}"
    );
}

#[tokio::test]
async fn remember_is_rate_limited_per_agent() {
    let config = Config {
        api_key: Some(KEY.to_string()),
        ..Config::default()
    };
    let app = build_app(AppState::with_rate_limit(
        Arc::new(InMemoryProvider::new()),
        config,
        3,
        Duration::from_secs(60),
    ));

    for i in 0..3 {
        let resp = app
            .clone()
            .oneshot(post(
                "/v1/remember",
                &json!({"agent_id": "a", "text": format!("fact {i}")}),
                true,
            ))
            .await
            .expect("oneshot");
        assert_eq!(resp.status(), StatusCode::CREATED, "call {i} should succeed");
    }

    let resp = app
        .oneshot(post(
            "/v1/remember",
            &json!({"agent_id": "a", "text": "one too many"}),
            true,
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    let body = json_body(resp).await;
    assert_eq!(body["error"]["code"], "RATE_LIMITED");
}
