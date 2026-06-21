//! Adversarial proof for live memory subscriptions (`Memory::watch`).
//!
//! Mirrors `p1_isolation_adversarial.rs`: the security-critical invariant is
//! that a subscription **never** delivers another agent's events, even when the
//! subscriber knows (or guesses) the other agent's id. Server-side isolation
//! lives inside `MemorySubscription::recv`, not in the caller.

use std::time::Duration;

use basemyai::{AgentId, Memory, MemoryEventKind, MemoryLayer};
use basemyai_core::{Embedder, Result, Store};

const DIM: usize = 384;

/// Deterministic, model-free embedder (same shape as the isolation suite).
struct FakeEmbedder;

impl FakeEmbedder {
    fn vec_for(text: &str) -> Vec<f32> {
        let mut v = vec![0.0_f32; DIM];
        for (i, b) in text.bytes().enumerate() {
            v[i % DIM] += f32::from(b) + 1.0;
        }
        v[0] += 1.0;
        v
    }
}

impl Embedder for FakeEmbedder {
    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        Ok(Self::vec_for(text))
    }

    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| Self::vec_for(t)).collect())
    }

    fn model_id(&self) -> &str {
        "fake-deterministic"
    }

    fn dim(&self) -> usize {
        DIM
    }
}

fn agent(id: &str) -> AgentId {
    AgentId::new(id).expect("non-empty agent id")
}

async fn open_memory(agent_id: &str) -> Memory {
    let store = Store::open_in_memory().await.expect("open in-memory store");
    Memory::open(store, Box::new(FakeEmbedder), agent(agent_id))
        .await
        .expect("open memory")
}

/// Short timeout helper: returns `None` if no event arrives in time.
async fn recv_soon(sub: &mut basemyai::MemorySubscription) -> Option<basemyai::MemoryEvent> {
    tokio::time::timeout(Duration::from_millis(300), sub.recv())
        .await
        .unwrap_or_default()
}

/// ISOLATION (critical): a subscriber for agent A must NOT receive agent B's
/// events, even sharing the same underlying database file. Then A's own write
/// is delivered, proving the channel is live and the filter is precise.
#[tokio::test]
async fn subscription_never_leaks_other_agents_events() {
    let path = temp_db_path("events-isolation");

    let store_a = Store::open(&path, None).await.expect("open A");
    store_a.migrate(&basemyai::schema()).await.expect("migrate A");
    let mem_a = Memory::new(store_a, Box::new(FakeEmbedder), agent("agent-a"));

    let store_b = Store::open(&path, None).await.expect("open B same db");
    store_b.migrate(&basemyai::schema()).await.expect("migrate B");
    // Hostile id mirroring the isolation suite — must not help cross the boundary.
    let mem_b = Memory::new(store_b, Box::new(FakeEmbedder), agent("agent-b' OR '1'='1"));

    let mut sub_a = mem_a.watch("agent-a", None);

    // Agent B writes: A's subscription must yield NOTHING.
    mem_b
        .remember("B private note", MemoryLayer::Semantic)
        .await
        .expect("B remembers");
    assert!(
        recv_soon(&mut sub_a).await.is_none(),
        "agent A subscription must not receive agent B's event"
    );

    // Agent A writes: A receives exactly that event.
    let id = mem_a
        .remember("A private note", MemoryLayer::Semantic)
        .await
        .expect("A remembers");
    let ev = recv_soon(&mut sub_a).await.expect("A receives its own event");
    assert_eq!(ev.agent_id, "agent-a");
    assert_eq!(ev.kind, MemoryEventKind::Remembered);
    assert_eq!(ev.layer, MemoryLayer::Semantic);
    assert_eq!(ev.id, id);
}

/// A subscriber that passes ANOTHER agent's id only ever sees THAT agent's
/// stream — it cannot escalate to events the `Memory` never emits for it.
#[tokio::test]
async fn watch_with_foreign_id_only_sees_that_foreign_agent() {
    let mem = open_memory("agent-a").await;
    // Subscribe as the wrong agent on agent-a's Memory (which only emits for A).
    let mut sub_foreign = mem.watch("someone-else", None);

    mem.remember("A note", MemoryLayer::Semantic)
        .await
        .expect("A remembers");
    assert!(
        recv_soon(&mut sub_foreign).await.is_none(),
        "a foreign-id subscription must not receive agent A's events"
    );
}

/// POSITIVE delivery per kind: remember→Remembered, invalidate→Invalidated,
/// forget→Forgotten, each carrying the affected id.
#[tokio::test]
async fn delivers_each_event_kind_with_correct_id() {
    let mem = open_memory("agent-a").await;
    let mut sub = mem.watch("agent-a", None);

    let id = mem
        .remember("a memory to mutate", MemoryLayer::Episodic)
        .await
        .expect("remember");
    let ev = recv_soon(&mut sub).await.expect("remembered event");
    assert_eq!(ev.kind, MemoryEventKind::Remembered);
    assert_eq!(ev.id, id);
    assert_eq!(ev.layer, MemoryLayer::Episodic);

    mem.invalidate(&id).await.expect("invalidate");
    let ev = recv_soon(&mut sub).await.expect("invalidated event");
    assert_eq!(ev.kind, MemoryEventKind::Invalidated);
    assert_eq!(ev.id, id);

    mem.forget(&id).await.expect("forget");
    let ev = recv_soon(&mut sub).await.expect("forgotten event");
    assert_eq!(ev.kind, MemoryEventKind::Forgotten);
    assert_eq!(ev.id, id);
}

/// A batch remember emits one event per inserted record.
#[tokio::test]
async fn batch_remember_emits_one_event_per_record() {
    let mem = open_memory("agent-a").await;
    let mut sub = mem.watch("agent-a", None);

    let texts = vec!["one".to_string(), "two".to_string(), "three".to_string()];
    let ids = mem
        .remember_batch(&texts, MemoryLayer::Semantic)
        .await
        .expect("batch remember");

    for expected in &ids {
        let ev = recv_soon(&mut sub).await.expect("batch event");
        assert_eq!(ev.kind, MemoryEventKind::Remembered);
        assert_eq!(&ev.id, expected);
    }
}

/// LAYER FILTER: a subscription scoped to `semantic` ignores a `short_term`
/// write but receives the next `semantic` one.
#[tokio::test]
async fn layer_filter_drops_other_layers() {
    let mem = open_memory("agent-a").await;
    let mut sub = mem.watch("agent-a", Some(MemoryLayer::Semantic));

    mem.remember("transient", MemoryLayer::ShortTerm)
        .await
        .expect("short-term remember");
    assert!(
        recv_soon(&mut sub).await.is_none(),
        "a semantic-only subscription must ignore a short_term event"
    );

    let id = mem
        .remember("durable fact", MemoryLayer::Semantic)
        .await
        .expect("semantic remember");
    let ev = recv_soon(&mut sub).await.expect("semantic event passes the filter");
    assert_eq!(ev.layer, MemoryLayer::Semantic);
    assert_eq!(ev.id, id);
}

/// LAGGED tolerance: flood the channel past its capacity (1024) WITHOUT
/// receiving, then assert `recv` still returns a valid (later) event rather
/// than erroring or panicking. The internal `RecvError::Lagged` branch keeps
/// the loop going.
#[tokio::test]
async fn recv_tolerates_lag_and_keeps_delivering() {
    let mem = open_memory("agent-a").await;
    let mut sub = mem.watch("agent-a", None);

    // Send more than the broadcast capacity (DEFAULT_EVENT_CAPACITY = 1024)
    // without draining the subscription, forcing the ring buffer to wrap.
    let flood = 1024usize + 50;
    let mut last_id = String::new();
    for i in 0..flood {
        last_id = mem
            .remember(&format!("flood-{i}"), MemoryLayer::Semantic)
            .await
            .expect("flood remember");
    }

    // Despite the lag, recv must yield a valid, still-buffered (later) event.
    let ev = recv_soon(&mut sub).await.expect("recv survives lag");
    assert_eq!(ev.kind, MemoryEventKind::Remembered);
    assert!(
        !ev.id.is_empty(),
        "a real event id is delivered after lag, not an error"
    );

    // And the channel is still live: a subsequent write is delivered too.
    let after = mem
        .remember("after the flood", MemoryLayer::Semantic)
        .await
        .expect("post-flood remember");
    // Drain whatever remains until we reach the post-flood event.
    let mut saw_after = ev.id == after || last_id == after;
    while !saw_after {
        match recv_soon(&mut sub).await {
            Some(e) if e.id == after => saw_after = true,
            Some(_) => continue,
            None => break,
        }
    }
    assert!(saw_after, "channel stays live and eventually delivers the post-flood event");
}

/// NO-SUBSCRIBER safety: a remember with zero live subscribers succeeds (the
/// best-effort send error is ignored, never propagated).
#[tokio::test]
async fn remember_with_no_subscriber_succeeds() {
    let mem = open_memory("agent-a").await;
    // No watch() call: the broadcast has no live receiver.
    let id = mem
        .remember("nobody is listening", MemoryLayer::Semantic)
        .await
        .expect("remember succeeds with no subscriber");
    assert!(!id.is_empty());
}

fn current_unix() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).expect("clock").as_secs()).expect("fits i64")
}

fn temp_db_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("basemyai-{name}-{}-{}.db", std::process::id(), current_unix()))
}
