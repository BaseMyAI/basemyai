//! Tests d'intégration de la couche mémoire : roundtrip remember/recall,
//! isolation par agent, expiration temporelle. Embedder fake déterministe.

use std::sync::Arc;

use basemyai::temporal::Validity;
use basemyai::{AgentId, AgentStats, Memory, MemoryLayer};
use basemyai_core::{Embedder, Result};
mod support;

const DIM: usize = 384;

/// Embedder déterministe : projette un texte sur un vecteur stable. Des textes
/// identiques donnent des vecteurs identiques (distance cosine nulle).
struct FakeEmbedder;

impl FakeEmbedder {
    fn vec_for(text: &str) -> Vec<f32> {
        let mut v = vec![0.0_f32; DIM];
        for (i, b) in text.bytes().enumerate() {
            v[i % DIM] += f32::from(b) + 1.0;
        }
        // Garantit un vecteur non nul (cosine indéfini sur le vecteur nul).
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

fn now() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).expect("clock").as_secs()).expect("fits i64")
}

async fn open_memory(agent_id: &str) -> Memory {
    let store = Arc::new(support::open_native_store());
    Memory::from_native_store(store, Box::new(FakeEmbedder), agent(agent_id))
        .await
        .expect("open memory")
}

#[tokio::test]
async fn remember_then_recall_returns_item() {
    let mem = open_memory("a").await;

    mem.remember("the sky is blue", MemoryLayer::Semantic)
        .await
        .expect("remember");

    let hits = mem.recall("the sky is blue", 5).await.expect("recall");
    assert!(
        hits.iter().any(|r| r.text == "the sky is blue"),
        "recall doit retrouver l'item mémorisé"
    );
    assert_eq!(hits[0].layer, MemoryLayer::Semantic);
}

#[tokio::test]
async fn recall_with_metric_supports_euclidean_and_hamming() {
    use basemyai::Metric;

    let mem = open_memory("a").await;

    mem.remember("the sky is blue", MemoryLayer::Semantic)
        .await
        .expect("remember 1");
    mem.remember("grass is green", MemoryLayer::Semantic)
        .await
        .expect("remember 2");

    // Le backend natif ne re-classe que Cosine aujourd'hui (ADR-032) —
    // Euclidean/Hamming renvoient une erreur franche, jamais un faux résultat.
    for metric in [Metric::Euclidean, Metric::Hamming] {
        let err = mem
            .recall_with_metric("the sky is blue", 5, metric)
            .await
            .expect_err("metric non implémentée doit échouer franchement");
        assert!(matches!(err, basemyai::MemoryError::Core(_)));
    }
    let hits = mem
        .recall_with_metric("the sky is blue", 5, Metric::Cosine)
        .await
        .expect("recall cosine");
    assert!(hits.iter().any(|r| r.text == "the sky is blue"));
}

#[tokio::test]
async fn recall_hybrid_surfaces_exact_keyword_match() {
    let mem = open_memory("a").await;

    mem.remember("the quick brown fox jumps", MemoryLayer::Semantic)
        .await
        .expect("r1");
    let acme = mem
        .remember("invoice ACME-42 reference number", MemoryLayer::Semantic)
        .await
        .expect("r2");
    mem.remember("grass is green in spring", MemoryLayer::Semantic)
        .await
        .expect("r3");

    // Terme exact rare : c'est le signal BM25 qui doit le faire remonter.
    let hits = mem.recall_hybrid("ACME-42", 5).await.expect("hybrid");
    assert!(
        hits.iter().any(|r| r.id == acme),
        "la recherche hybride doit retrouver le match exact ACME-42 via BM25"
    );
}

#[tokio::test]
async fn recall_hybrid_respects_isolation_and_validity() {
    let mem = open_memory("a").await;

    let n = now();
    let expired = Validity {
        valid_from: n - 100,
        valid_until: Some(n - 10),
    };
    mem.remember_with("token ZEBRA expired", MemoryLayer::Semantic, expired)
        .await
        .expect("remember expired");

    let hits = mem.recall_hybrid("ZEBRA", 5).await.expect("hybrid");
    assert!(
        hits.iter().all(|r| r.text != "token ZEBRA expired"),
        "un souvenir expiré ne doit pas remonter, même par BM25"
    );
}

#[tokio::test]
async fn forget_removes_from_fts_mirror() {
    let mem = open_memory("a").await;

    let id = mem
        .remember("WIDGET unique identifier", MemoryLayer::Semantic)
        .await
        .expect("remember");
    mem.forget(&id).await.expect("forget");

    let hits = mem.recall_hybrid("WIDGET", 5).await.expect("hybrid");
    assert!(
        hits.is_empty(),
        "après forget, le miroir FTS ne doit plus matcher le souvenir"
    );
}

#[tokio::test]
async fn remember_batch_inserts_all_and_mirrors_fts() {
    let mem = open_memory("a").await;

    let texts = vec![
        "first batched fact".to_string(),
        "second batched fact".to_string(),
        "invoice BATCH-99 reference".to_string(),
    ];
    let ids = mem
        .remember_batch(&texts, MemoryLayer::Semantic)
        .await
        .expect("remember_batch");

    assert_eq!(ids.len(), 3, "un id par texte, dans l'ordre");
    let unique: std::collections::HashSet<_> = ids.iter().collect();
    assert_eq!(unique.len(), 3, "les ids doivent être uniques");

    let stats = mem.stats().await.expect("stats");
    assert_eq!(stats.semantic, 3, "les 3 souvenirs du lot doivent être visibles");

    // Le miroir FTS doit être peuplé : un terme exact du lot remonte par BM25.
    let hits = mem.recall_hybrid("BATCH-99", 5).await.expect("hybrid");
    assert!(
        hits.iter().any(|r| r.id == ids[2]),
        "le miroir FTS doit couvrir les souvenirs insérés par lot"
    );
}

#[tokio::test]
async fn remember_batch_empty_is_noop() {
    let mem = open_memory("a").await;

    let ids = mem
        .remember_batch(&[], MemoryLayer::Semantic)
        .await
        .expect("remember_batch vide");
    assert!(ids.is_empty(), "lot vide => aucun id");
    assert_eq!(mem.stats().await.expect("stats").total(), 0);
}

#[tokio::test]
async fn concurrent_remembers_serialize_without_error() {
    // Les écritures sont sérialisées côté moteur (mono-écrivain, ADR-025) :
    // des `remember`/`remember_batch` concurrents doivent tous aboutir.
    let mem = open_memory("a").await;

    let batch = ["concurrent three".to_string(), "concurrent four".to_string()];
    let (r1, r2, r3) = tokio::join!(
        mem.remember("concurrent one", MemoryLayer::Semantic),
        mem.remember("concurrent two", MemoryLayer::Semantic),
        mem.remember_batch(&batch, MemoryLayer::Semantic),
    );
    r1.expect("remember 1");
    r2.expect("remember 2");
    r3.expect("batch");

    assert_eq!(
        mem.stats().await.expect("stats").semantic,
        4,
        "les 4 écritures concurrentes doivent toutes aboutir"
    );
}

#[tokio::test]
async fn isolation_hides_other_agents_items() {
    // Deux agents sur la MÊME instance de store natif (partagée, comme un
    // provider REST/MCP) : B ne doit jamais voir les souvenirs de A.
    let store = Arc::new(support::open_native_store());
    let mem_a = Memory::from_native_store(Arc::clone(&store), Box::new(FakeEmbedder), agent("A"))
        .await
        .expect("open memory A");
    mem_a
        .remember("secret of agent A", MemoryLayer::Semantic)
        .await
        .expect("A remembers");

    let mem_b = Memory::from_native_store(Arc::clone(&store), Box::new(FakeEmbedder), agent("B"))
        .await
        .expect("open memory B");
    mem_b
        .remember("public note of B", MemoryLayer::Semantic)
        .await
        .expect("B remembers");

    let hits = mem_b.recall("secret of agent A", 5).await.expect("B recalls");
    assert!(
        hits.iter().all(|r| r.text != "secret of agent A"),
        "un item de l'agent A ne doit JAMAIS apparaître dans le recall de l'agent B"
    );
}

#[tokio::test]
async fn temporal_excludes_expired_items() {
    let mem = open_memory("a").await;

    let n = now();
    // valid_until dans le passé => expiré.
    let expired = Validity {
        valid_from: n - 100,
        valid_until: Some(n - 10),
    };
    mem.remember_with("stale fact", MemoryLayer::Semantic, expired)
        .await
        .expect("remember expired");

    // valide actuellement.
    let live = Validity {
        valid_from: n - 100,
        valid_until: Some(n + 10_000),
    };
    mem.remember_with("fresh fact", MemoryLayer::Semantic, live)
        .await
        .expect("remember live");

    let hits = mem.recall("stale fact", 5).await.expect("recall");
    assert!(
        hits.iter().all(|r| r.text != "stale fact"),
        "un item expiré ne doit pas apparaître"
    );

    let hits_live = mem.recall("fresh fact", 5).await.expect("recall live");
    assert!(
        hits_live.iter().any(|r| r.text == "fresh fact"),
        "un item valide doit apparaître"
    );
}

#[tokio::test]
async fn recall_updates_last_access() {
    let store = Arc::new(support::open_native_store());
    let mem = Memory::from_native_store(Arc::clone(&store), Box::new(FakeEmbedder), agent("a"))
        .await
        .expect("open memory");

    mem.remember("traceable fact", MemoryLayer::Semantic)
        .await
        .expect("remember");

    let last_access_of = |rows: &[(String, basemyai_engine::MemoryRecord)]| {
        rows.iter()
            .find(|(_, r)| r.content == "traceable fact")
            .expect("record present")
            .1
            .last_access
    };

    // Avant recall : `last_access` doit valoir `valid_from` (jamais accédé).
    let before = store.export_rows(&agent("a")).await.expect("export before recall");
    let before_access = last_access_of(&before.memories);
    let valid_from = before
        .memories
        .iter()
        .find(|(_, r)| r.content == "traceable fact")
        .unwrap()
        .1
        .valid_from;
    assert_eq!(
        before_access, valid_from,
        "last_access doit valoir valid_from avant recall"
    );

    let hits = mem.recall("traceable fact", 5).await.expect("recall");
    assert!(!hits.is_empty(), "recall doit trouver l'item");

    // Après recall : `last_access` doit avoir avancé.
    let after = store.export_rows(&agent("a")).await.expect("export after recall");
    let after_access = last_access_of(&after.memories);
    assert!(
        after_access >= before_access,
        "last_access doit être mis à jour après recall"
    );
}

#[tokio::test]
async fn recall_by_layer_filters_correctly() {
    let mem = open_memory("a").await;

    // Même texte dans deux couches différentes.
    mem.remember("layered content", MemoryLayer::Semantic)
        .await
        .expect("semantic");
    mem.remember("layered content", MemoryLayer::Episodic)
        .await
        .expect("episodic");

    let semantic_hits = mem
        .recall_by_layer("layered content", MemoryLayer::Semantic, 5)
        .await
        .expect("recall semantic");
    assert!(
        !semantic_hits.is_empty(),
        "recall_by_layer Semantic doit retourner des résultats"
    );
    assert!(
        semantic_hits.iter().all(|r| r.layer == MemoryLayer::Semantic),
        "recall_by_layer Semantic ne doit retourner que des souvenirs Semantic"
    );

    let episodic_hits = mem
        .recall_by_layer("layered content", MemoryLayer::Episodic, 5)
        .await
        .expect("recall episodic");
    assert!(
        !episodic_hits.is_empty(),
        "recall_by_layer Episodic doit retourner des résultats"
    );
    assert!(
        episodic_hits.iter().all(|r| r.layer == MemoryLayer::Episodic),
        "recall_by_layer Episodic ne doit retourner que des souvenirs Episodic"
    );
}

#[tokio::test]
async fn invalidate_hides_item_from_recall() {
    let mem = open_memory("a").await;

    mem.remember("to be invalidated", MemoryLayer::Semantic)
        .await
        .expect("remember");

    // Retrouve l'id via recall.
    let hits = mem
        .recall("to be invalidated", 5)
        .await
        .expect("recall before invalidate");
    let id = hits
        .iter()
        .find(|r| r.text == "to be invalidated")
        .expect("item must exist")
        .id
        .clone();

    mem.invalidate(&id).await.expect("invalidate");

    // Après invalidation, le recall ne doit plus le retourner.
    let hits_after = mem
        .recall("to be invalidated", 5)
        .await
        .expect("recall after invalidate");
    assert!(
        hits_after.iter().all(|r| r.text != "to be invalidated"),
        "un item invalidé ne doit plus apparaître dans recall"
    );
}

#[tokio::test]
async fn forget_removes_item_physically() {
    let store = Arc::new(support::open_native_store());
    let mem = Memory::from_native_store(Arc::clone(&store), Box::new(FakeEmbedder), agent("a"))
        .await
        .expect("open memory");

    mem.remember("to be forgotten", MemoryLayer::Semantic)
        .await
        .expect("remember");

    let hits = mem.recall("to be forgotten", 5).await.expect("recall before forget");
    let id = hits
        .iter()
        .find(|r| r.text == "to be forgotten")
        .expect("item must exist")
        .id
        .clone();

    mem.forget(&id).await.expect("forget");

    // Recall ne doit plus retourner l'item.
    let hits_after = mem.recall("to be forgotten", 5).await.expect("recall after forget");
    assert!(
        hits_after.iter().all(|r| r.text != "to be forgotten"),
        "un item oublié ne doit plus apparaître dans recall"
    );

    // La ligne doit être physiquement supprimée.
    let rows = store.export_rows(&agent("a")).await.expect("export after forget");
    assert!(
        rows.memories.iter().all(|(rid, _)| *rid != id),
        "forget doit supprimer physiquement l'enregistrement"
    );
}

#[tokio::test]
async fn stats_counts_per_layer() {
    let mem = open_memory("a").await;

    mem.remember("s1", MemoryLayer::Semantic).await.expect("s1");
    mem.remember("s2", MemoryLayer::Semantic).await.expect("s2");
    mem.remember("e1", MemoryLayer::Episodic).await.expect("e1");
    mem.remember("p1", MemoryLayer::Procedural).await.expect("p1");

    let s: AgentStats = mem.stats().await.expect("stats");
    assert_eq!(s.semantic, 2, "2 souvenirs semantic");
    assert_eq!(s.episodic, 1, "1 souvenir episodic");
    assert_eq!(s.procedural, 1, "1 souvenir procedural");
    assert_eq!(s.short_term, 0, "0 souvenir short_term");
    assert_eq!(s.total(), 4, "total = 4");
}

#[tokio::test]
async fn remember_rejects_text_over_max_len() {
    let mem = open_memory("a").await;

    let too_long = "x".repeat(basemyai::MAX_TEXT_LEN + 1);
    let err = mem
        .remember(&too_long, MemoryLayer::Semantic)
        .await
        .expect_err("un texte au-delà de MAX_TEXT_LEN doit être rejeté");
    match err {
        basemyai::MemoryError::TextTooLong { len, max } => {
            assert_eq!(len, basemyai::MAX_TEXT_LEN + 1);
            assert_eq!(max, basemyai::MAX_TEXT_LEN);
        }
        other => panic!("erreur attendue MemoryError::TextTooLong, obtenu {other:?}"),
    }
}

#[tokio::test]
async fn remember_accepts_text_at_exact_max_len() {
    let mem = open_memory("a").await;

    let at_limit = "x".repeat(basemyai::MAX_TEXT_LEN);
    mem.remember(&at_limit, MemoryLayer::Semantic)
        .await
        .expect("un texte exactement à la limite doit être accepté");
}

#[tokio::test]
async fn search_graph_scoped_to_entity_mentions() {
    let mem = open_memory("a").await;

    // Souvenir mentionnant une entité du graphe ("Alice").
    mem.remember("Alice works at Acme", MemoryLayer::Semantic)
        .await
        .expect("remember entity");
    // Souvenir sans entité connue.
    mem.remember("the sky is blue", MemoryLayer::Semantic)
        .await
        .expect("remember other");

    mem.graph()
        .add_entity("e-alice", "person", "Alice")
        .await
        .expect("insert entity");

    let hits = mem.search_graph("who works where?", 5).await.expect("search_graph");

    // Seul le souvenir mentionnant "Alice" doit apparaître.
    assert!(
        hits.iter().any(|r| r.text == "Alice works at Acme"),
        "search_graph doit trouver les souvenirs mentionnant des entités du graphe"
    );
    assert!(
        hits.iter().all(|r| r.text != "the sky is blue"),
        "search_graph ne doit pas retourner les souvenirs sans entité"
    );
}
