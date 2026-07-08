//! Tests de **contrat** pour [`basemyai::storage::MemoryStore`] (suivi
//! ADR-019/ADR-020 : « add backend contract tests before any second backend
//! exists »). Pilotés par le trait directement, **pas** par `Memory` — pour
//! qu'ils restent valides verbatim contre une future seconde implémentation
//! (aujourd'hui, [`NativeMemoryStore`] est l'unique backend, ADR-032).

use basemyai::storage::{MemoryStore, NativeMemoryStore, NewMemory};
use basemyai::temporal::Validity;
use basemyai::{AgentId, MemoryLayer};
use basemyai_core::Metric;
mod support;

fn agent(id: &str) -> AgentId {
    AgentId::new(id).expect("non-empty agent id")
}

async fn engine() -> NativeMemoryStore {
    support::open_native_store()
}

/// Vecteur déterministe à la dimension du schéma (`EMBEDDING_DIM`) : deux
/// graines identiques donnent le même vecteur.
fn vec_for(seed: u8) -> Vec<f32> {
    let dim = basemyai::EMBEDDING_DIM;
    let mut v = vec![0.0_f32; dim];
    v[usize::from(seed) % dim] = 1.0;
    v[0] += 0.001; // évite le vecteur nul même quand seed % dim == 0
    v
}

#[tokio::test]
async fn put_memory_then_recall_vector_roundtrips() {
    let e = engine().await;
    let a = agent("a");
    e.put_memory(
        "m1",
        &a,
        MemoryLayer::Episodic,
        "bonjour",
        Validity::since(0),
        &vec_for(1),
        "user",
    )
    .await
    .expect("put");

    let got = e
        .recall_vector(&a, &vec_for(1), 5, None, Metric::Cosine, 0)
        .await
        .expect("recall");
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].id, "m1");
    assert_eq!(got[0].text, "bonjour");
    assert_eq!(got[0].layer, MemoryLayer::Episodic);
}

#[tokio::test]
async fn recall_vector_is_isolated_per_agent() {
    let e = engine().await;
    e.put_memory(
        "m1",
        &agent("a"),
        MemoryLayer::Episodic,
        "secret de A",
        Validity::since(0),
        &vec_for(1),
        "user",
    )
    .await
    .expect("put a");
    e.put_memory(
        "m2",
        &agent("b"),
        MemoryLayer::Episodic,
        "secret de B",
        Validity::since(0),
        &vec_for(1),
        "user",
    )
    .await
    .expect("put b");

    let seen_by_b = e
        .recall_vector(&agent("b"), &vec_for(1), 5, None, Metric::Cosine, 0)
        .await
        .expect("recall b");
    assert_eq!(seen_by_b.len(), 1);
    assert_eq!(seen_by_b[0].id, "m2");
}

#[tokio::test]
async fn recall_vector_excludes_expired_and_not_yet_valid() {
    let e = engine().await;
    let a = agent("a");
    let now = 1_000_i64;
    // Vecteurs distincts par ligne (pas de doublon exact) : on isole le
    // filtre temporel de tout artefact de l'index ANN sur des candidats
    // ex-aequo, avec seulement 3 lignes au total bien sous le facteur de
    // sur-échantillonnage (`KNN_OVERSAMPLE`), les trois restent candidates.
    e.put_memory(
        "expired",
        &a,
        MemoryLayer::Episodic,
        "périmé",
        Validity {
            valid_from: now - 100,
            valid_until: Some(now - 10),
        },
        &vec_for(1),
        "user",
    )
    .await
    .expect("put expired");
    e.put_memory(
        "future",
        &a,
        MemoryLayer::Episodic,
        "pas encore",
        Validity {
            valid_from: now + 100,
            valid_until: None,
        },
        &vec_for(2),
        "user",
    )
    .await
    .expect("put future");
    e.put_memory(
        "live",
        &a,
        MemoryLayer::Episodic,
        "vivant",
        Validity::since(now - 1),
        &vec_for(3),
        "user",
    )
    .await
    .expect("put live");

    let got = e
        .recall_vector(&a, &vec_for(3), 10, None, Metric::Cosine, now)
        .await
        .expect("recall");
    assert_eq!(got.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["live"]);
}

#[tokio::test]
async fn recall_vector_filters_by_layer() {
    let e = engine().await;
    let a = agent("a");
    e.put_memory(
        "ep",
        &a,
        MemoryLayer::Episodic,
        "un épisode",
        Validity::since(0),
        &vec_for(1),
        "user",
    )
    .await
    .expect("put episodic");
    e.put_memory(
        "sem",
        &a,
        MemoryLayer::Semantic,
        "un fait",
        Validity::since(0),
        &vec_for(1),
        "user",
    )
    .await
    .expect("put semantic");

    let got = e
        .recall_vector(&a, &vec_for(1), 10, Some(MemoryLayer::Semantic), Metric::Cosine, 0)
        .await
        .expect("recall semantic only");
    assert_eq!(got.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["sem"]);
}

#[tokio::test]
async fn put_memory_batch_is_atomic_and_ordered() {
    let e = engine().await;
    let a = agent("a");
    let (v1, v2) = (vec_for(1), vec_for(2));
    let items = vec![
        NewMemory {
            id: "b1".into(),
            layer: MemoryLayer::Episodic,
            text: "un",
            validity: Validity::since(0),
            vector: &v1,
            source: "user",
        },
        NewMemory {
            id: "b2".into(),
            layer: MemoryLayer::Episodic,
            text: "deux",
            validity: Validity::since(0),
            vector: &v2,
            source: "user",
        },
    ];
    e.put_memory_batch(&a, &items).await.expect("batch");

    let stats = e.agent_stats(&a, 0).await.expect("stats");
    assert_eq!(stats.episodic, 2);
}

#[tokio::test]
async fn put_memory_batch_empty_is_noop() {
    let e = engine().await;
    e.put_memory_batch(&agent("a"), &[]).await.expect("empty batch");
    let stats = e.agent_stats(&agent("a"), 0).await.expect("stats");
    assert_eq!(stats.total(), 0);
}

#[tokio::test]
async fn hydrate_preserves_order_and_skips_missing_ids() {
    let e = engine().await;
    let a = agent("a");
    e.put_memory(
        "m1",
        &a,
        MemoryLayer::Episodic,
        "premier",
        Validity::since(0),
        &vec_for(1),
        "user",
    )
    .await
    .expect("put m1");
    e.put_memory(
        "m2",
        &a,
        MemoryLayer::Episodic,
        "second",
        Validity::since(0),
        &vec_for(2),
        "user",
    )
    .await
    .expect("put m2");

    let ids = vec!["m2".to_string(), "missing".to_string(), "m1".to_string()];
    let got = e.hydrate(&a, &ids, 0).await.expect("hydrate");
    assert_eq!(got.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["m2", "m1"]);
}

#[tokio::test]
async fn hydrate_is_scoped_to_agent_even_when_id_is_known() {
    let e = engine().await;
    let a = agent("a");
    let b = agent("b");
    e.put_memory(
        "known-to-b",
        &a,
        MemoryLayer::Semantic,
        "secret de A",
        Validity::since(0),
        &vec_for(1),
        "user",
    )
    .await
    .expect("put a");

    let got = e
        .hydrate(&b, &["known-to-b".to_string()], 0)
        .await
        .expect("hydrate b with a id");
    assert!(got.is_empty(), "B ne doit pas hydrater un id appartenant a A");
}

#[tokio::test]
async fn invalidate_hides_from_recall_without_deleting() {
    let e = engine().await;
    let a = agent("a");
    e.put_memory(
        "m1",
        &a,
        MemoryLayer::Episodic,
        "x",
        Validity::since(0),
        &vec_for(1),
        "user",
    )
    .await
    .expect("put");

    e.invalidate(&a, "m1", 100).await.expect("invalidate");

    let got = e
        .recall_vector(&a, &vec_for(1), 5, None, Metric::Cosine, 100)
        .await
        .expect("recall after invalidate");
    assert!(got.is_empty());

    // Toujours présent à un instant antérieur à l'invalidation.
    let got_before = e
        .recall_vector(&a, &vec_for(1), 5, None, Metric::Cosine, 50)
        .await
        .expect("recall before");
    assert_eq!(got_before.len(), 1);
}

#[tokio::test]
async fn forget_deletes_physically() {
    let e = engine().await;
    let a = agent("a");
    e.put_memory(
        "m1",
        &a,
        MemoryLayer::Episodic,
        "x",
        Validity::since(0),
        &vec_for(1),
        "user",
    )
    .await
    .expect("put");

    e.forget(&a, "m1").await.expect("forget");

    let got = e.hydrate(&a, &["m1".to_string()], 0).await.expect("hydrate");
    assert!(got.is_empty(), "forget doit supprimer physiquement la ligne");
}

#[tokio::test]
async fn purge_agent_removes_memories_and_graph_only_for_that_agent() {
    let e = engine().await;
    e.put_memory(
        "m1",
        &agent("a"),
        MemoryLayer::Episodic,
        "x",
        Validity::since(0),
        &vec_for(1),
        "user",
    )
    .await
    .expect("put a");
    e.put_memory(
        "m2",
        &agent("b"),
        MemoryLayer::Episodic,
        "y",
        Validity::since(0),
        &vec_for(1),
        "user",
    )
    .await
    .expect("put b");
    e.graph_upsert_entity(&agent("a"), "e1", "thing", "E1", Validity::since(0))
        .await
        .expect("entity a");

    e.purge_agent(&agent("a")).await.expect("purge a");

    let stats_a = e.agent_stats(&agent("a"), 0).await.expect("stats a");
    assert_eq!(stats_a.total(), 0);
    let stats_b = e.agent_stats(&agent("b"), 0).await.expect("stats b");
    assert_eq!(stats_b.total(), 1);
}

#[tokio::test]
async fn agent_stats_counts_only_valid_memories_per_layer() {
    let e = engine().await;
    let a = agent("a");
    e.put_memory(
        "ep",
        &a,
        MemoryLayer::Episodic,
        "x",
        Validity::since(0),
        &vec_for(1),
        "user",
    )
    .await
    .expect("put episodic");
    e.put_memory(
        "sem",
        &a,
        MemoryLayer::Semantic,
        "y",
        Validity::since(0),
        &vec_for(2),
        "user",
    )
    .await
    .expect("put semantic");
    e.put_memory(
        "expired",
        &a,
        MemoryLayer::Semantic,
        "z",
        Validity {
            valid_from: 0,
            valid_until: Some(1),
        },
        &vec_for(3),
        "user",
    )
    .await
    .expect("put expired");

    let stats = e.agent_stats(&a, 100).await.expect("stats");
    assert_eq!(stats.episodic, 1);
    assert_eq!(stats.semantic, 1);
    assert_eq!(stats.total(), 2);
}

#[tokio::test]
async fn vector_and_keyword_ranking_ids_are_isolated_and_temporal() {
    let e = engine().await;
    let a = agent("a");
    e.put_memory(
        "m1",
        &a,
        MemoryLayer::Episodic,
        "le chat dort",
        Validity::since(0),
        &vec_for(1),
        "user",
    )
    .await
    .expect("put");
    e.put_memory(
        "other",
        &agent("b"),
        MemoryLayer::Episodic,
        "le chat dort",
        Validity::since(0),
        &vec_for(1),
        "user",
    )
    .await
    .expect("put other agent");

    let vec_ids = e
        .vector_ranking_ids(&a, &vec_for(1), 10, 0)
        .await
        .expect("vector ranking");
    assert_eq!(vec_ids, vec!["m1".to_string()]);

    let kw_ids = e
        .keyword_ranking_ids(&a, "\"chat\"", 10, 0)
        .await
        .expect("keyword ranking");
    assert_eq!(kw_ids, vec!["m1".to_string()]);
}

#[tokio::test]
async fn graph_upsert_and_traverse_is_idempotent_and_agent_scoped() {
    let e = engine().await;
    let a = agent("a");
    e.graph_upsert_entity(&a, "alice", "person", "Alice", Validity::since(0))
        .await
        .expect("alice");
    e.graph_upsert_entity(&a, "acme", "company", "Acme", Validity::since(0))
        .await
        .expect("acme");
    e.graph_upsert_edge(&a, "alice", "employeur", "acme", 1.0, 0)
        .await
        .expect("edge");
    // Idempotent : ré-upserter la même entité/arête ne duplique rien.
    e.graph_upsert_entity(&a, "alice", "person", "Alice", Validity::since(0))
        .await
        .expect("alice again");
    e.graph_upsert_edge(&a, "alice", "employeur", "acme", 1.0, 0)
        .await
        .expect("edge again");

    let reached = e.graph_traverse(&a, "alice", 1, 0).await.expect("traverse");
    assert_eq!(reached.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["acme"]);

    let reached_by_other = e
        .graph_traverse(&agent("b"), "alice", 1, 0)
        .await
        .expect("traverse other agent");
    assert!(reached_by_other.is_empty());
}

#[tokio::test]
async fn recent_episodes_returns_only_valid_episodic_layer_newest_first() {
    let e = engine().await;
    let a = agent("a");
    e.put_memory(
        "ep1",
        &a,
        MemoryLayer::Episodic,
        "premier épisode",
        Validity::since(10),
        &vec_for(1),
        "user",
    )
    .await
    .expect("put ep1");
    e.put_memory(
        "ep2",
        &a,
        MemoryLayer::Episodic,
        "second épisode",
        Validity::since(20),
        &vec_for(2),
        "user",
    )
    .await
    .expect("put ep2");
    e.put_memory(
        "sem",
        &a,
        MemoryLayer::Semantic,
        "un fait",
        Validity::since(15),
        &vec_for(3),
        "user",
    )
    .await
    .expect("put semantic");

    let episodes = e.recent_episodes(&a, 10, 100).await.expect("recent episodes");
    assert_eq!(
        episodes,
        vec!["second épisode".to_string(), "premier épisode".to_string()]
    );
}

#[tokio::test]
async fn exact_fact_exists_matches_only_semantic_layer_exact_content() {
    let e = engine().await;
    let a = agent("a");
    e.put_memory(
        "sem",
        &a,
        MemoryLayer::Semantic,
        "Alice travaille chez Acme",
        Validity::since(0),
        &vec_for(1),
        "consolidation",
    )
    .await
    .expect("put fact");
    e.put_memory(
        "ep",
        &a,
        MemoryLayer::Episodic,
        "Alice travaille chez Acme",
        Validity::since(0),
        &vec_for(2),
        "user",
    )
    .await
    .expect("put episode with same text");

    assert!(
        e.exact_fact_exists(&a, "Alice travaille chez Acme")
            .await
            .expect("exact match")
    );
    assert!(
        !e.exact_fact_exists(&a, "Bob travaille chez Beta")
            .await
            .expect("no match")
    );
    assert!(
        !e.exact_fact_exists(&agent("b"), "Alice travaille chez Acme")
            .await
            .expect("isolated by agent")
    );
}
