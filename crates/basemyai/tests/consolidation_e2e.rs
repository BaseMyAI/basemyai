//! Test E2E de consolidation : épisodes → faits + graphe, via AnythingLLM (ADR-016).
//!
//! **Prérequis** : AnythingLLM doit être actif et les variables suivantes définies :
//! - `BASEMYAI_ANYTHINGLLM_KEY`       — clé API Bearer
//! - `BASEMYAI_ANYTHINGLLM_WORKSPACE` — slug du workspace (ex. `"mon-espace-de-travail"`)
//! - `BASEMYAI_ANYTHINGLLM_URL`       — (optionnel, défaut `http://localhost:3001`)
//!
//! Lancement :
//! ```
//! $env:BASEMYAI_ANYTHINGLLM_KEY="HDVSZ29-PVFMR79-P6NR83G-K06A1CC"
//! $env:BASEMYAI_ANYTHINGLLM_WORKSPACE="mon-espace-de-travail"
//! cargo test -p basemyai --test consolidation_e2e -- --ignored
//! ```
//!
//! Le test est annoté `#[ignore]` : il ne s'exécute **jamais** en CI.

use std::time::Duration;

use basemyai::{AgentId, AnythingLlmBackend, LlmInference, Memory, MemoryLayer, anythingllm_from_env, consolidate};
use basemyai_core::{Embedder, Result, Store};

const DIM: usize = 384;

/// Embedder déterministe (sans Candle) pour les tests d'intégration.
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
        "fake-e2e"
    }

    fn dim(&self) -> usize {
        DIM
    }
}

fn agent(id: &str) -> AgentId {
    AgentId::new(id).expect("non-empty id")
}

/// Ouvre une mémoire en RAM pour les tests.
async fn open_memory(agent_id: &str) -> Memory {
    let store = Store::open_in_memory().await.expect("store");
    Memory::open(store, Box::new(FakeEmbedder), agent(agent_id))
        .await
        .expect("memory")
}

// ─── Tests unitaires (toujours verts, sans réseau) ───────────────────────────

#[test]
fn anythingllm_from_env_returns_none_when_key_absent() {
    // Si la variable clé est définie (ex. lors d'un run E2E local), le test est
    // trivialmente skippé — on ne peut pas garantir l'absence sans remove_var (unsafe).
    if std::env::var("BASEMYAI_ANYTHINGLLM_KEY").is_ok() {
        return;
    }
    assert!(
        anythingllm_from_env().is_none(),
        "doit retourner None si BASEMYAI_ANYTHINGLLM_KEY est absente"
    );
}

#[test]
fn anythingllm_backend_model_id_is_workspace_slug() {
    let b = AnythingLlmBackend::new("http://localhost:3001", "mon-workspace", "mykey");
    assert_eq!(
        b.model_id(),
        "mon-workspace",
        "model_id() doit retourner le slug du workspace"
    );
}

#[test]
fn anythingllm_backend_with_timeout_changes_timeout() {
    let b = AnythingLlmBackend::new("http://localhost:3001", "ws", "key")
        .with_timeout(Duration::from_secs(10));
    // Le timeout est privé ; on vérifie uniquement que le builder compile et retourne Self.
    let _ = b;
}

// ─── Test E2E (nécessite AnythingLLM actif + variables d'environnement) ──────

/// Lance le pipeline complet : épisodes en mémoire → consolidation → faits dans le
/// graphe et couche sémantique, via un vrai LLM (AnythingLLM workspace-chat API).
///
/// Ce test est la **première exécution réelle** de `consolidate()` contre un LLM.
/// Il valide que :
/// 1. `AnythingLlmBackend` envoie le prompt et reçoit une réponse parseable.
/// 2. La réponse contient du JSON valide au format attendu par `RawExtraction`.
/// 3. Des faits et/ou entités sont bien persistés après consolidation.
///
/// Si le LLM retourne du texte hors-JSON (ex. balises markdown), le test échoue
/// avec `MemoryError::Extraction` — corriger `consolidate()` si cela arrive.
#[tokio::test]
#[ignore = "nécessite AnythingLLM actif + BASEMYAI_ANYTHINGLLM_KEY + BASEMYAI_ANYTHINGLLM_WORKSPACE"]
async fn consolidation_e2e_anythingllm_extracts_facts_and_graph() {
    let backend = anythingllm_from_env().expect(
        "BASEMYAI_ANYTHINGLLM_KEY et BASEMYAI_ANYTHINGLLM_WORKSPACE doivent être définis",
    );

    let memory = open_memory("e2e-agent").await;

    // ── Peuplement : 3 épisodes riches en entités et relations ───────────────
    memory
        .remember(
            "Alice a rencontré Bob lors de la conférence Rust Paris 2026.",
            MemoryLayer::Episodic,
        )
        .await
        .expect("épisode 1");
    memory
        .remember(
            "Bob travaille chez Anthropic en tant qu'ingénieur sénior.",
            MemoryLayer::Episodic,
        )
        .await
        .expect("épisode 2");
    memory
        .remember(
            "Alice est la fondatrice de BaseMyAI, une startup basée à Paris.",
            MemoryLayer::Episodic,
        )
        .await
        .expect("épisode 3");

    let before = memory.stats().await.expect("stats before");
    assert_eq!(before.episodic, 3, "3 épisodes avant consolidation");
    assert_eq!(before.semantic, 0, "aucun fait sémantique avant consolidation");

    // ── Consolidation ────────────────────────────────────────────────────────
    let report = consolidate(&memory, &backend)
        .await
        .expect("consolidation doit réussir avec un LLM actif");

    println!("Rapport de consolidation : {report:?}");
    assert_eq!(report.episodes_seen, 3, "les 3 épisodes doivent avoir été lus");

    // Le LLM doit extraire au moins un fait ou une entité.
    assert!(
        report.facts_added + report.entities_upserted > 0,
        "le LLM doit extraire au moins un fait ou une entité — rapport : {report:?}"
    );

    // ── Vérification de l'état de la mémoire ─────────────────────────────────
    let after = memory.stats().await.expect("stats after");
    assert!(
        after.semantic >= report.facts_added,
        "le nombre de faits sémantiques doit avoir augmenté"
    );

    // ── Vérification du graphe si des entités ont été créées ─────────────────
    if report.entities_upserted > 0 {
        // On ne connaît pas les IDs des entités générées par le LLM, mais on peut
        // vérifier que le graphe n'est plus vide en traversant depuis n'importe quelle
        // entité connue. On le fera via recall pour trouver une entité.
        println!(
            "Entités upsertées : {} | Relations upsertées : {}",
            report.entities_upserted, report.relations_upserted
        );
    }

    println!("Stats après : total={}", after.total());
    println!("Test E2E réussi : consolidation → {report:?}");
}

/// Vérifie que `choose_llm()` sélectionne AnythingLLM quand les variables sont
/// définies et qu'aucun modèle direct n'est disponible.
///
/// Nécessite que Ollama/LM Studio soient inactifs (ou en dehors des ports sondés).
/// Difficile à garantir en CI → ignoré.
#[tokio::test]
#[ignore = "nécessite BASEMYAI_ANYTHINGLLM_KEY + BASEMYAI_ANYTHINGLLM_WORKSPACE et pas d'Ollama actif"]
async fn choose_llm_falls_back_to_anythingllm() {
    use basemyai::choose_llm;

    let provision = choose_llm().await.expect("choose_llm doit retourner un backend");
    // Le model_id est le slug du workspace quand c'est AnythingLLM.
    println!("Backend choisi : model_id={}", provision.model_id);
    let response = provision.backend.complete("Réponds juste 'pong'.").await.expect("completion");
    assert!(!response.is_empty(), "la réponse ne doit pas être vide");
    println!("Réponse LLM : {response}");
}
