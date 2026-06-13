//! Tests de bout en bout de la **consolidation** côté MCP (ADR-018).
//!
//! Monte un vrai serveur MCP et un vrai client MCP reliés par un duplex
//! in-memory — aucun réseau, aucun LLM réel, 100 % déterministe.
//!
//! 1. `consolidate_borrows_client_llm_via_sampling` : le client **annonce** la
//!    capability `sampling` et répond à `create_message` ; `consolidate` emprunte
//!    donc son LLM (niveau 1 de la politique). Chemin :
//!    remember → consolidate (sampling) → graphe → recall_graph.
//! 2. `consolidate_apply_persists_agent_extraction` : flux **piloté par l'agent**
//!    (niveau 3) — l'agent a déjà extrait le JSON et le persiste via
//!    `consolidate_apply`. Chemin : consolidate_apply → graphe + fait sémantique.
#![cfg(feature = "test-util")]

use std::sync::Arc;

use basemyai_mcp::{Config, InMemoryProvider, McpServer};
use rmcp::model::{
    CallToolRequestParams, ClientCapabilities, ClientInfo, CreateMessageRequestParams, CreateMessageResult, Role,
    SamplingMessage, SamplingMessageContent, object,
};
use rmcp::service::RequestContext;
use rmcp::{ClientHandler, ErrorData, RoleClient, ServiceExt};
use serde_json::{Value, json};

/// Construit un appel d'outil (le param est `#[non_exhaustive]` : pas de literal).
fn call(name: &'static str, args: Value) -> CallToolRequestParams {
    let mut p = CallToolRequestParams::new(name);
    p.arguments = Some(object(args));
    p
}

/// Le JSON que notre faux client « LLM » renvoie pour toute requête de sampling.
/// Conforme au schéma d'extraction de `consolidate` (facts / entities / relations).
const EXTRACTION_JSON: &str = r#"{
  "facts": ["Alice founded BaseMyAI"],
  "entities": [
    {"id": "alice", "kind": "person", "label": "Alice"},
    {"id": "basemyai", "kind": "org", "label": "BaseMyAI"}
  ],
  "relations": [
    {"src": "alice", "relation": "founded", "dst": "basemyai"}
  ]
}"#;

/// Client MCP de test : **annonce** la capability `sampling` (sinon le serveur,
/// suivant ADR-018, ne l'emprunterait pas) et répond avec un JSON fixe. C'est
/// l'analogue, en test, d'un client prêtant son LLM au serveur.
#[derive(Clone)]
struct SamplingClient;

impl ClientHandler for SamplingClient {
    #[allow(clippy::field_reassign_with_default)] // ClientInfo est #[non_exhaustive] : pas de struct-literal.
    fn get_info(&self) -> ClientInfo {
        let mut info = ClientInfo::default();
        info.capabilities = ClientCapabilities::builder().enable_sampling().build();
        info
    }

    async fn create_message(
        &self,
        _params: CreateMessageRequestParams,
        _context: RequestContext<RoleClient>,
    ) -> Result<CreateMessageResult, ErrorData> {
        Ok(CreateMessageResult::new(
            SamplingMessage::new(Role::Assistant, SamplingMessageContent::text(EXTRACTION_JSON)),
            "test-sampler".to_string(),
        ))
    }
}

#[tokio::test]
async fn consolidate_borrows_client_llm_via_sampling() {
    let (server_io, client_io) = tokio::io::duplex(1 << 16);

    let server = McpServer::new(Arc::new(InMemoryProvider::new()), Config::default());
    let server_task = tokio::spawn(async move {
        let running = server.serve(server_io).await.expect("serve server");
        running.waiting().await.expect("server loop");
    });

    let client = SamplingClient.serve(client_io).await.expect("serve client");

    // 1) Mémorise un épisode.
    client
        .call_tool(call(
            "remember",
            json!({ "agent_id": "a", "text": "Alice founded BaseMyAI in Paris.", "layer": "episodic" }),
        ))
        .await
        .expect("remember tool");

    // 2) Consolide : le client annonce le sampling → le serveur lui emprunte son LLM.
    let consolidate = client
        .call_tool(call("consolidate", json!({ "agent_id": "a" })))
        .await
        .expect("consolidate tool");
    assert_ne!(consolidate.is_error, Some(true), "la consolidation par sampling ne doit pas être en erreur");

    let result = consolidate.structured_content.expect("consolidate structured content");
    assert_eq!(result["status"], "done", "doit avoir consolidé côté serveur : {result:?}");
    assert_eq!(result["via"], "sampling", "doit être passé par le sampling : {result:?}");

    // 3) Le graphe doit être peuplé : on traverse depuis « alice » → « basemyai ».
    let graph = client
        .call_tool(call("recall_graph", json!({ "agent_id": "a", "start": "alice", "max_depth": 2 })))
        .await
        .expect("recall_graph tool");

    let value = graph.structured_content.expect("recall_graph structured content");
    let entities = value["entities"].as_array().expect("entities array");
    assert!(
        entities.iter().any(|e| e["id"] == "basemyai"),
        "la consolidation par sampling doit avoir créé l'entité et la relation : {entities:?}"
    );

    client.cancel().await.expect("cancel client");
    server_task.abort();
}

#[tokio::test]
async fn consolidate_apply_persists_agent_extraction() {
    let (server_io, client_io) = tokio::io::duplex(1 << 16);

    let server = McpServer::new(Arc::new(InMemoryProvider::new()), Config::default());
    let server_task = tokio::spawn(async move {
        let running = server.serve(server_io).await.expect("serve server");
        running.waiting().await.expect("server loop");
    });

    let client = SamplingClient.serve(client_io).await.expect("serve client");

    // Flux agent-driven : l'agent a extrait le JSON lui-même et le persiste.
    let applied = client
        .call_tool(call(
            "consolidate_apply",
            json!({
                "agent_id": "b",
                "facts": ["Bob maintains BaseMyAI"],
                "entities": [
                    {"id": "bob", "kind": "person", "label": "Bob"},
                    {"id": "basemyai", "kind": "org", "label": "BaseMyAI"}
                ],
                "relations": [{"src": "bob", "relation": "maintains", "dst": "basemyai"}]
            }),
        ))
        .await
        .expect("consolidate_apply tool");
    assert_ne!(applied.is_error, Some(true), "consolidate_apply ne doit pas être en erreur");

    let result = applied.structured_content.expect("apply structured content");
    assert_eq!(result["status"], "done");
    assert_eq!(result["via"], "agent", "doit être marqué comme piloté par l'agent : {result:?}");
    assert_eq!(result["facts_added"], 1);
    assert_eq!(result["entities_upserted"], 2);
    assert_eq!(result["relations_upserted"], 1);

    // Le graphe est peuplé : depuis « bob » on atteint « basemyai ».
    let graph = client
        .call_tool(call("recall_graph", json!({ "agent_id": "b", "start": "bob", "max_depth": 2 })))
        .await
        .expect("recall_graph tool");
    let value = graph.structured_content.expect("recall_graph structured content");
    let entities = value["entities"].as_array().expect("entities array");
    assert!(
        entities.iter().any(|e| e["id"] == "basemyai"),
        "consolidate_apply doit avoir créé l'entité et la relation : {entities:?}"
    );

    // Le fait a été promu en couche sémantique et est rappelable.
    let recall = client
        .call_tool(call("recall", json!({ "agent_id": "b", "query": "who maintains BaseMyAI", "k": 5 })))
        .await
        .expect("recall tool");
    let value = recall.structured_content.expect("recall structured content");
    let items = value["items"].as_array().expect("items array");
    assert!(
        items.iter().any(|i| i["text"].as_str().is_some_and(|t| t.contains("Bob maintains BaseMyAI"))),
        "le fait promu doit être rappelable : {items:?}"
    );

    client.cancel().await.expect("cancel client");
    server_task.abort();
}
