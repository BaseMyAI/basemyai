//! Tests d'intégration du wiring `MaintenanceWorker` (M0.2).
//! Vérifie que `ConsolidationTask` et `AdaptiveForgettingTask` tournent
//! correctement via l'interface `MaintenanceTask`. Le GC temporel
//! (`valid_until`) reste sans équivalent natif (hors scope ADR-037, voir
//! `crates/basemyai-cli/src/commands/maintenance.rs`) ; l'oubli adaptatif,
//! lui, a été porté sur le moteur natif par ADR-037 (scan applicatif au lieu
//! du `ROW_NUMBER() OVER` SQL retiré par ADR-033).

use std::sync::Arc;
use std::time::Duration;

use basemyai::{
    AdaptiveForgettingPolicy, AdaptiveForgettingTask, AgentId, ConsolidationTask, LlmInference, Memory, MemoryLayer,
};
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

/// `AdaptiveForgettingTask::run` évince physiquement les souvenirs excédant
/// la capacité — bout en bout via l'interface `MaintenanceTask` (ADR-037).
#[tokio::test]
async fn adaptive_forgetting_task_evicts_beyond_capacity_via_maintenance_interface() {
    let mem = Arc::new(open_memory("a").await);
    for i in 0..5 {
        mem.remember(&format!("souvenir numero {i}"), MemoryLayer::Semantic)
            .await
            .expect("souvenir");
    }
    assert_eq!(mem.stats().await.expect("stats").total(), 5);

    let policy = AdaptiveForgettingPolicy {
        capacity: 2,
        recency_half_life_secs: 86_400,
    };
    let task = AdaptiveForgettingTask::new(Arc::clone(&mem), policy);
    task.run().await.expect("run");

    assert_eq!(
        mem.stats().await.expect("stats").total(),
        2,
        "la capacité doit être respectée après l'éviction"
    );
}

/// No-op si l'agent est déjà sous la capacité : aucun souvenir ne disparaît.
#[tokio::test]
async fn adaptive_forgetting_task_is_a_noop_under_capacity() {
    let mem = Arc::new(open_memory("a").await);
    mem.remember("un seul souvenir", MemoryLayer::Semantic)
        .await
        .expect("souvenir");

    let policy = AdaptiveForgettingPolicy {
        capacity: 10,
        recency_half_life_secs: 86_400,
    };
    AdaptiveForgettingTask::new(Arc::clone(&mem), policy)
        .run()
        .await
        .expect("run");

    assert_eq!(mem.stats().await.expect("stats").total(), 1);
}

/// End-to-end : `AdaptiveForgettingTask` enregistrée dans un
/// `MaintenanceWorker` et déclenchée par la boucle de fond, comme
/// `ConsolidationTask` — même pattern d'injection (ADR-032/033/037).
#[tokio::test]
async fn adaptive_forgetting_runs_through_worker_background_loop() {
    let mem = Arc::new(open_memory("a").await);
    for i in 0..5 {
        mem.remember(&format!("souvenir numero {i}"), MemoryLayer::Semantic)
            .await
            .expect("souvenir");
    }

    let policy = AdaptiveForgettingPolicy {
        capacity: 1,
        recency_half_life_secs: 86_400,
    };
    MaintenanceWorker::new()
        .register(
            Duration::from_millis(40),
            Arc::new(AdaptiveForgettingTask::new(Arc::clone(&mem), policy)),
        )
        .start();

    tokio::time::sleep(Duration::from_millis(200)).await;

    assert_eq!(
        mem.stats().await.expect("stats").total(),
        1,
        "la boucle de maintenance doit avoir évincé les souvenirs excédentaires"
    );
}
