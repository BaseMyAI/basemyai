//! Export JSONL : agent A ne doit jamais exporter les souvenirs de agent B.

mod support;

use basemyai::{Memory, MemoryLayer};
use support::{FakeEmbedder, agent};

#[tokio::test]
async fn export_jsonl_contains_only_owning_agent_memories() {
    let store = std::sync::Arc::new(support::open_native_store());

    let mem_a = Memory::from_native_store(
        std::sync::Arc::clone(&store),
        Box::new(FakeEmbedder),
        agent("export-agent-a"),
    )
    .await
    .expect("open A");
    let mem_b = Memory::from_native_store(store, Box::new(FakeEmbedder), agent("export-agent-b"))
        .await
        .expect("open B");

    mem_a
        .remember("SECRET-A-ONLY-TOKEN", MemoryLayer::Semantic)
        .await
        .expect("remember A");
    mem_b
        .remember("SECRET-B-ONLY-TOKEN", MemoryLayer::Semantic)
        .await
        .expect("remember B");

    let jsonl = mem_a.export_jsonl().await.expect("export A");

    assert!(
        jsonl.contains("SECRET-A-ONLY-TOKEN"),
        "export must include agent A data"
    );
    assert!(
        !jsonl.contains("SECRET-B-ONLY-TOKEN"),
        "export must not leak agent B data: {jsonl}"
    );
    assert!(
        jsonl.contains("\"agent_id\":\"export-agent-a\""),
        "header must name exporting agent"
    );
}
