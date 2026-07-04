//! Routeur axum + handlers conformes à la spec OpenAPI du sidecar.
//!
//! Toutes les routes métier sont sous `/v1` et protégées par auth Bearer
//! (sauf `/health`). Chaque réponse porte `X-Request-Id` et `X-Basemyai-Version`.

use std::convert::Infallible;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use futures_util::stream;
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tower::ServiceBuilder;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::timeout::TimeoutLayer;

use basemyai::{MemoryEvent, MemoryEventKind, MemoryLayer, Validity};

use crate::error::RestError;
use crate::state::AppState;

/// Construit l'application axum complète (middleware + routes).
pub fn build_app(state: AppState) -> Router {
    let protected = Router::new()
        .route("/remember", post(remember))
        .route("/recall", post(recall))
        .route("/recall_hybrid", post(recall_hybrid))
        .route("/recall_graph", post(recall_graph))
        .route("/memories/{id}", delete(forget_memory))
        .route("/agent/{agent_id}", delete(forget_agent))
        .route("/agent/{agent_id}/stats", get(agent_stats))
        .route("/watch", get(watch))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_auth));

    let public = Router::new().route("/health", get(health));

    let max_body = state.config.max_body_bytes;
    let timeout = Duration::from_secs(state.config.timeout_secs);
    let request_id = HeaderName::from_static("x-request-id");

    let mw = ServiceBuilder::new()
        .layer(SetRequestIdLayer::new(request_id.clone(), MakeRequestUuid))
        .layer(PropagateRequestIdLayer::new(request_id))
        .layer(SetResponseHeaderLayer::overriding(
            HeaderName::from_static("x-basemyai-version"),
            HeaderValue::from_static(env!("CARGO_PKG_VERSION")),
        ))
        .layer(RequestBodyLimitLayer::new(max_body))
        .layer(TimeoutLayer::with_status_code(StatusCode::GATEWAY_TIMEOUT, timeout));

    Router::new()
        .nest("/v1", protected.merge(public))
        .with_state(state)
        .layer(mw)
}

// --- Auth ------------------------------------------------------------------

/// Middleware : exige un Bearer valide (sauf en mode `dev`), en temps constant.
async fn require_auth(State(state): State<AppState>, req: axum::extract::Request, next: Next) -> Response {
    if state.config.dev {
        return next.run(req).await;
    }
    match state.config.api_key.as_deref() {
        Some(key) if bearer_ok(req.headers(), key) => next.run(req).await,
        _ => RestError::Unauthorized.into_response(),
    }
}

fn bearer_ok(headers: &axum::http::HeaderMap, api_key: &str) -> bool {
    let Some(value) = headers.get(axum::http::header::AUTHORIZATION) else {
        return false;
    };
    let Ok(text) = value.to_str() else {
        return false;
    };
    let Some(token) = text.strip_prefix("Bearer ") else {
        return false;
    };
    token.as_bytes().ct_eq(api_key.as_bytes()).into()
}

// --- Bornes de validation (conformes à `openapi.yaml`, racine du crate) ----

const MAX_AGENT_ID_LEN: usize = 128;
const MAX_TEXT_LEN: usize = 65_536;
const MAX_QUERY_LEN: usize = 4096;
const MIN_K: usize = 1;
const MAX_K: usize = 100;
const MIN_DEPTH: u32 = 1;
const MAX_DEPTH: u32 = 10;

fn validate_agent_id(agent_id: &str) -> Result<(), RestError> {
    if agent_id.is_empty() || agent_id.chars().count() > MAX_AGENT_ID_LEN {
        return Err(RestError::Validation(format!(
            "agent_id must be 1..={MAX_AGENT_ID_LEN} characters"
        )));
    }
    Ok(())
}

fn validate_text(text: &str) -> Result<(), RestError> {
    if text.is_empty() || text.chars().count() > MAX_TEXT_LEN {
        return Err(RestError::Validation(format!(
            "text must be 1..={MAX_TEXT_LEN} characters"
        )));
    }
    Ok(())
}

fn validate_query(query: &str) -> Result<(), RestError> {
    if query.is_empty() || query.chars().count() > MAX_QUERY_LEN {
        return Err(RestError::Validation(format!(
            "query must be 1..={MAX_QUERY_LEN} characters"
        )));
    }
    Ok(())
}

fn validate_k(k: usize) -> Result<(), RestError> {
    if !(MIN_K..=MAX_K).contains(&k) {
        return Err(RestError::Validation(format!("k must be {MIN_K}..={MAX_K}")));
    }
    Ok(())
}

fn validate_max_depth(max_depth: u32) -> Result<(), RestError> {
    if !(MIN_DEPTH..=MAX_DEPTH).contains(&max_depth) {
        return Err(RestError::Validation(format!(
            "max_depth must be {MIN_DEPTH}..={MAX_DEPTH}"
        )));
    }
    Ok(())
}

fn validate_start(start: &str) -> Result<(), RestError> {
    if start.is_empty() {
        return Err(RestError::Validation("start must not be empty".to_string()));
    }
    Ok(())
}

// --- DTOs ------------------------------------------------------------------

#[derive(Deserialize)]
struct RememberRequest {
    agent_id: String,
    text: String,
    #[serde(default = "default_layer")]
    layer: String,
    #[serde(default)]
    valid_until: Option<i64>,
}

fn default_layer() -> String {
    "semantic".to_string()
}

#[derive(Deserialize)]
struct RecallRequest {
    agent_id: String,
    query: String,
    #[serde(default = "default_k")]
    k: usize,
    #[serde(default)]
    layer: Option<String>,
}

fn default_k() -> usize {
    10
}

#[derive(Deserialize)]
struct RecallGraphRequest {
    agent_id: String,
    start: String,
    #[serde(default = "default_depth")]
    max_depth: u32,
}

fn default_depth() -> u32 {
    3
}

#[derive(Deserialize)]
struct AgentQuery {
    agent_id: String,
}

#[derive(Deserialize)]
struct DeleteAgentQuery {
    #[serde(default)]
    confirm: Option<String>,
}

/// `GET /v1/watch?agent_id=...&layer=...` : couche optionnelle, mêmes noms que
/// `layer` ailleurs (`from_table`, ex. `"semantic"`). Sans `layer`, tous les
/// événements de l'agent sont relayés.
#[derive(Deserialize)]
struct WatchQuery {
    agent_id: String,
    #[serde(default)]
    layer: Option<String>,
}

#[derive(Serialize)]
struct IdResponse {
    id: String,
}

#[derive(Serialize)]
struct RecordDto {
    id: String,
    text: String,
    layer: String,
    score: f32,
}

#[derive(Serialize)]
struct RecallResponse {
    results: Vec<RecordDto>,
    truncated: bool,
}

#[derive(Serialize)]
struct EntityDto {
    id: String,
    kind: String,
    label: String,
    depth: u32,
}

#[derive(Serialize)]
struct GraphResponse {
    nodes: Vec<EntityDto>,
    truncated: bool,
}

#[derive(Serialize)]
struct StatsResponse {
    short_term: usize,
    episodic: usize,
    procedural: usize,
    semantic: usize,
    total: usize,
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    version: &'static str,
}

/// Payload SSE minimal (ADR-022) : identité du souvenir + nature de la
/// mutation, jamais le contenu (l'abonné rappelle par `id` s'il le veut).
#[derive(Serialize)]
struct MemoryEventDto {
    agent_id: String,
    kind: &'static str,
    layer: &'static str,
    id: String,
}

impl From<&MemoryEvent> for MemoryEventDto {
    fn from(ev: &MemoryEvent) -> Self {
        Self {
            agent_id: ev.agent_id.clone(),
            kind: match ev.kind {
                MemoryEventKind::Remembered => "remembered",
                MemoryEventKind::Invalidated => "invalidated",
                MemoryEventKind::Forgotten => "forgotten",
                MemoryEventKind::Consolidated => "consolidated",
                // `MemoryEventKind` est `#[non_exhaustive]` : un genre futur
                // atterrit ici plutôt que de casser la compilation.
                _ => "unknown",
            },
            layer: ev.layer.table(),
            id: ev.id.clone(),
        }
    }
}

// --- Handlers --------------------------------------------------------------

async fn remember(
    State(state): State<AppState>,
    Json(req): Json<RememberRequest>,
) -> Result<impl IntoResponse, RestError> {
    validate_agent_id(&req.agent_id)?;
    validate_text(&req.text)?;
    if !state.check_remember_rate(&req.agent_id).await {
        return Err(RestError::RateLimited);
    }
    let mem = state.memory_for(&req.agent_id).await?;
    let layer = MemoryLayer::from_table(&req.layer)?;
    let validity = Validity {
        valid_from: now_unix(),
        valid_until: req.valid_until,
    };
    let id = mem.remember_with(&req.text, layer, validity).await?;
    Ok((StatusCode::CREATED, Json(IdResponse { id })))
}

async fn recall(State(state): State<AppState>, Json(req): Json<RecallRequest>) -> Result<impl IntoResponse, RestError> {
    validate_agent_id(&req.agent_id)?;
    validate_query(&req.query)?;
    validate_k(req.k)?;
    let mem = state.memory_for(&req.agent_id).await?;
    let records = match req.layer.as_deref() {
        Some(layer) => {
            let layer = MemoryLayer::from_table(layer)?;
            mem.recall_by_layer(&req.query, layer, req.k).await?
        }
        None => mem.recall(&req.query, req.k).await?,
    };
    let items: Vec<RecordDto> = records
        .into_iter()
        .map(|r| {
            let score = r.similarity();
            RecordDto {
                id: r.id,
                text: r.text,
                layer: r.layer.table().to_string(),
                score,
            }
        })
        .collect();
    let (results, truncated) = truncate_to_fit(items, state.config.max_result_bytes);
    Ok(Json(RecallResponse { results, truncated }))
}

async fn recall_hybrid(
    State(state): State<AppState>,
    Json(req): Json<RecallRequest>,
) -> Result<impl IntoResponse, RestError> {
    validate_agent_id(&req.agent_id)?;
    validate_query(&req.query)?;
    validate_k(req.k)?;
    if req.layer.is_some() {
        return Err(RestError::Validation(
            "layer is not supported by /recall_hybrid; use /recall for layer-filtered recall".to_string(),
        ));
    }
    let mem = state.memory_for(&req.agent_id).await?;
    // Hybride : vecteur + BM25 fusionnés (RRF). `score` porte le score RRF
    // fusionné (ADR-014).
    let records = mem.recall_hybrid(&req.query, req.k).await?;
    let items: Vec<RecordDto> = records
        .into_iter()
        .map(|r| RecordDto {
            id: r.id,
            text: r.text,
            layer: r.layer.table().to_string(),
            score: r.score,
        })
        .collect();
    let (results, truncated) = truncate_to_fit(items, state.config.max_result_bytes);
    Ok(Json(RecallResponse { results, truncated }))
}

async fn recall_graph(
    State(state): State<AppState>,
    Json(req): Json<RecallGraphRequest>,
) -> Result<impl IntoResponse, RestError> {
    validate_agent_id(&req.agent_id)?;
    validate_start(&req.start)?;
    validate_max_depth(req.max_depth)?;
    let mem = state.memory_for(&req.agent_id).await?;
    let reached = mem.graph().traverse(&req.start, req.max_depth).await?;
    let nodes: Vec<EntityDto> = reached
        .into_iter()
        .map(|e| EntityDto {
            id: e.id,
            kind: e.kind,
            label: e.label,
            depth: e.depth,
        })
        .collect();
    let (nodes, truncated) = truncate_to_fit(nodes, state.config.max_result_bytes);
    Ok(Json(GraphResponse { nodes, truncated }))
}

async fn forget_memory(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<AgentQuery>,
) -> Result<impl IntoResponse, RestError> {
    validate_agent_id(&q.agent_id)?;
    let mem = state.memory_for(&q.agent_id).await?;
    mem.forget(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn forget_agent(
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
    Query(q): Query<DeleteAgentQuery>,
) -> Result<impl IntoResponse, RestError> {
    validate_agent_id(&agent_id)?;
    if q.confirm.as_deref() != Some(agent_id.as_str()) {
        return Err(RestError::Validation(
            "confirm must exactly match agent_id for destructive agent deletion".to_string(),
        ));
    }
    let mem = state.memory_for(&agent_id).await?;
    mem.purge_agent().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn agent_stats(
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
) -> Result<impl IntoResponse, RestError> {
    validate_agent_id(&agent_id)?;
    let mem = state.memory_for(&agent_id).await?;
    let s = mem.stats().await?;
    Ok(Json(StatsResponse {
        short_term: s.short_term,
        episodic: s.episodic,
        procedural: s.procedural,
        semantic: s.semantic,
        total: s.total(),
    }))
}

/// `GET /v1/watch` : relaie [`basemyai::Memory::watch`] en SSE, un
/// [`MemoryEventDto`] JSON par ligne `data:`. L'isolation par agent/couche est
/// déjà garantie par `MemorySubscription::recv` (ADR-022) — cette route ne
/// refait aucun filtrage, elle passe `agent_id` tel quel.
///
/// Déconnexion propre : aucune tâche de fond n'est `spawn`ée. Le flux SSE est
/// tiré directement par le corps de réponse axum ; quand le client se
/// déconnecte, axum arrête de poller le flux et abandonne la `MemorySubscription`
/// portée par `stream::unfold`, ce qui désabonne le récepteur `broadcast` via
/// son `Drop` — pas d'arrêt explicite à coder.
async fn watch(State(state): State<AppState>, Query(q): Query<WatchQuery>) -> Result<impl IntoResponse, RestError> {
    validate_agent_id(&q.agent_id)?;
    let layer = q.layer.as_deref().map(MemoryLayer::from_table).transpose()?;
    let mem = state.memory_for(&q.agent_id).await?;
    let subscription = mem.watch(&q.agent_id, layer);

    let stream = stream::unfold(subscription, |mut subscription| async move {
        let event = subscription.recv().await?;
        let dto = MemoryEventDto::from(&event);
        let data = serde_json::to_string(&dto).unwrap_or_else(|_| "{}".to_string());
        Some((Ok::<_, Infallible>(Event::default().data(data)), subscription))
    });

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

async fn health() -> impl IntoResponse {
    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}

// --- Helpers ---------------------------------------------------------------

/// Tronque une liste sérialisable pour tenir sous `max_bytes` (best-effort).
fn truncate_to_fit<T: Serialize>(mut items: Vec<T>, max_bytes: usize) -> (Vec<T>, bool) {
    let mut truncated = false;
    while !items.is_empty() {
        match serde_json::to_vec(&items) {
            Ok(bytes) if bytes.len() <= max_bytes => break,
            _ => {
                items.pop();
                truncated = true;
            }
        }
    }
    (items, truncated)
}

/// Temps Unix courant (secondes, UTC).
fn now_unix() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}
