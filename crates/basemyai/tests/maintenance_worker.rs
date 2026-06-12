//! Tests d'intégration du wiring MaintenanceWorker (M0.2).
//! Vérifie que `ConsolidationTask` tourne correctement via l'interface
//! `MaintenanceTask`, et que `AdaptiveForgetting` + `ExpiredMemoryGc`
//! s'enregistrent dans `MaintenanceWorker` sans modification.

use std::sync::Arc;
use std::time::Duration;

use basemyai::{AdaptiveForgetting, AgentId, ConsolidationTask, ExpiredMemoryGc, LlmInference, Memory, MemoryLayer};
use basemyai_core::{Embedder, MaintenanceTask, MaintenanceWorker, Result as CoreResult, Store};

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

/// `ConsolidationTask::run` appelle effectivement `consolidate` sur la mémoire
/// interne — le `_store` fourni est ignoré mais l'interface est respectée.
#[tokio::test]
async fn consolidation_task_runs_via_maintenance_interface() {
    let store = Store::open_in_memory().await.expect("open");
    let mem = Arc::new(
        Memory::open(store, Box::new(FakeEmbedder), agent("a"))
            .await
            .expect("open memory"),
    );

    mem.remember("Alice a rejoint Acme", MemoryLayer::Episodic)
        .await
        .expect("épisode");

    let task = ConsolidationTask::new(Arc::clone(&mem), Arc::new(FakeLlm));

    // Simule l'appel depuis le MaintenanceWorker : store passé en paramètre mais ignoré.
    let dummy = Store::open_in_memory().await.expect("dummy store");
    task.run(&dummy).await.expect("run");

    // Le fait est consolidé en couche sémantique.
    let hits = mem.recall("Alice travaille chez Acme", 5).await.expect("recall");
    assert!(
        hits.iter().any(|r| r.layer == MemoryLayer::Semantic),
        "le fait consolidé doit être retrouvé en semantic"
    );
}

/// Vérification à la compilation + test que `MaintenanceWorker` accepte les trois
/// types de tâche sans modification d'interface.
#[test]
fn maintenance_worker_registers_all_task_types() {
    let _worker = MaintenanceWorker::new()
        .register(Duration::from_secs(3600), Arc::new(ExpiredMemoryGc))
        .register(
            Duration::from_secs(7200),
            Arc::new(AdaptiveForgetting {
                capacity_per_agent: 10_000,
                recency_half_life_secs: 86_400,
            }),
        );
    // ConsolidationTask nécessite Memory + LLM — non instancié ici mais compilable.
    // Le test ci-dessus le vérifie end-to-end.
}
