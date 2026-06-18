//! Tests d'intégration de la couche mémoire : roundtrip remember/recall,
//! isolation par agent, expiration temporelle. Embedder fake déterministe.

use basemyai::temporal::Validity;
use basemyai::{AgentId, AgentStats, Memory, MemoryLayer};
use basemyai_core::libsql;
use basemyai_core::{Embedder, Result, Store};

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

#[tokio::test]
async fn remember_then_recall_returns_item() {
    let store = Store::open_in_memory().await.expect("open");
    let mem = Memory::open(store, Box::new(FakeEmbedder), agent("a"))
        .await
        .expect("open memory");

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

    let store = Store::open_in_memory().await.expect("open");
    let mem = Memory::open(store, Box::new(FakeEmbedder), agent("a"))
        .await
        .expect("open memory");

    mem.remember("the sky is blue", MemoryLayer::Semantic)
        .await
        .expect("remember 1");
    mem.remember("grass is green", MemoryLayer::Semantic)
        .await
        .expect("remember 2");

    for metric in [Metric::Euclidean, Metric::Hamming, Metric::Cosine] {
        let hits = mem
            .recall_with_metric("the sky is blue", 5, metric)
            .await
            .expect("recall");
        assert!(
            hits.iter().any(|r| r.text == "the sky is blue"),
            "metric {metric:?} doit retrouver l'item exact"
        );
    }
}

#[tokio::test]
async fn recall_hybrid_surfaces_exact_keyword_match() {
    let store = Store::open_in_memory().await.expect("open");
    let mem = Memory::open(store, Box::new(FakeEmbedder), agent("a"))
        .await
        .expect("open memory");

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
    let store = Store::open_in_memory().await.expect("open");
    let mem = Memory::open(store, Box::new(FakeEmbedder), agent("a"))
        .await
        .expect("open memory");

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
    let store = Store::open_in_memory().await.expect("open");
    let mem = Memory::open(store, Box::new(FakeEmbedder), agent("a"))
        .await
        .expect("open memory");

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
    let store = Store::open_in_memory().await.expect("open");
    let mem = Memory::open(store, Box::new(FakeEmbedder), agent("a"))
        .await
        .expect("open memory");

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
    let store = Store::open_in_memory().await.expect("open");
    let mem = Memory::open(store, Box::new(FakeEmbedder), agent("a"))
        .await
        .expect("open memory");

    let ids = mem
        .remember_batch(&[], MemoryLayer::Semantic)
        .await
        .expect("remember_batch vide");
    assert!(ids.is_empty(), "lot vide => aucun id");
    assert_eq!(mem.stats().await.expect("stats").total(), 0);
}

#[tokio::test]
async fn concurrent_remembers_serialize_without_error() {
    // Les écritures sont transactionnelles sur une connexion partagée : le
    // verrou writer du Store doit sérialiser les transactions concurrentes
    // (sans lui, le second BEGIN imbriqué échouerait).
    let store = Store::open_in_memory().await.expect("open");
    let mem = Memory::open(store, Box::new(FakeEmbedder), agent("a"))
        .await
        .expect("open memory");

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
    // Agent A mémorise dans une base, on copie son contenu dans la base de B
    // n'est pas possible (in-memory) : on vérifie plutôt que, partageant la
    // MÊME base, B (recall borné à `agent_id = 'B'`) ne voit pas les lignes de A.
    let store = Store::open_in_memory().await.expect("open");
    store.migrate(&basemyai::schema()).await.expect("migrate");

    // Insère un item de l'agent A directement (même base, connexion partagée).
    let vec_a = FakeEmbedder::vec_for("secret of agent A");
    let lit = format!("[{}]", vec_a.iter().map(f32::to_string).collect::<Vec<_>>().join(","));
    let conn = store.connect();
    conn.execute(
        "INSERT INTO memory (id, agent_id, layer, content, valid_from, valid_until, emb) \
         VALUES (?1, 'A', 'semantic', 'secret of agent A', 0, NULL, vector(?2))",
        basemyai_core::libsql::params!["row-a", lit],
    )
    .await
    .expect("insert A");

    // Mémoire bornée à l'agent B sur la MÊME base.
    let mem_b = Memory::new(store, Box::new(FakeEmbedder), agent("B"));
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
    let store = Store::open_in_memory().await.expect("open");
    let mem = Memory::open(store, Box::new(FakeEmbedder), agent("a"))
        .await
        .expect("open memory");

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
    let store = Store::open_in_memory().await.expect("open");
    let conn = store.connect();
    let mem = Memory::open(store, Box::new(FakeEmbedder), agent("a"))
        .await
        .expect("open memory");

    mem.remember("traceable fact", MemoryLayer::Semantic)
        .await
        .expect("remember");

    // Avant recall : last_access doit être NULL.
    let mut rows = conn
        .query(
            "SELECT last_access FROM memory WHERE content = ?1",
            libsql::params!["traceable fact"],
        )
        .await
        .expect("query before recall");
    let row = rows.next().await.expect("next").expect("row");
    let val: libsql::Value = row.get(0).expect("get");
    assert!(
        matches!(val, libsql::Value::Null),
        "last_access doit être NULL avant recall"
    );

    let hits = mem.recall("traceable fact", 5).await.expect("recall");
    assert!(!hits.is_empty(), "recall doit trouver l'item");

    // Après recall : last_access doit être renseigné.
    let mut rows = conn
        .query(
            "SELECT last_access FROM memory WHERE content = ?1",
            libsql::params!["traceable fact"],
        )
        .await
        .expect("query after recall");
    let row = rows.next().await.expect("next").expect("row");
    let val: libsql::Value = row.get(0).expect("get");
    assert!(
        matches!(val, libsql::Value::Integer(_)),
        "last_access doit être défini après recall"
    );
}

#[tokio::test]
async fn recall_by_layer_filters_correctly() {
    let store = Store::open_in_memory().await.expect("open");
    let mem = Memory::open(store, Box::new(FakeEmbedder), agent("a"))
        .await
        .expect("open memory");

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
    let store = Store::open_in_memory().await.expect("open");
    let mem = Memory::open(store, Box::new(FakeEmbedder), agent("a"))
        .await
        .expect("open memory");

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
    let store = Store::open_in_memory().await.expect("open");
    let conn = store.connect();
    let mem = Memory::open(store, Box::new(FakeEmbedder), agent("a"))
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
    let mut rows = conn
        .query("SELECT COUNT(*) FROM memory WHERE id = ?1", libsql::params![id])
        .await
        .expect("count query");
    let row = rows.next().await.expect("next").expect("row");
    let count: i64 = row.get(0).expect("count");
    assert_eq!(count, 0, "forget doit supprimer physiquement la ligne");
}

#[tokio::test]
async fn stats_counts_per_layer() {
    let store = Store::open_in_memory().await.expect("open");
    let mem = Memory::open(store, Box::new(FakeEmbedder), agent("a"))
        .await
        .expect("open memory");

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
async fn search_graph_scoped_to_entity_mentions() {
    let store = Store::open_in_memory().await.expect("open");
    let conn = store.connect();
    let mem = Memory::open(store, Box::new(FakeEmbedder), agent("a"))
        .await
        .expect("open memory");

    // Souvenir mentionnant une entité du graphe ("Alice").
    mem.remember("Alice works at Acme", MemoryLayer::Semantic)
        .await
        .expect("remember entity");
    // Souvenir sans entité connue.
    mem.remember("the sky is blue", MemoryLayer::Semantic)
        .await
        .expect("remember other");

    // Ajoute l'entité "Alice" au graphe via SQL direct (conn partagée).
    conn.execute(
        "INSERT INTO entity (id, agent_id, kind, label, valid_from, valid_until, importance) \
         VALUES (?1, ?2, ?3, ?4, ?5, NULL, 0)",
        libsql::params!["e-alice", "a", "person", "Alice", 0_i64],
    )
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
