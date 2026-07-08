//! Tests d'intégration du wiring `MaintenanceWorker` (M0.2).
//! Vérifie que `ConsolidationTask` tourne correctement via l'interface
//! `MaintenanceTask` — GC/oubli adaptatif étaient les deux autres tâches
//! enregistrées ici avant ADR-032 (retrait de libSQL) ; elles n'ont pas
//! d'équivalent natif aujourd'hui (voir `crates/basemyai-cli/src/commands/
//! maintenance.rs`).

use std::sync::Arc;
use std::time::Duration;

use basemyai::{AgentId, ConsolidationTask, LlmInference, Memory, MemoryLayer};
use basemyai_core::{Embedder, MaintenanceTask, MaintenanceWorker, Result as CoreResult};
mod support;

const DIM: usize = 384;

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
        "fake"
    }

    fn dim(&self) -> usize {
        DIM
    }
}

struct FakeLlm;

#[async_trait::async_trait]
impl LlmInference for FakeLlm {
    async fn complete(&self, _prompt: &str) -> basemyai::Result<String> {
        Ok(r#"{"facts":["Alice travaille chez Acme"],"entities":[{"id":"alice","kind":"person","label":"Alice"},{"id":"acme","kind":"company","label":"Acme"}],"relations":[{"src":"alice","relation":"employeur","dst":"acme"}]}"#.to_string())
    }

    fn model_id(&self) -> &str {
        "fake-llm"
    }
}

fn agent(id: &str) -> AgentId {
    AgentId::new(id).expect("agent id non vide")
}

async fn open_memory(agent_id: &str) -> Memory {
    let store = Arc::new(support::open_native_store());
    Memory::from_native_store(store, Box::new(FakeEmbedder), agent(agent_id))
        .await
        .expect("open memory")
}

/// `ConsolidationTask::run` appelle effectivement `consolidate` sur la mémoire
/// interne.
#[tokio::test]
async fn consolidation_task_runs_via_maintenance_interface() {
    let mem = Arc::new(open_memory("a").await);

    mem.remember("Alice a rejoint Acme", MemoryLayer::Episodic)
        .await
        .expect("épisode");

    let task = ConsolidationTask::new(Arc::clone(&mem), Arc::new(FakeLlm));
    task.run().await.expect("run");

    // Le fait est consolidé en couche sémantique.
    let hits = mem.recall("Alice travaille chez Acme", 5).await.expect("recall");
    assert!(
        hits.iter().any(|r| r.layer == MemoryLayer::Semantic),
        "le fait consolidé doit être retrouvé en semantic"
    );
}

/// End-to-end : `ConsolidationTask` enregistrée dans un `MaintenanceWorker` et
/// déclenchée par la **boucle de fond** (`start`), pas par un appel direct à
/// `run`. Prouve que le wiring tient à travers `tokio::spawn` + `sleep`.
#[tokio::test]
async fn consolidation_runs_through_worker_background_loop() {
    let mem = Arc::new(open_memory("a").await);
    mem.remember("Alice a rejoint Acme", MemoryLayer::Episodic)
        .await
        .expect("épisode");

    // Intervalle court : la boucle déclenche la consolidation rapidement.
    MaintenanceWorker::new()
        .register(
            Duration::from_millis(40),
            Arc::new(ConsolidationTask::new(Arc::clone(&mem), Arc::new(FakeLlm))),
        )
        .start();

    // Laisse la boucle de fond s'exécuter au moins une fois.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let hits = mem.recall("Alice travaille chez Acme", 5).await.expect("recall");
    assert!(
        hits.iter().any(|r| r.layer == MemoryLayer::Semantic),
        "la boucle de maintenance doit avoir consolidé le fait en semantic"
    );
}
