//! Façades destinées aux consommateurs externes (MCP, REST, bindings), gated
//! `test-util` : constructeur `:memory:` sans modèle, `remember` renvoyant l'UUID,
//! et accès graphe via `Memory::graph()`. Compile à vide sans la feature.
#![cfg(feature = "test-util")]

use basemyai::{Memory, MemoryLayer};

#[tokio::test]
async fn open_in_memory_roundtrip_returns_id() {
    let mem = Memory::open_in_memory("agent-x").await.expect("open in-memory");

    let id = mem
        .remember("the sky is blue", MemoryLayer::Semantic)
        .await
        .expect("remember");
    assert!(!id.is_empty(), "remember doit renvoyer un UUID non vide");

    let hits = mem.recall("the sky is blue", 5).await.expect("recall");
    assert!(
        hits.iter().any(|r| r.id == id && r.text == "the sky is blue"),
        "le souvenir mémorisé doit être retrouvé par son id"
    );
}

#[tokio::test]
async fn open_in_memory_rejects_empty_agent() {
    let err = Memory::open_in_memory("").await;
    assert!(
        matches!(err, Err(basemyai::MemoryError::MissingAgent)),
        "un agent_id vide doit être refusé (invariant d'isolation)"
    );
}

#[tokio::test]
async fn graph_facade_shares_agent_and_store() {
    let mem = Memory::open_in_memory("agent-g").await.expect("open in-memory");
    let graph = mem.graph();
    assert_eq!(graph.agent().as_str(), "agent-g");

    graph.add_entity("alice", "person", "Alice").await.expect("entity");
    graph.add_entity("acme", "org", "Acme").await.expect("entity");
    graph.add_edge("alice", "works_at", "acme", 1.0).await.expect("edge");

    let reached = graph.traverse("alice", 2).await.expect("traverse");
    assert!(
        reached.iter().any(|r| r.id == "acme"),
        "la traversée doit atteindre Acme depuis Alice"
    );
}
