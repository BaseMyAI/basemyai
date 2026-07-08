//! Tests d'intégration de la consolidation (VISION §5.1). Fournisseur LLM **fake
//! déterministe** (zéro réseau) renvoyant une extraction JSON canned : on vérifie
//! la promotion des faits en `semantic`, le peuplement du graphe, et la
//! déduplication (relancer ne duplique pas).

use std::sync::Arc;

use basemyai::{AgentId, LlmInference, Memory, MemoryEventKind, MemoryLayer, consolidate};
use basemyai_core::{Embedder, Result as CoreResult};
mod support;

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

async fn open_memory(agent_id: &str) -> Memory {
    let store = Arc::new(support::open_native_store());
    Memory::from_native_store(store, Box::new(FakeEmbedder), agent(agent_id))
        .await
        .expect("open memory")
}

#[tokio::test]
async fn consolidates_episodes_into_facts_and_graph() {
    let mem = open_memory("a").await;

    mem.remember("Alice a rejoint Acme en mars", MemoryLayer::Episodic)
        .await
        .expect("ep1");
    mem.remember("Acme a racheté Beta", MemoryLayer::Episodic)
        .await
        .expect("ep2");

    let report = consolidate(&mem, &FakeLlm).await.expect("consolidate");
    assert_eq!(report.episodes_seen, 2);
    assert_eq!(report.facts_added, 1);
    assert_eq!(report.facts_skipped, 0);
    assert_eq!(report.entities_upserted, 2);
    assert_eq!(report.relations_upserted, 1);

    // Le fait est promu en `semantic` et redevient recherchable.
    let hits = mem.recall("Alice travaille chez Acme", 5).await.expect("recall");
    assert!(
        hits.iter()
            .any(|r| r.text == "Alice travaille chez Acme" && r.layer == MemoryLayer::Semantic),
        "le fait consolidé doit être retrouvé en couche sémantique"
    );

    // Le graphe est peuplé (2 entités, 1 arête) pour l'agent.
    let reached = mem.graph().traverse("alice", 1).await.expect("traverse");
    assert!(reached.iter().any(|r| r.id == "acme"), "l'arête employeur doit exister");
}

#[tokio::test]
async fn consolidation_is_idempotent() {
    let mem = open_memory("a").await;

    mem.remember("Alice a rejoint Acme", MemoryLayer::Episodic)
        .await
        .expect("ep");

    let first = consolidate(&mem, &FakeLlm).await.expect("first");
    assert_eq!(first.facts_added, 1);

    // Deuxième passe : le fait et les nœuds existent déjà → rien de dupliqué.
    let second = consolidate(&mem, &FakeLlm).await.expect("second");
    assert_eq!(second.facts_added, 0, "aucun fait nouveau");
    assert_eq!(second.facts_skipped, 1, "le fait identique est ignoré");

    // Un seul fait sémantique malgré les deux passes.
    let stats = mem.stats().await.expect("stats");
    assert_eq!(stats.semantic, 1);
}

/// `MemoryEventKind` distingue un fait promu par consolidation
/// (`Consolidated`) d'un souvenir mémorisé directement par l'agent
/// (`Remembered`) — audit sécurité (ADR-018), traçabilité de l'escalade de
/// confiance `episodic → semantic`.
#[tokio::test]
async fn consolidated_facts_are_tagged_with_consolidation_source() {
    let mem = open_memory("a").await;
    let mut watcher = mem.watch("a", None);

    // Souvenir direct de l'agent : événement `Remembered`.
    mem.remember("direct user memory", MemoryLayer::Semantic)
        .await
        .expect("direct remember");
    let direct_event = watcher.recv().await.expect("event for direct remember");
    assert_eq!(direct_event.kind, MemoryEventKind::Remembered);

    mem.remember("Alice a rejoint Acme en mars", MemoryLayer::Episodic)
        .await
        .expect("episode");
    let _episode_event = watcher.recv().await.expect("event for episode");

    consolidate(&mem, &FakeLlm).await.expect("consolidate");
    let consolidated_event = watcher.recv().await.expect("event for consolidated fact");
    assert_eq!(
        consolidated_event.kind,
        MemoryEventKind::Consolidated,
        "un fait promu par consolidation doit émettre l'événement Consolidated"
    );
}

/// Une reformulation très proche d'un fait déjà connu doit être détectée
/// comme doublon par similarité sémantique, pas seulement par contenu exact.
/// `HashEmbedder`/`FakeEmbedder` est une simple somme de bytes par position :
/// ajouter un seul caractère en fin de chaîne ne déplace presque pas le
/// vecteur résultant, donc la similarité cosinus avec l'original reste très
/// proche de 1.
#[tokio::test]
async fn near_duplicate_fact_is_detected_via_semantic_similarity() {
    let mem = open_memory("a").await;

    // Pré-existant : quasi identique au fait canned renvoyé par `FakeLlm`
    // ("Alice travaille chez Acme"), au caractère final près — pas le même
    // contenu exact, donc le check par égalité seule laisserait passer le
    // doublon.
    mem.remember("Alice travaille chez Acme.", MemoryLayer::Semantic)
        .await
        .expect("seed fact");

    mem.remember("Alice a rejoint Acme en mars", MemoryLayer::Episodic)
        .await
        .expect("episode");

    let report = consolidate(&mem, &FakeLlm).await.expect("consolidate");
    assert_eq!(
        report.facts_added, 0,
        "le fait quasi-identique déjà connu ne doit pas être promu une 2e fois"
    );
    assert_eq!(report.facts_skipped, 1, "il doit être compté comme doublon");

    let stats = mem.stats().await.expect("stats");
    assert_eq!(
        stats.semantic, 1,
        "un seul fait sémantique doit subsister malgré la reformulation"
    );
}

#[tokio::test]
async fn no_episodes_is_a_noop() {
    let mem = open_memory("a").await;

    // Aucun épisode : la consolidation ne touche pas au LLM et renvoie un rapport vide.
    let report = consolidate(&mem, &FakeLlm).await.expect("consolidate");
    assert_eq!(report, basemyai::ConsolidationReport::default());
}
