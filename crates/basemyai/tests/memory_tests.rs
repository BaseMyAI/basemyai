//! Point d'entrée du runner de tests **déclaratifs** (N2,
//! `docs/TODO-NATIVE-ENGINE.md`). Rejoue `memory_tests::scenarios::all()`
//! contre le backend natif via `backend_suite!` — le runner reste générique
//! sur `MemoryStore` (posé au N2 pour un diff multi-backend qui a bien eu
//! lieu tant que libSQL vivait, ADR-032 §conséquences), donc brancher un futur
//! second backend resterait mécanique.

#[path = "memory_tests/mod.rs"]
mod memory_tests;
mod support;

use memory_tests::run_scenario;

/// Enregistre un backend : génère un `#[tokio::test]` qui rejoue **tous** les
/// scénarios de `memory_tests::scenarios::all()` contre une instance fraîche
/// du backend nommé `$backend`, construite par `$make`.
macro_rules! backend_suite {
    ($backend:ident, $make:expr) => {
        #[tokio::test]
        async fn $backend() {
            for scenario in memory_tests::scenarios::all() {
                let store = $make().await;
                run_scenario(&store, &scenario).await;
            }
        }
    };
}

/// `put_memory_batch` est tout-ou-rien : un id dupliqué **au milieu** du lot
/// ne doit laisser aucune trace des items valides qui l'entourent
/// (`PersistentMemoryIndex::put_many`, résorbant l'écart « atomique par
/// item » d'ADR-027 §6).
async fn assert_put_memory_batch_is_all_or_nothing<S: basemyai::storage::MemoryStore>(store: &S) {
    use basemyai::MemoryLayer;
    use basemyai::storage::NewMemory;
    use basemyai::temporal::Validity;

    let agent = basemyai::AgentId::new("batch-atomicity-agent").expect("agent id");
    let existing = memory_tests::vec_for(9);
    store
        .put_memory(
            "existing",
            &agent,
            MemoryLayer::Episodic,
            "déjà là",
            Validity::since(0),
            &existing,
            "user",
        )
        .await
        .expect("seed");

    let v1 = memory_tests::vec_for(1);
    let v2 = memory_tests::vec_for(2);
    let v3 = memory_tests::vec_for(3);
    let items = vec![
        NewMemory {
            id: "fresh-1".to_string(),
            layer: MemoryLayer::Episodic,
            text: "un",
            validity: Validity::since(0),
            vector: &v1,
            source: "user",
        },
        NewMemory {
            id: "existing".to_string(), // duplicate: collides with the seed
            layer: MemoryLayer::Episodic,
            text: "dup",
            validity: Validity::since(0),
            vector: &v2,
            source: "user",
        },
        NewMemory {
            id: "fresh-2".to_string(),
            layer: MemoryLayer::Episodic,
            text: "deux",
            validity: Validity::since(0),
            vector: &v3,
            source: "user",
        },
    ];
    assert!(
        store.put_memory_batch(&agent, &items).await.is_err(),
        "un id dupliqué dans le lot doit faire échouer tout le batch"
    );

    // Ni fresh-1 ni fresh-2 ne doivent avoir survécu à l'échec.
    let hydrated = store
        .hydrate(&agent, &["fresh-1".to_string(), "fresh-2".to_string()], 0)
        .await
        .expect("hydrate");
    assert!(
        hydrated.is_empty(),
        "aucun item du lot en échec ne doit être visible : {hydrated:?}"
    );
    // Seul le souvenir seedé avant le batch doit rester.
    let stats = store.agent_stats(&agent, 0).await.expect("stats");
    assert_eq!(stats.total(), 1, "le batch en échec ne doit rien avoir persisté");
}

/// Backend natif frais (répertoire temporaire jetable, supprimé au drop) —
/// une instance par scénario.
async fn make_native_store() -> basemyai::storage::NativeMemoryStore {
    support::open_native_store()
}

backend_suite!(native, make_native_store);

#[tokio::test]
async fn native_put_memory_batch_is_all_or_nothing() {
    assert_put_memory_batch_is_all_or_nothing(&make_native_store().await).await;
}

/// Backend natif **chiffré au repos** (N5.4, ADR-030) : la suite complète des
/// scénarios rejouée contre un store natif dont WAL et SST sont scellés — le
/// chiffrement doit être transparent pour tout le contrat `MemoryStore`, zéro
/// divergence tolérée avec le backend en clair ci-dessus.
async fn make_native_encrypted_store() -> basemyai::storage::NativeMemoryStore {
    support::open_encrypted_native_store("clé-de-test-scénarios")
}

backend_suite!(native_encrypted, make_native_encrypted_store);

/// Rotation de clé (N5.4, ADR-030 §4) : la donnée mémorisée avant rotation
/// reste lisible sous la nouvelle clé, l'ancienne clé n'ouvre plus rien, et
/// l'instance ayant exécuté la rotation reste utilisable sans réouverture.
#[tokio::test]
async fn native_rotate_key_preserves_data_and_invalidates_old_key() {
    use basemyai::MemoryLayer;
    use basemyai::storage::{MemoryStore, NativeMemoryStore};
    use basemyai::temporal::Validity;
    use basemyai_core::Metric;

    let dir = tempfile::tempdir().expect("tempdir");
    let agent = basemyai::AgentId::new("native-rotate-agent").expect("agent id");
    let vector = memory_tests::vec_for(1);

    {
        let store = NativeMemoryStore::open_encrypted(dir.path(), "ancienne-clé").expect("open chiffré");
        store
            .put_memory(
                "m1",
                &agent,
                MemoryLayer::Semantic,
                "la lune est en roche",
                Validity::since(0),
                &vector,
                "user",
            )
            .await
            .expect("put avant rotation");

        store.rotate_key("nouvelle-clé").await.expect("rotation");

        // L'instance reste pleinement utilisable après rotation (ADR-030 §4).
        let got = store
            .recall_vector(&agent, &vector, 5, None, Metric::Cosine, 0, true)
            .await
            .expect("recall post-rotation sur la même instance");
        assert_eq!(got.len(), 1, "l'instance doit rester utilisable après rotate_key");
    }

    // L'ancienne clé ne doit plus ouvrir le store.
    assert!(
        NativeMemoryStore::open_encrypted(dir.path(), "ancienne-clé").is_err(),
        "l'ancienne clé ne doit plus ouvrir le store après rotation"
    );

    // La nouvelle clé rouvre et retrouve le souvenir intact.
    let store = NativeMemoryStore::open_encrypted(dir.path(), "nouvelle-clé").expect("reopen nouvelle clé");
    let got = store
        .recall_vector(&agent, &vector, 5, None, Metric::Cosine, 0, true)
        .await
        .expect("recall sous la nouvelle clé");
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].id, "m1");
    assert_eq!(got[0].text, "la lune est en roche");
}

/// `rotate_key` sur un store natif ouvert en clair : erreur franche.
/// Concurrence des lecteurs (N5.5, barre hardening M6) : depuis le passage de
/// `NativeMemoryStore` de `Mutex` à `RwLock`, plusieurs lectures doivent
/// pouvoir s'exécuter **en parallèle** sans se corrompre ni se bloquer les
/// unes les autres. Ce test vérifie la correction sous charge concurrente
/// (beaucoup de lectures mixtes en vol simultanément, résultats tous
/// corrects) et **mesure** — sans assertion stricte sur la latence, trop
/// bruitée en CI — le ratio séquentiel/concurrent, journalisé pour
/// inspection humaine plutôt que comme un seuil de flakiness.
#[tokio::test]
async fn native_concurrent_reads_are_correct_and_faster_than_sequential() {
    use basemyai::MemoryLayer;
    use basemyai::storage::MemoryStore;
    use basemyai::temporal::Validity;
    use std::sync::Arc;
    use std::time::Instant;

    let store = Arc::new(support::open_native_store());
    let agent = basemyai::AgentId::new("concurrent-reads-agent").expect("agent id");

    const N: usize = 200;
    for i in 0..N {
        let vector = memory_tests::vec_for((i % 251) as u8);
        store
            .put_memory(
                &format!("m{i}"),
                &agent,
                MemoryLayer::Episodic,
                &format!("mémoire numéro {i} avec un terme{i}unique"),
                Validity::since(0),
                &vector,
                "user",
            )
            .await
            .expect("seed");
    }

    const READS: usize = 64;
    let query = memory_tests::vec_for(7);

    // Correction sous charge concurrente : READS lectures de nature
    // différente (vecteur, mot-clé, stats, graphe) en vol simultanément.
    let mut handles = Vec::with_capacity(READS);
    for i in 0..READS {
        let store = Arc::clone(&store);
        let agent = agent.clone();
        let query = query.clone();
        handles.push(tokio::spawn(async move {
            match i % 3 {
                0 => {
                    let ids = store
                        .vector_ranking_ids(&agent, &query, 10, 0, true)
                        .await
                        .expect("vector ranking");
                    assert!(
                        !ids.is_empty(),
                        "des souvenirs existent, le classement ne doit pas être vide"
                    );
                }
                1 => {
                    let stats = store.agent_stats(&agent, 0).await.expect("stats");
                    assert_eq!(stats.total(), N, "toutes les mémoires seedées doivent être comptées");
                }
                _ => {
                    let exists = store
                        .exact_fact_exists(&agent, "mémoire numéro 0 avec un terme0unique", 0)
                        .await
                        .expect("exact fact");
                    // Couche episodic, pas semantic : jamais un "fait exact" — la
                    // parité de `exact_fact_exists` (ADR-027 §6) est ce qui est
                    // sous test ici, pas la présence du souvenir en tant que tel.
                    assert!(!exists);
                }
            }
        }));
    }
    for handle in handles {
        handle.await.expect("tâche concurrente ne doit pas paniquer");
    }

    // Mesure (journalisée, pas assertée strictement) : READS lectures
    // séquentielles vs. la même charge lancée concurremment.
    let sequential_start = Instant::now();
    for _ in 0..READS {
        store
            .vector_ranking_ids(&agent, &query, 10, 0, true)
            .await
            .expect("sequential read");
    }
    let sequential = sequential_start.elapsed();

    let concurrent_start = Instant::now();
    let mut handles = Vec::with_capacity(READS);
    for _ in 0..READS {
        let store = Arc::clone(&store);
        let agent = agent.clone();
        let query = query.clone();
        handles.push(tokio::spawn(async move {
            store.vector_ranking_ids(&agent, &query, 10, 0, true).await
        }));
    }
    for handle in handles {
        handle.await.expect("concurrent read task").expect("concurrent read");
    }
    let concurrent = concurrent_start.elapsed();

    eprintln!(
        "native_concurrent_reads: {READS} reads — sequential {sequential:?}, concurrent {concurrent:?} \
         (ratio {:.2}x)",
        sequential.as_secs_f64() / concurrent.as_secs_f64().max(f64::EPSILON)
    );
}

#[tokio::test]
async fn native_rotate_key_on_plaintext_store_fails() {
    let store = support::open_native_store();
    assert!(
        store.rotate_key("peu-importe").await.is_err(),
        "rotate_key sur un store non chiffré doit échouer"
    );
}

#[test]
fn native_wrong_encryption_key_maps_to_typed_core_error() {
    use basemyai::storage::NativeMemoryStore;
    use basemyai_core::CoreError;

    let dir = tempfile::tempdir().expect("tempdir");
    NativeMemoryStore::open_encrypted(dir.path(), "bonne-clé").expect("open chiffré");
    let Err(err) = NativeMemoryStore::open_encrypted(dir.path(), "mauvaise-clé") else {
        panic!("mauvaise clé aurait dû échouer");
    };
    match err {
        basemyai::MemoryError::Core(CoreError::WrongEncryptionKey) => {}
        other => panic!("attendu WrongEncryptionKey, reçu {other:?}"),
    }
}
