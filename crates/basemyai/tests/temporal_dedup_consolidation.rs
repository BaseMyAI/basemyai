//! `exact_fact_exists` respecte la validité temporelle (dédup consolidation).

mod support;

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use basemyai::storage::MemoryStore;
use basemyai::{MemoryLayer, Validity};
use support::{FakeEmbedder, agent};

fn unix_now() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before unix epoch")
            .as_secs(),
    )
    .expect("timestamp fits i64")
}

#[tokio::test]
async fn invalidated_semantic_fact_not_counted_as_existing() {
    let store = Arc::new(support::open_native_store());
    let mem_a = basemyai::Memory::from_native_store(Arc::clone(&store), Box::new(FakeEmbedder), agent("dedup-a"))
        .await
        .expect("open");

    let fact = "Alice works at Acme Corp";
    let at = unix_now();
    let id = mem_a
        .remember_with(fact, MemoryLayer::Semantic, Validity::since(at.saturating_sub(60)))
        .await
        .expect("remember fact");

    assert!(
        store
            .exact_fact_exists(mem_a.agent(), fact, at)
            .await
            .expect("exists before invalidate"),
        "fact valid before invalidate"
    );

    mem_a.invalidate(&id).await.expect("invalidate");

    let after = unix_now();
    assert!(
        !store
            .exact_fact_exists(mem_a.agent(), fact, after)
            .await
            .expect("exists after invalidate"),
        "invalidated fact must not block re-promotion via exact_fact_exists at consolidation time"
    );
}

#[tokio::test]
async fn expired_fact_not_counted_as_existing() {
    let store = Arc::new(support::open_native_store());
    let mem = basemyai::Memory::from_native_store(Arc::clone(&store), Box::new(FakeEmbedder), agent("dedup-exp"))
        .await
        .expect("open");

    let fact = "Temporary contract ends Friday";
    mem.remember_with(
        fact,
        MemoryLayer::Semantic,
        Validity {
            valid_from: 0,
            valid_until: Some(50),
        },
    )
    .await
    .expect("remember bounded");

    assert!(
        store
            .exact_fact_exists(mem.agent(), fact, 25)
            .await
            .expect("valid at 25"),
        "valid inside window"
    );
    assert!(
        !store
            .exact_fact_exists(mem.agent(), fact, 100)
            .await
            .expect("check at 100"),
        "expired fact must not count as existing"
    );
}
