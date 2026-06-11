//! Tests d'intégration de la couche mémoire : roundtrip remember/recall,
//! isolation par agent, expiration temporelle. Embedder fake déterministe.

use basemyai::temporal::Validity;
use basemyai::{AgentId, Memory, MemoryLayer};
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
    let mem = Memory::open(store, Box::new(FakeEmbedder), agent("a")).await.expect("open memory");

    mem.remember("the sky is blue", MemoryLayer::Semantic).await.expect("remember");

    let hits = mem.recall("the sky is blue", 5).await.expect("recall");
    assert!(hits.iter().any(|r| r.text == "the sky is blue"), "recall doit retrouver l'item mémorisé");
    assert_eq!(hits[0].layer, MemoryLayer::Semantic);
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
    mem_b.remember("public note of B", MemoryLayer::Semantic).await.expect("B remembers");

    let hits = mem_b.recall("secret of agent A", 5).await.expect("B recalls");
    assert!(
        hits.iter().all(|r| r.text != "secret of agent A"),
        "un item de l'agent A ne doit JAMAIS apparaître dans le recall de l'agent B"
    );
}

#[tokio::test]
async fn temporal_excludes_expired_items() {
    let store = Store::open_in_memory().await.expect("open");
    let mem = Memory::open(store, Box::new(FakeEmbedder), agent("a")).await.expect("open memory");

    let n = now();
    // valid_until dans le passé => expiré.
    let expired = Validity { valid_from: n - 100, valid_until: Some(n - 10) };
    mem.remember_with("stale fact", MemoryLayer::Semantic, expired).await.expect("remember expired");

    // valide actuellement.
    let live = Validity { valid_from: n - 100, valid_until: Some(n + 10_000) };
    mem.remember_with("fresh fact", MemoryLayer::Semantic, live).await.expect("remember live");

    let hits = mem.recall("stale fact", 5).await.expect("recall");
    assert!(hits.iter().all(|r| r.text != "stale fact"), "un item expiré ne doit pas apparaître");

    let hits_live = mem.recall("fresh fact", 5).await.expect("recall live");
    assert!(hits_live.iter().any(|r| r.text == "fresh fact"), "un item valide doit apparaître");
}
