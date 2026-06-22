//! Tests du **pool de lecteurs** libSQL (ADR-020, hardening M6) : lectures
//! concurrentes sur un store fichier (WAL), lectures pendant une écriture en
//! cours, dégénérescence en mémoire, et présence du side-car `-wal`.
//!
//! Pilotés via [`Store`] (+ [`LibsqlMemoryStore`] pour le chemin sémantique),
//! comme `storage_contract.rs`. Note : `RUST_TEST_THREADS=1`
//! (`.cargo/config.toml`) sérialise les *tests* entre eux, mais **pas** les
//! tâches `tokio` d'un même test — la concurrence testée ici est interne au
//! runtime async, exactement le chemin du pool en production.

use std::path::PathBuf;
use std::sync::Arc;

use basemyai::storage::{LibsqlMemoryStore, MemoryStore};
use basemyai::temporal::Validity;
use basemyai::{AgentId, MemoryLayer};
use basemyai_core::{Filter, Metric, Store, Value};
use tokio::task::JoinSet;

fn agent(id: &str) -> AgentId {
    AgentId::new(id).expect("non-empty agent id")
}

fn now() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).expect("clock").as_secs()).expect("fits i64")
}

fn temp_db_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("basemyai-{name}-{}-{}.db", std::process::id(), now()))
}

/// Vecteur déterministe à la dimension du schéma (`EMBEDDING_DIM`).
fn vec_for(seed: u8) -> Vec<f32> {
    let dim = basemyai::EMBEDDING_DIM;
    let mut v = vec![0.0_f32; dim];
    v[usize::from(seed) % dim] = 1.0;
    v[0] += 0.001;
    v
}

/// Filtre `WHERE` agent (sans validité — suffisant ici), pour piloter
/// `vector_knn` du core directement.
fn agent_filter(agent: &AgentId) -> Filter {
    Filter::new("agent_id = ?", vec![Value::Text(agent.as_str().to_string())])
}

async fn migrated_file_store(path: &std::path::Path) -> Store {
    let store = Store::open(path, None).await.expect("open file store");
    store.migrate(&basemyai::schema()).await.expect("migrate");
    store
}

/// Nettoie le fichier de base + ses side-cars WAL/SHM en fin de test.
fn cleanup(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(path.with_extension("db-wal"));
    let _ = std::fs::remove_file(path.with_extension("db-shm"));
}

/// ~64 lectures `recall_vector` concurrentes sur un store fichier (pool actif)
/// renvoient toutes le résultat attendu, sans panique ni erreur.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_reads_on_file_store_all_succeed() {
    let path = temp_db_path("pool-concurrent-reads");
    let engine = Arc::new(LibsqlMemoryStore::new(migrated_file_store(&path).await));
    let a = agent("a");

    for i in 0..8_u8 {
        engine
            .put_memory(
                &format!("m{i}"),
                &a,
                MemoryLayer::Episodic,
                &format!("contenu {i}"),
                Validity::since(0),
                &vec_for(i + 1),
                "user",
            )
            .await
            .expect("put");
    }

    let mut tasks = JoinSet::new();
    for _ in 0..64 {
        let engine = Arc::clone(&engine);
        let a = a.clone();
        tasks.spawn(async move {
            engine
                .recall_vector(&a, &vec_for(1), 5, None, Metric::Cosine, 0)
                .await
                .expect("concurrent recall")
        });
    }

    let mut count = 0;
    while let Some(res) = tasks.join_next().await {
        let got = res.expect("task join");
        assert!(!got.is_empty(), "every concurrent read returns results");
        assert!(got.iter().any(|r| r.id == "m0"), "nearest neighbour present");
        count += 1;
    }
    assert_eq!(count, 64);

    cleanup(&path);
}

/// Sous WAL, les lectures réussissent pendant qu'une transaction d'écriture est
/// ouverte (writer non commité) — le pool lecteur ne se bloque pas sur le writer.
/// On pilote `Store` directement pour tenir le `WriteTxn` ouvert pendant la lecture.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reads_succeed_while_write_txn_in_progress() {
    let path = temp_db_path("pool-read-during-write");
    let store = Arc::new(migrated_file_store(&path).await);
    let a = agent("a");

    // Seed une ligne via le chemin sémantique (écriture committée).
    {
        let engine = LibsqlMemoryStore::new(migrated_file_store(&path).await);
        engine
            .put_memory(
                "seed",
                &a,
                MemoryLayer::Episodic,
                "graine",
                Validity::since(0),
                &vec_for(1),
                "user",
            )
            .await
            .expect("seed put");
    }

    // Ouvre une transaction writer et garde-la ouverte (non committée).
    let txn = store.begin_write().await.expect("begin write");

    // Pendant ce temps, une lecture via le pool (reader()) doit réussir.
    let neighbors = store
        .vector_knn("memory", &vec_for(1), 5, Some(&agent_filter(&a)))
        .await
        .expect("read during open write txn");
    assert!(
        neighbors.iter().any(|n| n.id == "seed"),
        "la lecture pendant une ecriture ouverte voit la donnee committee"
    );

    txn.commit().await.expect("commit");
    cleanup(&path);
}

/// Le store **en mémoire** (pool dégénéré, `readers` vide) reste fonctionnel
/// de bout en bout : `reader()` retombe sur le writer partagé.
#[tokio::test]
async fn in_memory_store_degenerate_pool_roundtrips() {
    let store = Store::open_in_memory().await.expect("open in memory");
    store.migrate(&basemyai::schema()).await.expect("migrate");
    let engine = LibsqlMemoryStore::new(store);
    let a = agent("a");

    engine
        .put_memory(
            "m1",
            &a,
            MemoryLayer::Episodic,
            "bonjour",
            Validity::since(0),
            &vec_for(1),
            "user",
        )
        .await
        .expect("put");

    let got = engine
        .recall_vector(&a, &vec_for(1), 5, None, Metric::Cosine, 0)
        .await
        .expect("recall");
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].id, "m1");
}

/// Après une écriture sur un store fichier en WAL, le side-car `-wal` existe.
#[tokio::test]
async fn wal_sidecar_exists_after_write() {
    let path = temp_db_path("pool-wal-sidecar");
    let engine = LibsqlMemoryStore::new(migrated_file_store(&path).await);
    let a = agent("a");
    engine
        .put_memory(
            "m1",
            &a,
            MemoryLayer::Episodic,
            "x",
            Validity::since(0),
            &vec_for(1),
            "user",
        )
        .await
        .expect("put");

    let wal = path.with_extension("db-wal");
    assert!(wal.exists(), "le side-car WAL doit exister apres une ecriture: {wal:?}");

    cleanup(&path);
}
