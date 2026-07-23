//! Zero network : remember/recall ne nécessitent pas de socket une fois l'embedder fourni.

#[path = "../support/mod.rs"]
mod support;

use basemyai::{Memory, MemoryLayer};
use support::{FakeEmbedder, agent};

#[tokio::test]
async fn remember_recall_succeeds_with_blocked_proxy() {
    // Proxy invalide : toute tentative réseau échouerait immédiatement.
    unsafe {
        std::env::set_var("HTTP_PROXY", "http://127.0.0.1:9");
        std::env::set_var("HTTPS_PROXY", "http://127.0.0.1:9");
    }

    let store = std::sync::Arc::new(support::open_native_store());
    let mem = Memory::from_native_store(store, Box::new(FakeEmbedder), agent("offline-agent"))
        .await
        .expect("open memory");

    let id = mem
        .remember("offline memory works", MemoryLayer::Semantic)
        .await
        .expect("remember without network");

    let hits = mem.recall("offline memory", 5).await.expect("recall without network");

    assert!(hits.iter().any(|r| r.id == id));
}
