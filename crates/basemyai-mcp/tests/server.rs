//! Tests d'intégration du serveur MCP via l'`InMemoryProvider` (sans CMake ni
//! Candle). On appelle directement les handlers d'outils — c'est le même chemin
//! que celui emprunté par le routeur MCP (pool, troncation, audit).
#![cfg(feature = "test-util")]

use std::sync::Arc;

use basemyai_mcp::{
    ApplyEntity, ApplyRelation, Config, ConsolidateApplyParams, InMemoryProvider, InvalidateParams, McpServer,
    RecallGraphParams, RecallParams, RememberParams, StatsParams,
};
use rmcp::ServiceExt;
use rmcp::handler::client::ClientHandler;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{CallToolRequestParams, ClientInfo, object};
use rmcp::service::{RoleClient, RunningService};
use serde_json::{Value, json};

fn server() -> McpServer {
    McpServer::new(Arc::new(InMemoryProvider::new()), Config::default())
}

fn remember(agent: &str, text: &str, layer: &str) -> Parameters<RememberParams> {
    Parameters(RememberParams {
        agent_id: agent.to_string(),
        text: text.to_string(),
        layer: layer.to_string(),
    })
}

fn overlong_agent_id() -> String {
    "a".repeat(129)
}

fn call(name: &'static str, args: Value) -> CallToolRequestParams {
    let mut p = CallToolRequestParams::new(name);
    p.arguments = Some(object(args));
    p
}

#[derive(Clone)]
struct BasicClient;

impl ClientHandler for BasicClient {
    fn get_info(&self) -> ClientInfo {
        ClientInfo::default()
    }
}

async fn mcp_client() -> (RunningService<RoleClient, BasicClient>, tokio::task::JoinHandle<()>) {
    let (server_io, client_io) = tokio::io::duplex(1 << 16);
    let server = server();
    let server_task = tokio::spawn(async move {
        let running = server.serve(server_io).await.expect("serve server");
        running.waiting().await.expect("server loop");
    });
    let client = BasicClient.serve(client_io).await.expect("serve client");
    (client, server_task)
}

async fn shutdown(client: RunningService<RoleClient, BasicClient>, server_task: tokio::task::JoinHandle<()>) {
    client.cancel().await.expect("cancel client");
    let _ = server_task.await;
}

#[tokio::test]
async fn remember_recall_stats_invalidate_roundtrip() {
    let s = server();

    let Json(created) = s
        .remember(remember("a", "the sky is blue", "semantic"))
        .await
        .expect("remember");
    assert!(!created.id.is_empty(), "remember renvoie l'UUID créé");

    let Json(rec) = s
        .recall(Parameters(RecallParams {
            agent_id: "a".to_string(),
            query: "the sky is blue".to_string(),
            k: 5,
            include_procedural: false,
            exclude_imported: false,
        }))
        .await
        .expect("recall");
    assert!(rec.items.iter().any(|i| i.id == created.id), "le souvenir est retrouvé");
    assert!(!rec.truncated);

    let Json(st) = s
        .stats(Parameters(StatsParams {
            agent_id: "a".to_string(),
        }))
        .await
        .expect("stats");
    assert_eq!(st.semantic, 1);
    assert_eq!(st.total, 1);

    let Json(inv) = s
        .invalidate(Parameters(InvalidateParams {
            agent_id: "a".to_string(),
            id: created.id.clone(),
        }))
        .await
        .expect("invalidate");
    assert!(inv.invalidated);

    let Json(after) = s
        .recall(Parameters(RecallParams {
            agent_id: "a".to_string(),
            query: "the sky is blue".to_string(),
            k: 5,
            include_procedural: false,
            exclude_imported: false,
        }))
        .await
        .expect("recall after invalidate");
    assert!(
        after.items.iter().all(|i| i.id != created.id),
        "un souvenir invalidé ne réapparaît pas dans les recalls"
    );
}

#[tokio::test]
async fn recall_hybrid_surfaces_exact_term() {
    let s = server();
    s.remember(remember("a", "invoice ACME-42 reference number", "semantic"))
        .await
        .expect("remember");

    let Json(rec) = s
        .recall_hybrid(Parameters(RecallParams {
            agent_id: "a".to_string(),
            query: "ACME-42".to_string(),
            k: 5,
            include_procedural: false,
            exclude_imported: false,
        }))
        .await
        .expect("recall_hybrid");
    assert!(
        rec.items.iter().any(|i| i.text.contains("ACME-42")),
        "le recall hybride doit faire remonter le terme exact via BM25"
    );
}

#[tokio::test]
async fn isolation_between_agents() {
    let s = server();
    s.remember(remember("a", "secret of agent A", "semantic"))
        .await
        .expect("remember a");

    let Json(rb) = s
        .recall(Parameters(RecallParams {
            agent_id: "b".to_string(),
            query: "secret of agent A".to_string(),
            k: 5,
            include_procedural: false,
            exclude_imported: false,
        }))
        .await
        .expect("recall b");
    assert!(rb.items.is_empty(), "l'agent B ne voit jamais la mémoire de A");
}

#[tokio::test]
async fn empty_agent_id_is_rejected() {
    let s = server();
    let err = match s.remember(remember("", "x", "semantic")).await {
        Err(e) => e,
        Ok(_) => panic!("un agent_id vide doit être rejeté"),
    };
    assert!(
        err.message.contains("agent_id"),
        "agent_id vide rejeté : {}",
        err.message
    );
}

#[tokio::test]
async fn unknown_layer_is_rejected() {
    let s = server();
    let err = match s.remember(remember("a", "x", "bogus")).await {
        Err(e) => e,
        Ok(_) => panic!("une couche inconnue doit être rejetée"),
    };
    assert!(
        err.message.contains("layer"),
        "couche inconnue rejetée : {}",
        err.message
    );
}

#[tokio::test]
async fn recall_k_out_of_bounds_is_rejected() {
    let s = server();
    let err = match s
        .recall(Parameters(RecallParams {
            agent_id: "a".to_string(),
            query: "anything".to_string(),
            k: 2_000_000_000,
            include_procedural: false,
            exclude_imported: false,
        }))
        .await
    {
        Err(e) => e,
        Ok(_) => panic!("un k hors bornes doit être rejeté"),
    };
    assert!(err.message.contains('k'), "k hors bornes rejeté : {}", err.message);
}

#[tokio::test]
async fn recall_graph_max_depth_out_of_bounds_is_rejected() {
    let s = server();
    let err = match s
        .recall_graph(Parameters(RecallGraphParams {
            agent_id: "a".to_string(),
            start: "alice".to_string(),
            max_depth: 100_000,
        }))
        .await
    {
        Err(e) => e,
        Ok(_) => panic!("un max_depth hors bornes doit être rejeté"),
    };
    assert!(
        err.message.contains("max_depth"),
        "max_depth hors bornes rejeté : {}",
        err.message
    );
}

#[tokio::test]
async fn remember_text_too_long_is_rejected() {
    let s = server();
    let text = "x".repeat(65_537);
    let err = match s.remember(remember("a", &text, "semantic")).await {
        Err(e) => e,
        Ok(_) => panic!("un text trop long doit être rejeté"),
    };
    assert!(err.message.contains("text"), "text trop long rejeté : {}", err.message);
}

#[tokio::test]
async fn remember_agent_id_too_long_is_rejected() {
    let s = server();
    let agent_id = overlong_agent_id();
    let err = match s.remember(remember(&agent_id, "x", "semantic")).await {
        Err(e) => e,
        Ok(_) => panic!("un agent_id trop long doit être rejeté"),
    };
    assert!(
        err.message.contains("agent_id"),
        "agent_id trop long rejeté : {}",
        err.message
    );
}

#[tokio::test]
async fn invalidate_agent_id_too_long_is_rejected() {
    let s = server();
    let err = match s
        .invalidate(Parameters(InvalidateParams {
            agent_id: overlong_agent_id(),
            id: "memory-id".to_string(),
        }))
        .await
    {
        Err(e) => e,
        Ok(_) => panic!("un agent_id trop long doit être rejeté"),
    };
    assert!(
        err.message.contains("agent_id"),
        "agent_id trop long rejeté : {}",
        err.message
    );
}

#[tokio::test]
async fn stats_agent_id_too_long_is_rejected() {
    let s = server();
    let err = match s
        .stats(Parameters(StatsParams {
            agent_id: overlong_agent_id(),
        }))
        .await
    {
        Err(e) => e,
        Ok(_) => panic!("un agent_id trop long doit être rejeté"),
    };
    assert!(
        err.message.contains("agent_id"),
        "agent_id trop long rejeté : {}",
        err.message
    );
}

#[tokio::test]
async fn consolidate_agent_id_too_long_is_rejected() {
    let (client, server_task) = mcp_client().await;
    let err = client
        .call_tool(call("consolidate", json!({ "agent_id": overlong_agent_id() })))
        .await
        .expect_err("un agent_id trop long doit être rejeté");
    assert!(
        err.to_string().contains("agent_id"),
        "agent_id trop long rejeté : {err}"
    );
    shutdown(client, server_task).await;
}

#[tokio::test]
async fn consolidate_apply_oversized_facts_is_rejected() {
    use basemyai::MAX_CONSOLIDATION_FACTS;
    use basemyai_mcp::{ApplyEntity, ApplyRelation, ConsolidateApplyParams};

    let s = server();
    let err = match s
        .consolidate_apply(Parameters(ConsolidateApplyParams {
            agent_id: "a".to_string(),
            facts: vec!["f".to_string(); MAX_CONSOLIDATION_FACTS + 1],
            entities: vec![ApplyEntity {
                id: "alice".to_string(),
                kind: "person".to_string(),
                label: "Alice".to_string(),
            }],
            relations: vec![ApplyRelation {
                src: "alice".to_string(),
                relation: "knows".to_string(),
                dst: "alice".to_string(),
            }],
        }))
        .await
    {
        Err(e) => e,
        Ok(_) => panic!("payload massif doit être rejeté"),
    };
    assert!(
        err.message.contains("trop de faits"),
        "rejet explicite : {}",
        err.message
    );
}

#[tokio::test]
async fn consolidate_apply_agent_id_too_long_is_rejected() {
    let s = server();
    let err = match s
        .consolidate_apply(Parameters(ConsolidateApplyParams {
            agent_id: overlong_agent_id(),
            facts: vec!["fact".to_string()],
            entities: vec![ApplyEntity {
                id: "alice".to_string(),
                kind: "person".to_string(),
                label: "Alice".to_string(),
            }],
            relations: vec![ApplyRelation {
                src: "alice".to_string(),
                relation: "knows".to_string(),
                dst: "alice".to_string(),
            }],
        }))
        .await
    {
        Err(e) => e,
        Ok(_) => panic!("un agent_id trop long doit être rejeté"),
    };
    assert!(
        err.message.contains("agent_id"),
        "agent_id trop long rejeté : {}",
        err.message
    );
}

#[tokio::test]
async fn recall_graph_on_empty_graph_is_ok() {
    let s = server();
    s.remember(remember("a", "seed episode", "episodic"))
        .await
        .expect("seed");

    let Json(g) = s
        .recall_graph(Parameters(RecallGraphParams {
            agent_id: "a".to_string(),
            start: "alice".to_string(),
            max_depth: 2,
        }))
        .await
        .expect("recall_graph");
    assert!(g.entities.is_empty());
    assert!(!g.truncated);
}
