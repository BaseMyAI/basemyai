//! Tests d'intégration du wiring `MaintenanceWorker` (M0.2).
//! Vérifie que `ConsolidationTask`, `AdaptiveForgettingTask` et
//! `ExpiredMemoryGcTask` tournent correctement via l'interface
//! `MaintenanceTask`. L'oubli adaptatif a été porté sur le moteur natif par
//! ADR-037 (scan applicatif au lieu du `ROW_NUMBER() OVER` SQL retiré par
//! ADR-033) ; le GC temporel par ADR-038 (scan applicatif paginé au lieu
//! d'un `DELETE` SQL fenêtré). Les deux mécanismes opèrent sur des
//! ensembles disjoints par construction (actifs vs. expirés) — voir la doc
//! de `basemyai::maintenance::expired_gc`.

use std::sync::Arc;
use std::time::Duration;

use basemyai::storage::{MemoryStore, NativeMemoryStore};
use basemyai::temporal::Validity;
use basemyai::{
    AdaptiveForgettingPolicy, AdaptiveForgettingTask, AgentId, ConsolidationTask, ExpiredMemoryGcTask, LlmInference,
    Memory, MemoryLayer,
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

/// Temps Unix courant — `basemyai::now_unix` est `pub(crate)`, hors de
/// portée pour un test d'intégration (crate externe) ; ce test n'a besoin
/// que d'un instant "maintenant" plausible pour interroger `list_memories`.
fn now() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

async fn open_memory(agent_id: &str) -> Memory {
    let store = Arc::new(support::open_native_store());
    Memory::from_native_store(store, Box::new(FakeEmbedder), agent(agent_id))
        .await
        .expect("open memory")
}

/// Comme [`open_memory`], mais renvoie aussi le store natif partagé — pour
/// les tests qui ont besoin d'ouvrir une **seconde** `Memory` (autre agent)
/// sur le **même** store (isolation adversariale) ou d'inspecter des
/// candidats bas niveau (`scan_for_forgetting`/`scan_expired`) sans passer
/// par la façade `Memory`.
async fn open_memory_with_store(agent_id: &str) -> (Memory, Arc<NativeMemoryStore>) {
    let store = Arc::new(support::open_native_store());
    let mem = Memory::from_native_store(Arc::clone(&store), Box::new(FakeEmbedder), agent(agent_id))
        .await
        .expect("open memory");
    (mem, store)
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

/// Capacité zéro : tout ce qui est actif est évincé, y compris via le
/// `MaintenanceTask` (pas seulement au niveau de `select_victims`, déjà
/// testé unitairement dans `maintenance::adaptive_forgetting::tests`).
#[tokio::test]
async fn adaptive_forgetting_zero_capacity_evicts_everything_via_task() {
    let mem = Arc::new(open_memory("a").await);
    for i in 0..3 {
        mem.remember(&format!("souvenir {i}"), MemoryLayer::Semantic)
            .await
            .expect("souvenir");
    }
    let policy = AdaptiveForgettingPolicy {
        capacity: 0,
        recency_half_life_secs: 86_400,
    };
    AdaptiveForgettingTask::new(Arc::clone(&mem), policy)
        .run()
        .await
        .expect("run");
    assert_eq!(mem.stats().await.expect("stats").total(), 0);
}

/// Isolation stricte : l'oubli adaptatif de l'agent A ne doit ni compter ni
/// évincer les souvenirs de l'agent B, même sur le **même** store partagé
/// (isolation structurelle par préfixe de clé, ADR-006/ADR-027 §2).
#[tokio::test]
async fn adaptive_forgetting_does_not_cross_agent_boundary() {
    let (mem_a, store) = open_memory_with_store("agent-a").await;
    let mem_b = Memory::from_native_store(Arc::clone(&store), Box::new(FakeEmbedder), agent("agent-b"))
        .await
        .expect("open agent B memory");

    for i in 0..5 {
        mem_a
            .remember(&format!("secret agent A {i}"), MemoryLayer::Semantic)
            .await
            .expect("souvenir A");
    }
    for i in 0..5 {
        mem_b
            .remember(&format!("secret agent B {i}"), MemoryLayer::Semantic)
            .await
            .expect("souvenir B");
    }

    let policy = AdaptiveForgettingPolicy {
        capacity: 1,
        recency_half_life_secs: 86_400,
    };
    let report = mem_a.adaptive_forget(policy).await.expect("adaptive_forget A");

    assert_eq!(report.scanned, 5, "le scan de A ne doit voir que les 5 souvenirs de A");
    assert_eq!(mem_a.stats().await.expect("stats A").total(), 1);
    assert_eq!(
        mem_b.stats().await.expect("stats B").total(),
        5,
        "agent B ne doit subir aucune éviction déclenchée pour agent A"
    );
}

/// ADR-038 (amendement ADR-037) : un souvenir invalidé ou déjà expiré ne
/// doit ni compter dans la population scannée, ni pouvoir "protéger" sa
/// place au détriment d'un souvenir actif moins bien noté — les deux
/// mécanismes opèrent sur des ensembles disjoints par construction.
#[tokio::test]
async fn adaptive_forgetting_ignores_invalidated_and_expired_memories() {
    let (mem, store) = open_memory_with_store("a").await;
    let agent_id = agent("a");

    // Deux souvenirs actifs.
    let active_1 = mem.remember("actif un", MemoryLayer::Semantic).await.expect("actif 1");
    let active_2 = mem
        .remember("actif deux", MemoryLayer::Semantic)
        .await
        .expect("actif 2");

    // Un souvenir invalidé (valid_until = now au moment de l'appel).
    let invalidated = mem.remember("invalidé", MemoryLayer::Semantic).await.expect("invalidé");
    mem.invalidate(&invalidated).await.expect("invalidate");

    // Un souvenir déjà expiré à l'écriture (fenêtre de validité entièrement
    // dans le passé).
    let expired = mem
        .remember_with(
            "expiré",
            MemoryLayer::Semantic,
            Validity {
                valid_from: 0,
                valid_until: Some(1),
            },
        )
        .await
        .expect("expiré");

    let policy = AdaptiveForgettingPolicy {
        capacity: 1,
        recency_half_life_secs: 86_400,
    };
    let report = mem.adaptive_forget(policy).await.expect("adaptive_forget");

    assert_eq!(
        report.scanned, 2,
        "seuls les souvenirs actifs (actif un, actif deux) doivent être scannés"
    );
    assert_eq!(report.evicted, 1, "1 des 2 actifs dépasse la capacité de 1");
    assert_eq!(
        mem.stats().await.expect("stats").total(),
        1,
        "1 souvenir actif doit survivre"
    );

    // Les invalidé/expiré n'ont pas été touchés physiquement par l'oubli
    // adaptatif : ils restent présents en base (visibles via `list_memories`
    // avec `include_invalid = true`), seul le GC temporel a vocation à les
    // supprimer (ADR-038).
    let now = now();
    let all = store
        .list_memories(&agent_id, None, 100, true, now)
        .await
        .expect("list_memories");
    let ids: Vec<&str> = all.iter().map(|r| r.id.as_str()).collect();
    assert!(
        ids.contains(&invalidated.as_str()),
        "invalidé doit survivre à l'oubli adaptatif"
    );
    assert!(
        ids.contains(&expired.as_str()),
        "expiré doit survivre à l'oubli adaptatif"
    );
    // Exactement un des deux actifs doit avoir survécu.
    let surviving_actives = [active_1.as_str(), active_2.as_str()]
        .into_iter()
        .filter(|id| ids.contains(id))
        .count();
    assert_eq!(surviving_actives, 1);
}

/// Résilience à une interruption partielle : si une victime a déjà été
/// évincée par un passage antérieur (crash simulé entre deux `forget`), un
/// second passage doit converger vers la capacité cible sans erreur, sans
/// re-tenter la victime déjà partie (chaque `forget` est sa propre
/// transaction moteur — jamais de rollback global, ADR-037 §Conséquences).
#[tokio::test]
async fn adaptive_forgetting_is_resumable_after_a_partial_previous_pass() {
    let (mem, store) = open_memory_with_store("a").await;
    let agent_id = agent("a");
    for i in 0..4 {
        mem.remember(&format!("souvenir {i}"), MemoryLayer::Semantic)
            .await
            .expect("souvenir");
    }
    let policy = AdaptiveForgettingPolicy {
        capacity: 1,
        recency_half_life_secs: 86_400,
    };

    // Découvre les victimes sans les évincer (dry-run) — simule ce qu'une
    // passe réelle aurait décidé.
    let store_handle: Arc<NativeMemoryStore> = Arc::clone(&store);
    let dyn_store: Arc<dyn MemoryStore> = store_handle;
    let preview = basemyai::maintenance::run_adaptive_forget(&dyn_store, &agent_id, policy, true)
        .await
        .expect("dry run");
    assert_eq!(preview.evicted, 3);

    // Simule un crash qui a eu le temps d'évincer UN souvenir avant de
    // s'interrompre : on force sa suppression physique directement via le
    // store, hors de tout passage `Memory::adaptive_forget` (n'importe quel
    // souvenir convient — `select_victims` évincera le reste au prochain
    // passage, quel qu'il soit, puisque la capacité est de 1).
    let any_id = store
        .list_memories(&agent_id, None, 1, false, now())
        .await
        .expect("list")
        .into_iter()
        .next()
        .expect("au moins un souvenir actif")
        .id;
    store.forget(&agent_id, &any_id).await.expect("pre-forget");
    assert_eq!(mem.stats().await.expect("stats").total(), 3);

    // La passe réelle doit converger vers la capacité malgré l'état déjà
    // partiellement évincé, sans erreur.
    let report = mem.adaptive_forget(policy).await.expect("resumed adaptive_forget");
    assert_eq!(report.scanned, 3, "le souvenir pré-évincé n'est plus scanné");
    assert_eq!(mem.stats().await.expect("stats after resume").total(), 1);
}

/// GC temporel : supprime les souvenirs expirés (`valid_until <= now`) et
/// **uniquement** ceux-là — les actifs survivent intacts (ADR-038).
#[tokio::test]
async fn expired_gc_deletes_only_expired_memories() {
    let mem = Arc::new(open_memory("a").await);
    mem.remember("actif un", MemoryLayer::Semantic).await.expect("actif 1");
    mem.remember("actif deux", MemoryLayer::Semantic)
        .await
        .expect("actif 2");
    mem.remember_with(
        "expiré un",
        MemoryLayer::Semantic,
        Validity {
            valid_from: 0,
            valid_until: Some(1),
        },
    )
    .await
    .expect("expiré 1");
    mem.remember_with(
        "expiré deux",
        MemoryLayer::Semantic,
        Validity {
            valid_from: 0,
            valid_until: Some(2),
        },
    )
    .await
    .expect("expiré 2");

    let report = mem.expired_gc(100).await.expect("expired_gc");
    assert_eq!(report.examined, 2);
    assert_eq!(report.deleted, 2);
    assert_eq!(report.pages, 1);
    assert_eq!(mem.stats().await.expect("stats").total(), 2, "les 2 actifs survivent");

    let hits = mem.recall("actif", 10).await.expect("recall");
    assert_eq!(hits.len(), 2);
}

/// Idempotence : un second passage sur un état déjà nettoyé ne trouve rien
/// et ne supprime rien.
#[tokio::test]
async fn expired_gc_is_idempotent() {
    let mem = Arc::new(open_memory("a").await);
    mem.remember_with(
        "expiré",
        MemoryLayer::Semantic,
        Validity {
            valid_from: 0,
            valid_until: Some(1),
        },
    )
    .await
    .expect("expiré");

    let first = mem.expired_gc(100).await.expect("first pass");
    assert_eq!(first.deleted, 1);

    let second = mem.expired_gc(100).await.expect("second pass");
    assert_eq!(second.examined, 0);
    assert_eq!(second.deleted, 0);
    assert_eq!(second.pages, 0);
}

/// Pagination réelle : plus d'expirés que `page_size`, le GC doit boucler
/// sur plusieurs pages et tous les supprimer.
#[tokio::test]
async fn expired_gc_paginates_across_multiple_pages() {
    let mem = Arc::new(open_memory("a").await);
    for i in 0..5 {
        mem.remember_with(
            &format!("expiré {i}"),
            MemoryLayer::Semantic,
            Validity {
                valid_from: 0,
                valid_until: Some(1),
            },
        )
        .await
        .expect("expiré");
    }
    let report = mem.expired_gc(2).await.expect("paginated gc");
    assert_eq!(report.examined, 5);
    assert_eq!(report.deleted, 5);
    assert_eq!(report.pages, 3, "5 éléments par pages de 2 => 3 pages (2+2+1)");
    assert_eq!(mem.stats().await.expect("stats").total(), 0);
}

/// `page_size == 0` est un rejet explicite, pas un no-op silencieux qui se
/// lirait à tort comme "rien n'était expiré".
#[tokio::test]
async fn expired_gc_rejects_zero_page_size() {
    let mem = Arc::new(open_memory("a").await);
    let err = mem.expired_gc(0).await.expect_err("page_size 0 must be rejected");
    assert!(matches!(err, basemyai::MemoryError::InvalidGcPageSize));
}

/// Isolation stricte, même discipline que l'oubli adaptatif : le GC de
/// l'agent A ne doit jamais toucher les souvenirs (même expirés) de B.
#[tokio::test]
async fn expired_gc_does_not_cross_agent_boundary() {
    let (mem_a, store) = open_memory_with_store("agent-a").await;
    let mem_b = Memory::from_native_store(Arc::clone(&store), Box::new(FakeEmbedder), agent("agent-b"))
        .await
        .expect("open agent B memory");

    mem_a
        .remember_with(
            "expiré A",
            MemoryLayer::Semantic,
            Validity {
                valid_from: 0,
                valid_until: Some(1),
            },
        )
        .await
        .expect("expiré A");
    mem_b
        .remember_with(
            "expiré B",
            MemoryLayer::Semantic,
            Validity {
                valid_from: 0,
                valid_until: Some(1),
            },
        )
        .await
        .expect("expiré B");

    let report = mem_a.expired_gc(100).await.expect("gc A");
    assert_eq!(report.deleted, 1, "seul le souvenir expiré de A doit être supprimé");

    let now = now();
    let agent_b_id = agent("agent-b");
    let remaining_b = store
        .list_memories(&agent_b_id, None, 100, true, now)
        .await
        .expect("list B");
    assert_eq!(remaining_b.len(), 1, "le souvenir expiré de B doit survivre au GC de A");
}

/// Cohérence des index après suppression : un souvenir évincé (oubli
/// adaptatif) ou supprimé (GC) ne doit plus jamais remonter, ni par
/// recherche vectorielle ni par recherche hybride (BM25) — les deux index
/// disparaissent avec le souvenir (atomicité `forget`, ADR-027 §3).
#[tokio::test]
async fn expired_gc_removes_from_vector_and_keyword_indexes() {
    let mem = Arc::new(open_memory("a").await);
    mem.remember_with(
        "licorne violette unique",
        MemoryLayer::Semantic,
        Validity {
            valid_from: 0,
            valid_until: Some(1),
        },
    )
    .await
    .expect("expiré");

    mem.expired_gc(100).await.expect("gc");

    let vector_hits = mem
        .recall("licorne violette unique", 5)
        .await
        .expect("recall vectoriel");
    assert!(
        vector_hits.is_empty(),
        "le souvenir supprimé ne doit plus remonter par vecteur"
    );
    let hybrid_hits = mem
        .recall_hybrid("licorne violette unique", 5)
        .await
        .expect("recall hybride");
    assert!(
        hybrid_hits.is_empty(),
        "le souvenir supprimé ne doit plus remonter par BM25/hybride"
    );
}

/// Résilience à une interruption partielle, même discipline que l'oubli
/// adaptatif : un souvenir déjà supprimé par un passage antérieur (crash
/// simulé) ne fait pas échouer une reprise, qui termine le travail restant.
#[tokio::test]
async fn expired_gc_is_resumable_after_a_partial_previous_pass() {
    let (mem, store) = open_memory_with_store("a").await;
    let agent_id = agent("a");
    let mut expired_ids = Vec::new();
    for i in 0..3 {
        let id = mem
            .remember_with(
                &format!("expiré {i}"),
                MemoryLayer::Semantic,
                Validity {
                    valid_from: 0,
                    valid_until: Some(1),
                },
            )
            .await
            .expect("expiré");
        expired_ids.push(id);
    }

    // Simule un crash qui a eu le temps de supprimer UN souvenir expiré
    // avant de s'interrompre.
    store.forget(&agent_id, &expired_ids[0]).await.expect("pre-forget");

    let report = mem.expired_gc(100).await.expect("resumed gc");
    assert_eq!(report.examined, 2, "le souvenir pré-supprimé n'est plus scanné");
    assert_eq!(report.deleted, 2);
    assert_eq!(mem.stats().await.expect("stats").total(), 0);
}

/// End-to-end : `ExpiredMemoryGcTask` enregistrée dans un `MaintenanceWorker`
/// et déclenchée par la boucle de fond, même pattern que `ConsolidationTask`
/// et `AdaptiveForgettingTask` (ADR-038).
#[tokio::test]
async fn expired_gc_runs_through_worker_background_loop() {
    let mem = Arc::new(open_memory("a").await);
    mem.remember_with(
        "expiré",
        MemoryLayer::Semantic,
        Validity {
            valid_from: 0,
            valid_until: Some(1),
        },
    )
    .await
    .expect("expiré");
    mem.remember("actif", MemoryLayer::Semantic).await.expect("actif");

    MaintenanceWorker::new()
        .register(
            Duration::from_millis(40),
            Arc::new(ExpiredMemoryGcTask::new(Arc::clone(&mem), 100)),
        )
        .start();

    tokio::time::sleep(Duration::from_millis(200)).await;

    assert_eq!(
        mem.stats().await.expect("stats").total(),
        1,
        "la boucle de maintenance doit avoir supprimé le souvenir expiré, laissant l'actif"
    );
}

/// No-op si rien n'est expiré : le rapport est vide, rien n'est supprimé.
#[tokio::test]
async fn expired_gc_task_is_a_noop_when_nothing_expired() {
    let mem = Arc::new(open_memory("a").await);
    mem.remember("actif", MemoryLayer::Semantic).await.expect("actif");

    ExpiredMemoryGcTask::new(Arc::clone(&mem), 100)
        .run()
        .await
        .expect("run");

    assert_eq!(mem.stats().await.expect("stats").total(), 1);
}
