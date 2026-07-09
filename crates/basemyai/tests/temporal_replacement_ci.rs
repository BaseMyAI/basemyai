//! Port CI de `examples/temporal_replacement.rs` — invalidation + recall.

mod support;

use basemyai::{Memory, MemoryLayer};
use support::{FakeEmbedder, agent};

#[tokio::test]
async fn invalidated_fact_not_recalled_hybrid() {
    let store = std::sync::Arc::new(support::open_native_store());
    let memory = Memory::from_native_store(store, Box::new(FakeEmbedder), agent("temporal-ci"))
        .await
        .expect("open memory");

    let old_id = memory
        .remember("The user is on the Free billing plan.", MemoryLayer::Semantic)
        .await
        .expect("remember old");

    memory.invalidate(&old_id).await.expect("invalidate old");

    memory
        .remember("The user is on the Pro billing plan.", MemoryLayer::Semantic)
        .await
        .expect("remember new");

    let hits = memory
        .recall_hybrid("current billing plan", 5)
        .await
        .expect("recall hybrid");

    assert!(
        hits.iter().any(|r| r.text.contains("Pro billing plan")),
        "current fact should be recalled: {hits:?}"
    );
    assert!(
        hits.iter().all(|r| !r.text.contains("Free billing plan")),
        "invalidated fact must not be recalled: {hits:?}"
    );
}
