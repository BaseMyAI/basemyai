//! Graph traverse : un agent ne doit pas atteindre les entités d'un autre agent.

mod support;

use basemyai::{Memory, MemoryLayer};
use support::{FakeEmbedder, agent};

#[tokio::test]
async fn graph_traverse_does_not_cross_agents() {
    let store = std::sync::Arc::new(support::open_native_store());

    let mem_a = Memory::from_native_store(std::sync::Arc::clone(&store), Box::new(FakeEmbedder), agent("graph-a"))
        .await
        .expect("open A");
    let mem_b = Memory::from_native_store(store, Box::new(FakeEmbedder), agent("graph-b"))
        .await
        .expect("open B");

    mem_a
        .graph()
        .add_entity("node-a", "secret", "Agent A private node")
        .await
        .expect("entity A");
    mem_a
        .graph()
        .add_entity("leaf-a", "secret", "Agent A private leaf")
        .await
        .expect("leaf A");
    mem_a
        .graph()
        .add_edge("node-a", "points_to", "leaf-a", 1.0)
        .await
        .expect("edge A");

    mem_b
        .graph()
        .add_entity("node-b", "secret", "Agent B decoy")
        .await
        .expect("entity B");

    // Attaquant B connaît les ids de A et tente une traversée.
    let reached = mem_b
        .graph()
        .traverse("node-a", 3)
        .await
        .expect("traverse from foreign id");

    assert!(
        reached.is_empty(),
        "agent B must not traverse agent A graph nodes: {reached:?}"
    );

    let legit = mem_a.graph().traverse("node-a", 3).await.expect("traverse A");
    assert!(!legit.is_empty(), "agent A must reach its own graph");
}

#[tokio::test]
async fn search_graph_does_not_surface_other_agent_memories() {
    let store = std::sync::Arc::new(support::open_native_store());

    let mem_a = Memory::from_native_store(std::sync::Arc::clone(&store), Box::new(FakeEmbedder), agent("sg-a"))
        .await
        .expect("open A");
    let mem_b = Memory::from_native_store(store, Box::new(FakeEmbedder), agent("sg-b"))
        .await
        .expect("open B");

    mem_a
        .graph()
        .add_entity("acme", "org", "Acme Corp")
        .await
        .expect("entity");
    mem_a
        .remember("Alice works at Acme Corp", MemoryLayer::Semantic)
        .await
        .expect("remember with entity mention");

    mem_b
        .remember("Bob works at Acme Corp", MemoryLayer::Semantic)
        .await
        .expect("remember B");

    let hits = mem_b.search_graph("Acme Corp", 10).await.expect("search_graph B");
    assert!(
        hits.iter().all(|r| !r.text.contains("Alice")),
        "agent B search_graph must not return agent A memories: {hits:?}"
    );
}
