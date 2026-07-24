//! Tests d'intégration export/import JSONL : roundtrip complet (souvenirs +
//! graphe + validité), idempotence, rejets de flux invalides.

use basemyai::temporal::Validity;
use basemyai::{AgentId, Memory, MemoryError, MemoryLayer};
use basemyai_core::{Embedder, Result};
#[path = "../support/mod.rs"]
mod support;

const DIM: usize = 384;

/// Embedder déterministe : projette un texte sur un vecteur stable (cf. memory.rs).
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

fn now() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).expect("clock").as_secs()).expect("fits i64")
}

async fn open_memory(agent_id: &str) -> Memory {
    let store = support::open_native_store();
    Memory::from_native_store(std::sync::Arc::new(store), Box::new(FakeEmbedder), agent(agent_id))
        .await
        .expect("open memory")
}

/// Peuple une mémoire : 2 souvenirs valides, 1 expiré, 2 entités, 1 relation.
async fn seed(mem: &Memory) {
    mem.remember("the sky is blue", MemoryLayer::Semantic)
        .await
        .expect("r1");
    mem.remember("invoice ACME-42 reference", MemoryLayer::Episodic)
        .await
        .expect("r2");
    let n = now();
    mem.remember_with(
        "expired token ZEBRA",
        MemoryLayer::Semantic,
        Validity {
            valid_from: n - 100,
            valid_until: Some(n - 10),
        },
    )
    .await
    .expect("r3 expired");

    let graph = mem.graph();
    graph.add_entity("e-alice", "person", "Alice").await.expect("entity 1");
    graph.add_entity("e-acme", "org", "Acme").await.expect("entity 2");
    graph
        .add_edge("e-alice", "works_at", "e-acme", 0.9)
        .await
        .expect("edge");
}

#[tokio::test]
async fn roundtrip_restores_memories_graph_and_validity() {
    let source = open_memory("a").await;
    seed(&source).await;

    let jsonl = source.export_jsonl().await.expect("export");
    assert!(
        jsonl.lines().count() >= 7,
        "en-tête + 3 souvenirs + 2 entités + 1 arête"
    );

    // Import dans une base neuve, sous un AUTRE agent : l'export est portable.
    let target = open_memory("b").await;
    let report = target.import_jsonl(&jsonl).await.expect("import");
    assert_eq!(report.memories, 3, "3 souvenirs importés (expiré inclus : backup)");
    assert_eq!(report.entities, 2);
    assert_eq!(report.edges, 1);
    assert_eq!(
        report.memories_skipped + report.entities_skipped + report.edges_skipped,
        0
    );

    // Recall sémantique : les valides remontent, l'expiré non.
    let hits = target.recall("the sky is blue", 5).await.expect("recall");
    assert!(hits.iter().any(|r| r.text == "the sky is blue"));
    let zebra = target.recall("expired token ZEBRA", 5).await.expect("recall expired");
    assert!(
        zebra.iter().all(|r| r.text != "expired token ZEBRA"),
        "la fenêtre de validité doit survivre au roundtrip"
    );

    // Le miroir FTS est reconstruit : un terme exact remonte en hybride.
    let acme = target.recall_hybrid("ACME-42", 5).await.expect("hybrid");
    assert!(acme.iter().any(|r| r.text == "invoice ACME-42 reference"));

    // Le graphe est restauré et traversable.
    let reached = target.graph().traverse("e-alice", 2).await.expect("traverse");
    assert!(
        reached.iter().any(|r| r.id == "e-acme"),
        "la relation works_at doit survivre au roundtrip"
    );

    // Les couches sont préservées.
    let stats = target.stats().await.expect("stats");
    assert_eq!(stats.episodic, 1);
    assert_eq!(stats.semantic, 1, "seul le semantic valide compte (l'expiré est exclu)");
}

#[tokio::test]
async fn import_is_idempotent() {
    let source = open_memory("a").await;
    seed(&source).await;
    let jsonl = source.export_jsonl().await.expect("export");

    let target = open_memory("a").await;
    target.import_jsonl(&jsonl).await.expect("import 1");
    let second = target.import_jsonl(&jsonl).await.expect("import 2");

    assert_eq!(second.memories, 0, "ré-import : rien de nouveau");
    assert_eq!(second.memories_skipped, 3);
    assert_eq!(second.entities_skipped, 2);
    assert_eq!(second.edges_skipped, 1);

    let stats = target.stats().await.expect("stats");
    assert_eq!(stats.total(), 2, "pas de doublon après double import");
}

#[tokio::test]
async fn import_rejects_invalid_streams() {
    let mem = open_memory("a").await;

    // Pas du JSON.
    let err = mem.import_jsonl("definitely not json").await.expect_err("garbage");
    assert!(matches!(err, MemoryError::Porting(_)), "got: {err:?}");

    // JSON valide mais pas d'en-tête.
    let no_header = r#"{"type":"memory","id":"x","layer":"semantic","content":"hi","valid_from":0}"#;
    let err = mem.import_jsonl(no_header).await.expect_err("no header");
    assert!(matches!(err, MemoryError::Porting(_)), "got: {err:?}");

    // En-tête d'une version future.
    let future = r#"{"type":"header","format":"basemyai-export","version":99,"agent_id":"a","embedding_model":"m","embedding_dim":384,"exported_at":0}"#;
    let err = mem.import_jsonl(future).await.expect_err("future version");
    assert!(matches!(err, MemoryError::Porting(_)), "got: {err:?}");

    // Rien ne doit avoir été écrit.
    assert_eq!(mem.stats().await.expect("stats").total(), 0);
}

#[tokio::test]
async fn export_of_empty_memory_is_header_only() {
    let mem = open_memory("a").await;
    let jsonl = mem.export_jsonl().await.expect("export");
    let lines: Vec<_> = jsonl.lines().collect();
    assert_eq!(lines.len(), 1, "mémoire vide => en-tête seul");
    assert!(lines[0].contains("\"basemyai-export\""));

    // Et un tel export se réimporte sans bruit.
    let report = mem.import_jsonl(&jsonl).await.expect("import");
    assert_eq!(report, basemyai::ImportReport::default());
}
