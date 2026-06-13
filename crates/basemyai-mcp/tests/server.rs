//! Tests d'intégration du serveur MCP via l'`InMemoryProvider` (sans CMake ni
//! Candle). On appelle directement les handlers d'outils — c'est le même chemin
//! que celui emprunté par le routeur MCP (pool, troncation, audit).
#![cfg(feature = "test-util")]

use std::sync::Arc;

use basemyai_mcp::{
    Config, InMemoryProvider, InvalidateParams, McpServer, RecallGraphParams, RecallParams, RememberParams, StatsParams,
};
use rmcp::handler::server::wrapper::{Json, Parameters};

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
