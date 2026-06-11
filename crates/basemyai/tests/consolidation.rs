//! Tests d'intégration de la consolidation (VISION §5.1). Fournisseur LLM **fake
//! déterministe** (zéro réseau) renvoyant une extraction JSON canned : on vérifie
//! la promotion des faits en `semantic`, le peuplement du graphe, et la
//! déduplication (relancer ne duplique pas).

use basemyai::{consolidate, AgentId, LlmInference, Memory, MemoryLayer};
use basemyai_core::libsql::Connection;
use basemyai_core::{Embedder, Result as CoreResult, Store};

const DIM: usize = 384;

/// Embedder déterministe (mêmes textes → mêmes vecteurs).
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
    fn embed(&self, text: &str) -> CoreResult<Vec<f32>> {
        Ok(Self::vec_for(text))
    }
    fn embed_batch(&self, texts: &[String]) -> CoreResult<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| Self::vec_for(t)).collect())
    }
    fn model_id(&self) -> &str {
        "fake-deterministic"
    }
    fn dim(&self) -> usize {
        DIM
    }
}

/// LLM fake : ignore le prompt, renvoie une extraction JSON fixe.
struct FakeLlm;

#[async_trait::async_trait]
impl LlmInference for FakeLlm {
    async fn complete(&self, _prompt: &str) -> basemyai::Result<String> {
        Ok(r#"{
            "facts": ["Alice travaille chez Acme"],
            "entities": [
                {"id": "alice", "kind": "person", "label": "Alice"},
                {"id": "acme", "kind": "company", "label": "Acme"}
            ],
            "relations": [
                {"src": "alice", "relation": "employeur", "dst": "acme"}
            ]
        }"#
        .to_string())
    }
    fn model_id(&self) -> &str {
        "fake-llm"
    }
}

fn agent(id: &str) -> AgentId {
    AgentId::new(id).expect("non-empty agent id")
}

async fn scalar_i64(conn: &Connection, sql: &str) -> i64 {
    let mut rows = conn.query(sql, ()).await.expect("query");
    let row = rows.next().await.expect("row").expect("une ligne");
    row.get::<i64>(0).expect("i64")
}

#[tokio::test]
async fn consolidates_episodes_into_facts_and_graph() {
    let store = Store::open_in_memory().await.expect("open");
    // Connexion gardée pour inspecter le graphe (base :memory: partagée).
    let conn = store.connect();
    let mem = Memory::open(store, Box::new(FakeEmbedder), agent("a")).await.expect("open memory");

    mem.remember("Alice a rejoint Acme en mars", MemoryLayer::Episodic).await.expect("ep1");
    mem.remember("Acme a racheté Beta", MemoryLayer::Episodic).await.expect("ep2");

    let report = consolidate(&mem, &FakeLlm).await.expect("consolidate");
    assert_eq!(report.episodes_seen, 2);
    assert_eq!(report.facts_added, 1);
    assert_eq!(report.facts_skipped, 0);
    assert_eq!(report.entities_upserted, 2);
    assert_eq!(report.relations_upserted, 1);

    // Le fait est promu en `semantic` et redevient recherchable.
    let hits = mem.recall("Alice travaille chez Acme", 5).await.expect("recall");
    assert!(
        hits.iter().any(|r| r.text == "Alice travaille chez Acme" && r.layer == MemoryLayer::Semantic),
        "le fait consolidé doit être retrouvé en couche sémantique"
    );

    // Le graphe est peuplé (2 entités, 1 arête) pour l'agent.
    assert_eq!(scalar_i64(&conn, "SELECT COUNT(*) FROM entity WHERE agent_id = 'a'").await, 2);
    assert_eq!(scalar_i64(&conn, "SELECT COUNT(*) FROM edge WHERE agent_id = 'a'").await, 1);
}

#[tokio::test]
async fn consolidation_is_idempotent() {
    let store = Store::open_in_memory().await.expect("open");
    let conn = store.connect();
    let mem = Memory::open(store, Box::new(FakeEmbedder), agent("a")).await.expect("open memory");

    mem.remember("Alice a rejoint Acme", MemoryLayer::Episodic).await.expect("ep");

    let first = consolidate(&mem, &FakeLlm).await.expect("first");
    assert_eq!(first.facts_added, 1);

    // Deuxième passe : le fait et les nœuds existent déjà → rien de dupliqué.
    let second = consolidate(&mem, &FakeLlm).await.expect("second");
    assert_eq!(second.facts_added, 0, "aucun fait nouveau");
    assert_eq!(second.facts_skipped, 1, "le fait identique est ignoré");

    // Un seul fait sémantique, deux entités, une arête malgré les deux passes.
    assert_eq!(
        scalar_i64(&conn, "SELECT COUNT(*) FROM memory WHERE agent_id='a' AND layer='semantic'").await,
        1
    );
    assert_eq!(scalar_i64(&conn, "SELECT COUNT(*) FROM entity WHERE agent_id='a'").await, 2);
    assert_eq!(scalar_i64(&conn, "SELECT COUNT(*) FROM edge WHERE agent_id='a'").await, 1);
}

#[tokio::test]
async fn no_episodes_is_a_noop() {
    let store = Store::open_in_memory().await.expect("open");
    let mem = Memory::open(store, Box::new(FakeEmbedder), agent("a")).await.expect("open memory");

    // Aucun épisode : la consolidation ne touche pas au LLM et renvoie un rapport vide.
    let report = consolidate(&mem, &FakeLlm).await.expect("consolidate");
    assert_eq!(report, basemyai::ConsolidationReport::default());
}
