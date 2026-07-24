//! Point d'entrée du runner de tests **déclaratifs** (N2,
//! `docs/TODO-NATIVE-ENGINE.md`). Rejoue `memory_tests::scenarios::all()`
//! contre le backend natif via `backend_suite!` — le runner reste générique
//! sur `MemoryStore` (posé au N2 pour un diff multi-backend qui a bien eu
//! lieu tant que libSQL vivait, ADR-032 §conséquences), donc brancher un futur
//! second backend resterait mécanique.

#[path = "memory_tests/mod.rs"]
mod memory_tests;
#[path = "../support/mod.rs"]
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
            1.0,
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
            importance: 1.0,
        },
        NewMemory {
            id: "existing".to_string(), // duplicate: collides with the seed
            layer: MemoryLayer::Episodic,
            text: "dup",
            validity: Validity::since(0),
            vector: &v2,
            source: "user",
            importance: 1.0,
        },
        NewMemory {
            id: "fresh-2".to_string(),
            layer: MemoryLayer::Episodic,
            text: "deux",
            validity: Validity::since(0),
            vector: &v3,
            source: "user",
            importance: 1.0,
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

/// `forget_many` (ADR-041 §7.4) : suppression par lots bornés, parité DELETE
/// (ids absents / d'un autre agent / dupliqués ignorés en silence), résultat
/// indépendant des bornes de lot, et re-run idempotent.
#[tokio::test]
async fn native_forget_many_is_bounded_idempotent_and_agent_scoped() {
    use basemyai::MemoryLayer;
    use basemyai::storage::{ForgetBatchOptions, MemoryStore, NewMemory};
    use basemyai::temporal::Validity;

    let store = make_native_store().await;
    let agent = basemyai::AgentId::new("agent-fm").expect("agent id");
    let other = basemyai::AgentId::new("agent-autre").expect("agent id");

    let vectors: Vec<Vec<f32>> = (0..6u8).map(memory_tests::vec_for).collect();
    let items: Vec<NewMemory<'_>> = (0..5usize)
        .map(|i| NewMemory {
            id: format!("m{i}"),
            layer: MemoryLayer::Episodic,
            text: "le chat dort",
            validity: Validity::since(0),
            vector: &vectors[i],
            source: "user",
            importance: 1.0,
        })
        .collect();
    store.put_memory_batch(&agent, &items).await.expect("batch");
    store
        .put_memory(
            "m0",
            &other,
            MemoryLayer::Episodic,
            "autre agent",
            Validity::since(0),
            &vectors[5],
            "user",
            1.0,
        )
        .await
        .expect("seed autre agent");

    // Bornes minuscules : chaque lot est un souvenir — le résultat doit être
    // identique aux défauts. "m0" de l'autre agent, "fantome" et le doublon
    // ne comptent jamais.
    let removed = store
        .forget_many(
            &agent,
            &[
                "m0".to_string(),
                "m1".to_string(),
                "fantome".to_string(),
                "m2".to_string(),
                "m1".to_string(),
            ],
            ForgetBatchOptions {
                max_items: 1,
                max_wal_bytes: 1,
            },
        )
        .await
        .expect("forget_many");
    assert_eq!(removed, 3);

    let stats = store.agent_stats(&agent, 0).await.expect("stats");
    assert_eq!(stats.total(), 2, "m3 et m4 doivent survivre");
    let other_stats = store.agent_stats(&other, 0).await.expect("stats");
    assert_eq!(
        other_stats.total(),
        1,
        "l'id partagé m0 ne doit jamais fuir inter-agent"
    );

    // Re-run : pur no-op (reprise idempotente après interruption).
    let removed = store
        .forget_many(
            &agent,
            &["m0".to_string(), "m1".to_string(), "m2".to_string()],
            ForgetBatchOptions::default(),
        )
        .await
        .expect("re-run");
    assert_eq!(removed, 0);

    // Lot vide : no-op sans erreur.
    assert_eq!(
        store
            .forget_many(&agent, &[], ForgetBatchOptions::default())
            .await
            .expect("lot vide"),
        0
    );
}

/// Registre d'agents (ADR-041 §7.5) : identifiants seuls, inscrit au premier
/// souvenir, désinscrit par `purge_agent` — jamais par un simple `forget`.
#[tokio::test]
async fn native_list_agents_tracks_inserts_and_purges() {
    use basemyai::MemoryLayer;
    use basemyai::storage::MemoryStore;
    use basemyai::temporal::Validity;

    let store = make_native_store().await;
    assert!(store.list_agents().await.expect("list").is_empty());

    let a = basemyai::AgentId::new("agent-a").expect("agent id");
    let b = basemyai::AgentId::new("agent-b").expect("agent id");
    for (agent, seed) in [(&b, 1u8), (&a, 2)] {
        let v = memory_tests::vec_for(seed);
        store
            .put_memory(
                "m1",
                agent,
                MemoryLayer::Episodic,
                "x",
                Validity::since(0),
                &v,
                "user",
                1.0,
            )
            .await
            .expect("put");
    }
    assert_eq!(
        store.list_agents().await.expect("list"),
        vec!["agent-a".to_string(), "agent-b".to_string()]
    );

    store.forget(&b, "m1").await.expect("forget");
    assert_eq!(
        store.list_agents().await.expect("list").len(),
        2,
        "oublier le dernier souvenir laisse l'agent inscrit (visite no-op bon marché)"
    );

    store.purge_agent(&b).await.expect("purge");
    assert_eq!(store.list_agents().await.expect("list"), vec!["agent-a".to_string()]);
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
                1.0,
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

#[tokio::test]
async fn native_full_rotation_preserves_passphrase_mode_and_data() {
    use basemyai::MemoryLayer;
    use basemyai::storage::{MemoryStore, NativeMemoryStore};
    use basemyai::temporal::Validity;
    use basemyai_core::{EncryptionKey, Metric};

    let dir = tempfile::tempdir().expect("tempdir");
    let agent = basemyai::AgentId::new("native-full-rotate-agent").expect("agent id");
    let vector = memory_tests::vec_for(2);

    {
        let store = NativeMemoryStore::open_encrypted(dir.path(), "ancienne-clé").expect("open raw-key store");
        store
            .put_memory(
                "m1",
                &agent,
                MemoryLayer::Semantic,
                "donnée à ré-encrypter",
                Validity::since(0),
                &vector,
                "user",
                1.0,
            )
            .await
            .expect("put before full rotation");
        store
            .rotate_key_full(EncryptionKey::passphrase("nouvelle passphrase"))
            .await
            .expect("full rotate to passphrase");

        let got = store
            .recall_vector(&agent, &vector, 5, None, Metric::Cosine, 0, true)
            .await
            .expect("live store remains usable");
        assert_eq!(got.len(), 1);
    }

    assert!(NativeMemoryStore::open_encrypted(dir.path(), "nouvelle passphrase").is_err());
    let store = NativeMemoryStore::open_with_key(dir.path(), &EncryptionKey::passphrase("nouvelle passphrase"))
        .expect("reopen passphrase generation");
    let got = store
        .recall_vector(&agent, &vector, 5, None, Metric::Cosine, 0, true)
        .await
        .expect("recall after reopen");
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].text, "donnée à ré-encrypter");
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
                1.0,
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

/// ADR-043 §3/J4 (N13) exit criterion, made concrete at the `NativeMemoryStore`
/// level — not just `Engine` (the edit/snapshot protocol itself is already
/// pinned there by `compact_prepare_then_concurrent_flush_then_commit_keeps_both`,
/// `basemyai-engine/tests/snapshot_compaction.rs`): "aucun lecteur … n'est
/// jamais bloqué pendant toute la durée d'une compaction — seulement pendant
/// la bascule finale, dont le coût est O(1)".
///
/// Design note: on this engine a single ordinary `put_memory` is itself not
/// free (real vector-index maintenance — tens of ms), comparable to or
/// larger than a moderate compaction merge's own cost (a plain KV
/// materialize-and-rewrite, cheap per entry). So neither "one read's latency
/// vs. an idle baseline" nor "aggregate throughput with vs. without
/// compaction" isolates the property under test at an affordable dataset
/// size — both are dominated by ordinary per-write lock contention (real,
/// expected, out of scope: writes stay exclusive and serialized, ADR-043
/// §4/§5), not by the compaction itself. The test instead isolates a single,
/// deliberately large compaction in its own window and counts how many
/// concurrent reads *complete* while that window is open:
///
/// 1. Seed a sizeable dataset spread across many SSTs with compaction
///    structurally disabled (`compaction_sst_threshold: usize::MAX`) — no
///    reader running, so this phase is only as slow as ordinary writes are.
/// 2. Reopen the same on-disk store with `compaction_sst_threshold: 1` and
///    start a background reader hammering `vector_ranking_ids`.
/// 3. Issue exactly one more `put_memory` — its flush immediately exceeds
///    the threshold, so this single call schedules exactly one compaction
///    covering everything phase 1 built up, big enough to take a genuinely
///    measurable slice of wall-clock time.
///
/// If that compaction's merge still held the write lock for its full
/// duration (the pre-J4 design), the reader would be excluded from
/// acquiring even a read lock for that whole window — zero reads could
/// complete while the triggering call is in flight. Under the ADR-043 §3/J4
/// design the merge runs under a shared read lock (concurrent with readers)
/// and only the O(1) commit is exclusive, so multiple reads should complete
/// during that window despite the compaction running.
#[tokio::test]
async fn native_reads_are_not_blocked_for_the_duration_of_a_concurrent_compaction() {
    use basemyai::MemoryLayer;
    use basemyai::storage::{MemoryStore, NativeMemoryStore};
    use basemyai::temporal::Validity;
    use basemyai_engine::EngineOptions;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    let dir = tempfile::tempdir().expect("tempdir");
    let agent = basemyai::AgentId::new("compaction-concurrency-agent").expect("agent id");
    let query = memory_tests::vec_for(7);

    // Phase 1: build up a dataset spread across many SSTs, sequentially, no
    // reader contention — deliberately sizeable so the single compaction
    // triggered in phase 3 covers enough real live data (memory record +
    // vector-index node + graph edges + FTS postings, all real, nothing
    // synthetic) to take a genuinely measurable slice of wall-clock time.
    {
        let seed_options = EngineOptions {
            memtable_flush_threshold: 60,
            compaction_sst_threshold: usize::MAX,
            ..EngineOptions::default()
        };
        let store = NativeMemoryStore::open_with_engine_options(dir.path(), seed_options).expect("open for seeding");
        const N: usize = 1200;
        for i in 0..N {
            let vector = memory_tests::vec_for((i % 251) as u8);
            store
                .put_memory(
                    &format!("m{i}"),
                    &agent,
                    MemoryLayer::Episodic,
                    &format!("mémoire numéro {i}"),
                    Validity::since(0),
                    &vector,
                    "user",
                    1.0,
                )
                .await
                .expect("seed");
        }
    } // dropped here: releases the writer lock before the reopen below.

    // Phase 2: reopen with threshold 1 — the very next flush guarantees a
    // compaction covering everything phase 1 built up.
    let compact_options = EngineOptions {
        memtable_flush_threshold: 1,
        compaction_sst_threshold: 1,
        ..EngineOptions::default()
    };
    let store = Arc::new(
        NativeMemoryStore::open_with_engine_options(dir.path(), compact_options).expect("reopen for compaction"),
    );

    let stop = Arc::new(AtomicBool::new(false));
    let reader_store = Arc::clone(&store);
    let reader_agent = agent.clone();
    let reader_query = query.clone();
    let reader_stop = Arc::clone(&stop);
    let reader = tokio::spawn(async move {
        let mut completions = Vec::new();
        while !reader_stop.load(Ordering::Relaxed) {
            reader_store
                .vector_ranking_ids(&reader_agent, &reader_query, 10, 0, true)
                .await
                .expect("read concurrent with compaction");
            completions.push(Instant::now());
        }
        completions
    });

    // Give the reader a moment to start hammering before the trigger, and a
    // moment after to keep sampling past it, without pinning the whole
    // window to a fixed sleep for correctness (only for giving the
    // background task room to run — the assertion below depends only on
    // real timestamps, not on this delay's exact length).
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Phase 3: the single triggering write — its own `with_inner` call
    // awaits the whole scheduled compaction before returning (see
    // `NativeMemoryStore::with_inner`'s doc), so `trigger_start..trigger_end`
    // brackets the compaction's real wall-clock window.
    let trigger_start = Instant::now();
    store
        .put_memory(
            "trigger",
            &agent,
            MemoryLayer::Episodic,
            "trigger",
            Validity::since(0),
            &memory_tests::vec_for(255),
            "user",
            1.0,
        )
        .await
        .expect("trigger write + compaction");
    let trigger_end = Instant::now();
    let trigger_duration = trigger_end - trigger_start;

    tokio::time::sleep(Duration::from_millis(20)).await;
    stop.store(true, Ordering::Relaxed);
    let completions = reader.await.expect("reader task");

    let reads_during_trigger = completions
        .iter()
        .filter(|&&t| t >= trigger_start && t <= trigger_end)
        .count();

    eprintln!(
        "native_reads_during_compaction: trigger write+compaction took {trigger_duration:?}, \
         {reads_during_trigger} reads completed concurrently during that window ({} total reads)",
        completions.len()
    );

    assert!(
        trigger_duration > Duration::from_millis(15),
        "the triggering write should have scheduled a genuinely measurable compaction (got \
         {trigger_duration:?}) — the seeded dataset may be too small for this test to be meaningful"
    );
    // A handful of reads already in flight when the trigger's write lock is
    // requested can still complete afterward under *either* design (they
    // don't need to re-acquire anything) — empirically, up to ~2 on this
    // dataset even when compaction is forced through a single exclusive
    // lock for its whole duration (verified by temporarily simulating that
    // pre-J4 shape locally: 2 reads completed in a comparable ~270ms
    // window). Genuine concurrency during the merge itself looks
    // completely different in scale — 36 reads completed in a comparable
    // ~256ms window under the real ADR-043 §3/J4 code path. `>= 6` sits
    // solidly between the two (3x above the blocked-design ceiling, 6x
    // below the concurrent-design floor observed), not a hair-thin margin.
    assert!(
        reads_during_trigger >= 6,
        "only {reads_during_trigger} read(s) completed while the triggering write's compaction was in \
         flight ({trigger_duration:?}) — looks like reads were blocked for (most of) the merge, not \
         just its brief commit"
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
