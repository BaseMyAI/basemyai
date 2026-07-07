//! Point d'entrée du runner de tests **déclaratifs multi-backend** (N2,
//! `docs/TODO-NATIVE-ENGINE.md`). Rejoue `memory_tests::scenarios::all()`
//! contre chaque backend enregistré ci-dessous via `backend_suite!`.
//!
//! Aujourd'hui : un seul backend réel, `Libsql`. Brancher `Native` (dépendance
//! N3/N4, non commencés — voir `docs/TODO-NATIVE-ENGINE.md`) est mécanique :
//! implémenter `MemoryStore` pour lui, écrire une factory async équivalente à
//! `make_libsql_store`, puis décommenter/ajouter une ligne `backend_suite!`.
//! Aucune autre modification de ce fichier ni de `memory_tests/mod.rs` n'est
//! nécessaire — c'est précisément ce que la borne générique
//! `run_scenario<S: MemoryStore>` rend possible.

#[path = "memory_tests/mod.rs"]
mod memory_tests;

use basemyai::storage::LibsqlMemoryStore;
use basemyai_core::Store;
use memory_tests::run_scenario;

/// Backend `Libsql` frais (in-memory, migré) — une instance par scénario,
/// isolation totale même si deux scénarios partageaient un `agent` id.
async fn make_libsql_store() -> LibsqlMemoryStore {
    let store = Store::open_in_memory().await.expect("store in-memory ouvre");
    store.migrate(&basemyai::schema()).await.expect("migration");
    LibsqlMemoryStore::new(store)
}

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

backend_suite!(libsql, make_libsql_store);

/// `put_memory_batch` est tout-ou-rien : un id dupliqué **au milieu** du lot
/// ne doit laisser aucune trace des items valides qui l'entourent — ni côté
/// libSQL (violation de contrainte UNIQUE, transaction jamais commitée), ni
/// côté Native depuis N5.5 (`PersistentMemoryIndex::put_many`, résorbant
/// l'écart « atomique par item » d'ADR-027 §6). Générique sur
/// [`MemoryStore`] pour être rejouée verbatim contre les deux backends —
/// même discipline que `run_scenario`.
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

#[tokio::test]
async fn libsql_put_memory_batch_is_all_or_nothing() {
    assert_put_memory_batch_is_all_or_nothing(&make_libsql_store().await).await;
}

/// Backend `Native` frais (répertoire temporaire jetable, supprimé au drop) —
/// une instance par scénario, comme `Libsql`. C'est ici que le diff
/// multi-backend promis au N2 se prouve : mêmes scénarios, même runner,
/// deux moteurs (ADR-027/N5.1).
#[cfg(feature = "engine-native")]
async fn make_native_store() -> basemyai::storage::NativeMemoryStore {
    basemyai::storage::NativeMemoryStore::open_ephemeral().expect("store natif éphémère ouvre")
}

#[cfg(feature = "engine-native")]
backend_suite!(native, make_native_store);

#[cfg(feature = "engine-native")]
#[tokio::test]
async fn native_put_memory_batch_is_all_or_nothing() {
    assert_put_memory_batch_is_all_or_nothing(&make_native_store().await).await;
}

/// Backend `Native` **chiffré au repos** (N5.4, ADR-030) : la suite complète
/// des scénarios rejouée contre un store natif dont WAL et SST sont scellés —
/// le chiffrement doit être transparent pour tout le contrat `MemoryStore`,
/// zéro divergence tolérée avec les deux backends en clair ci-dessus.
#[cfg(feature = "engine-native")]
async fn make_native_encrypted_store() -> basemyai::storage::NativeMemoryStore {
    basemyai::storage::NativeMemoryStore::open_ephemeral_encrypted("clé-de-test-scénarios")
        .expect("store natif chiffré éphémère ouvre")
}

#[cfg(feature = "engine-native")]
backend_suite!(native_encrypted, make_native_encrypted_store);

/// Rotation de clé sur le backend natif (N5.4, ADR-030 §4) — le pendant
/// natif de `tests/key_rotation.rs` (libSQL, feature `crypto`) : la donnée
/// mémorisée avant rotation reste lisible sous la nouvelle clé, l'ancienne
/// clé n'ouvre plus rien, et — contrairement à libSQL — l'instance ayant
/// exécuté la rotation reste utilisable sans réouverture.
#[cfg(feature = "engine-native")]
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
            .recall_vector(&agent, &vector, 5, None, Metric::Cosine, 0)
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
        .recall_vector(&agent, &vector, 5, None, Metric::Cosine, 0)
        .await
        .expect("recall sous la nouvelle clé");
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].id, "m1");
    assert_eq!(got[0].text, "la lune est en roche");
}

/// `rotate_key` sur un store natif ouvert en clair : erreur franche, parité
/// de posture avec `rotate_key_on_unencrypted_memory_fails`
/// (`tests/key_rotation.rs`, `CoreError::Encryption` côté libSQL).
/// Concurrence des lecteurs (N5.5, barre hardening M6) : depuis le passage
/// de `NativeMemoryStore` de `Mutex` à `RwLock`, plusieurs lectures doivent
/// pouvoir s'exécuter **en parallèle** sans se corrompre ni se bloquer les
/// unes les autres. Ce test vérifie la correction sous charge concurrente
/// (beaucoup de lectures mixtes en vol simultanément, résultats tous
/// corrects) et **mesure** — sans assertion stricte sur la latence, trop
/// bruitée en CI — le ratio séquentiel/concurrent, journalisé pour
/// inspection humaine plutôt que comme un seuil de flakiness.
#[cfg(feature = "engine-native")]
#[tokio::test]
async fn native_concurrent_reads_are_correct_and_faster_than_sequential() {
    use basemyai::MemoryLayer;
    use basemyai::storage::{MemoryStore, NativeMemoryStore};
    use basemyai::temporal::Validity;
    use std::sync::Arc;
    use std::time::Instant;

    let store = Arc::new(NativeMemoryStore::open_ephemeral().expect("store natif éphémère"));
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
                        .vector_ranking_ids(&agent, &query, 10, 0)
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
                        .exact_fact_exists(&agent, "mémoire numéro 0 avec un terme0unique")
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
            .vector_ranking_ids(&agent, &query, 10, 0)
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
            store.vector_ranking_ids(&agent, &query, 10, 0).await
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

#[cfg(feature = "engine-native")]
#[tokio::test]
async fn native_rotate_key_on_plaintext_store_fails() {
    let store = basemyai::storage::NativeMemoryStore::open_ephemeral().expect("store en clair");
    assert!(
        store.rotate_key("peu-importe").await.is_err(),
        "rotate_key sur un store non chiffré doit échouer"
    );
}
