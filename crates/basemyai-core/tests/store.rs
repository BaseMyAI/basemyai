//! Store libSQL réel : migrations idempotentes, roundtrip, vecteur natif.

use basemyai_core::{EncryptionKey, Filter, Metric, Migration, Store, Value, libsql};

const SCHEMA: [Migration; 1] = [Migration {
    version: 1,
    up_sql: "CREATE TABLE note (id INTEGER PRIMARY KEY, body TEXT NOT NULL);",
}];

const KEEP_SCHEMA: [Migration; 1] = [Migration {
    version: 1,
    up_sql: "CREATE TABLE keep_flag (id TEXT PRIMARY KEY, keep INTEGER NOT NULL);",
}];

const BROKEN_SCHEMA: [Migration; 1] = [Migration {
    version: 1,
    up_sql: "CREATE TABLE partial_migration (id INTEGER PRIMARY KEY); SELECT * FROM missing_table;",
}];

fn now() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).expect("clock").as_secs()).expect("fits i64")
}

fn temp_db_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("basemyai-core-{name}-{}-{}.db", std::process::id(), now()))
}

fn cleanup(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(path.with_extension("db-wal"));
    let _ = std::fs::remove_file(path.with_extension("db-shm"));
}

#[tokio::test]
async fn migrate_applies_schema_and_is_idempotent() {
    let store = Store::open_in_memory().await.expect("open in-memory");
    store.migrate(&SCHEMA).await.expect("first migrate");
    store.migrate(&SCHEMA).await.expect("re-migrate is a no-op");

    let conn = store.connect();
    conn.execute("INSERT INTO note (body) VALUES ('hello')", ())
        .await
        .expect("insert");
    let mut rows = conn
        .query("SELECT body FROM note WHERE id = 1", ())
        .await
        .expect("select");
    let row = rows.next().await.expect("row").expect("one row");
    let body: String = row.get(0).expect("get body");
    assert_eq!(body, "hello");
}

#[tokio::test]
async fn concurrent_cold_migrations_apply_once() {
    let path = temp_db_path("concurrent-migrate");
    let store_a = Store::open(&path, None).await.expect("open A");
    let store_b = Store::open(&path, None).await.expect("open B");

    let (a, b) = tokio::join!(store_a.migrate(&SCHEMA), store_b.migrate(&SCHEMA));
    a.expect("migrate A");
    b.expect("migrate B");

    let conn = store_a.connect();
    let mut rows = conn
        .query("SELECT COUNT(*) FROM _schema_version WHERE version = 1", ())
        .await
        .expect("count schema version");
    let row = rows.next().await.expect("row").expect("one row");
    let count: i64 = row.get(0).expect("count");
    assert_eq!(count, 1, "concurrent cold migration must record the version once");

    cleanup(&path);
}

#[tokio::test]
async fn failed_migration_rolls_back_ddl_and_version() {
    let store = Store::open_in_memory().await.expect("open");
    let err = store.migrate(&BROKEN_SCHEMA).await;
    assert!(err.is_err(), "broken migration must fail");

    let conn = store.connect();
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'partial_migration'",
            (),
        )
        .await
        .expect("query sqlite_master");
    let row = rows.next().await.expect("row").expect("one row");
    let table_count: i64 = row.get(0).expect("count");
    assert_eq!(table_count, 0, "DDL from a failed migration must roll back");

    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = '_schema_version'",
            (),
        )
        .await
        .expect("query schema version table");
    let row = rows.next().await.expect("row").expect("one row");
    let schema_version_tables: i64 = row.get(0).expect("count");
    assert_eq!(
        schema_version_tables, 0,
        "failed migration must not persist schema bookkeeping"
    );
}

#[tokio::test]
async fn native_vector_knn_returns_nearest() {
    let store = Store::open_in_memory().await.expect("open");
    store.ensure_vector_table("emb", 3).await.expect("vector table");
    store
        .vector_upsert("emb", "a", &[1.0, 2.0, 3.0])
        .await
        .expect("upsert a");
    store
        .vector_upsert("emb", "b", &[9.0, 9.0, 9.0])
        .await
        .expect("upsert b");

    let hits = store.vector_knn("emb", &[1.0, 2.0, 3.0], 1, None).await.expect("knn");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, "a", "le plus proche de [1,2,3] est 'a'");
}

#[tokio::test]
async fn euclidean_metric_reranks_by_l2_distance() {
    let store = Store::open_in_memory().await.expect("open");
    store.ensure_vector_table("emb", 3).await.expect("vector table");
    // 'near' colinéaire à la query (cosinus identique) mais plus loin en L2 ;
    // 'exact' est la query elle-même. L'euclidienne doit préférer 'exact'.
    store
        .vector_upsert("emb", "exact", &[1.0, 0.0, 0.0])
        .await
        .expect("up exact");
    store
        .vector_upsert("emb", "near", &[5.0, 0.0, 0.0])
        .await
        .expect("up near");
    store
        .vector_upsert("emb", "far", &[0.0, 1.0, 0.0])
        .await
        .expect("up far");

    let hits = store
        .vector_knn_metric("emb", &[1.0, 0.0, 0.0], 2, None, Metric::Euclidean)
        .await
        .expect("euclidean knn");
    assert_eq!(hits[0].id, "exact", "L2 : le vecteur identique est le plus proche");
    assert!(hits[0].distance < hits[1].distance, "distances triées croissantes");
}

#[tokio::test]
async fn hamming_metric_counts_sign_differences() {
    let store = Store::open_in_memory().await.expect("open");
    store.ensure_vector_table("emb", 3).await.expect("vector table");
    store
        .vector_upsert("emb", "same_signs", &[2.0, 3.0, 4.0])
        .await
        .expect("up same");
    store
        .vector_upsert("emb", "one_flip", &[-1.0, 3.0, 4.0])
        .await
        .expect("up flip");

    let hits = store
        .vector_knn_metric("emb", &[1.0, 1.0, 1.0], 2, None, Metric::Hamming)
        .await
        .expect("hamming knn");
    // Query toute positive : 'same_signs' (0 diff) avant 'one_flip' (1 diff).
    assert_eq!(hits[0].id, "same_signs");
    assert_eq!(hits[0].distance, 0.0);
    assert_eq!(hits[1].distance, 1.0);
}

#[tokio::test]
async fn knn_reports_real_cosine_distance() {
    let store = Store::open_in_memory().await.expect("open");
    store.ensure_vector_table("emb", 3).await.expect("vector table");
    store
        .vector_upsert("emb", "same", &[1.0, 0.0, 0.0])
        .await
        .expect("upsert same");
    store
        .vector_upsert("emb", "orth", &[0.0, 1.0, 0.0])
        .await
        .expect("upsert orth");

    let hits = store.vector_knn("emb", &[1.0, 0.0, 0.0], 2, None).await.expect("knn");
    assert_eq!(hits.len(), 2);
    // Trié par distance croissante : le vecteur identique d'abord (~0), l'orthogonal ensuite (~1).
    assert_eq!(hits[0].id, "same");
    assert!(
        hits[0].distance < 0.001,
        "distance au vecteur identique ~0, vu {}",
        hits[0].distance
    );
    assert!(
        hits[1].distance > 0.9,
        "distance à l'orthogonal ~1, vu {}",
        hits[1].distance
    );
}

#[tokio::test]
async fn knn_filter_oversamples_to_return_k() {
    let store = Store::open_in_memory().await.expect("open");
    store.migrate(&KEEP_SCHEMA).await.expect("schema");
    store.ensure_vector_table("emb", 3).await.expect("vector table");

    // 20 vecteurs ; les plus proches de la requête sont marqués keep=0 (à exclure),
    // les bons candidats keep=1 sont volontairement plus loin. Sans sur-échantillonnage,
    // le top-k natif ne ramènerait que des keep=0 et le filtre viderait le résultat.
    for i in 0..20i64 {
        let id = format!("v{i}");
        let drift = (i as f32) * 0.01;
        store
            .vector_upsert("emb", &id, &[1.0 - drift, drift, 0.0])
            .await
            .expect("upsert");
        let keep = i64::from(i >= 10);
        store
            .connect()
            .execute(
                "INSERT INTO keep_flag (id, keep) VALUES (?1, ?2)",
                libsql::params![id, keep],
            )
            .await
            .expect("flag");
    }

    let filter = Filter::new(
        "t.id IN (SELECT id FROM keep_flag WHERE keep = ?)",
        vec![Value::Integer(1)],
    );
    let hits = store
        .vector_knn("emb", &[1.0, 0.0, 0.0], 5, Some(&filter))
        .await
        .expect("knn");
    assert_eq!(
        hits.len(),
        5,
        "le sur-échantillonnage doit garantir k=5 malgré le filtre"
    );
    for h in &hits {
        let n: i64 = h.id.trim_start_matches('v').parse().expect("id");
        assert!(n >= 10, "seuls les keep=1 (v10..v19) doivent passer, vu {}", h.id);
    }
}

#[tokio::test]
async fn invalid_table_identifier_is_rejected() {
    let store = Store::open_in_memory().await.expect("open");
    assert!(store.ensure_vector_table("emb; DROP TABLE x", 3).await.is_err());
}

/// Sans la feature `crypto`, fournir une clé doit échouer proprement.
#[cfg(not(feature = "crypto"))]
#[tokio::test]
async fn encryption_requires_crypto_feature() {
    let path = std::env::temp_dir().join("basemyai-enc-pending.db");
    let result = Store::open(&path, Some(EncryptionKey::new("k"))).await;
    assert!(result.is_err(), "le chiffrement exige la feature `crypto` (CMake)");
}

/// Avec la feature `crypto` : roundtrip chiffré au repos. La bonne clé relit la
/// base ; une mauvaise clé ne peut pas la déchiffrer.
#[cfg(feature = "crypto")]
#[tokio::test]
async fn encrypted_roundtrip_and_wrong_key_fails() {
    let path = std::env::temp_dir().join("basemyai-core-crypto-roundtrip.db");
    let _ = std::fs::remove_file(&path);

    // Écrit un vecteur sous la clé correcte, puis ferme.
    {
        let store = Store::open(&path, Some(EncryptionKey::new("correct-horse")))
            .await
            .expect("open encrypted");
        store.ensure_vector_table("emb", 3).await.expect("table");
        store.vector_upsert("emb", "a", &[1.0, 2.0, 3.0]).await.expect("upsert");
    }

    // Rouvre avec la bonne clé : la donnée est relisible.
    {
        let store = Store::open(&path, Some(EncryptionKey::new("correct-horse")))
            .await
            .expect("reopen ok");
        let hits = store.vector_knn("emb", &[1.0, 2.0, 3.0], 1, None).await.expect("knn");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "a");
    }

    // Rouvre avec une mauvaise clé : impossible de lire la base chiffrée
    // (échec à l'ouverture ou à la première requête).
    {
        let usable = match Store::open(&path, Some(EncryptionKey::new("wrong-key"))).await {
            Ok(store) => store.vector_knn("emb", &[1.0, 2.0, 3.0], 1, None).await.is_ok(),
            Err(_) => false,
        };
        assert!(
            !usable,
            "une mauvaise clé ne doit jamais permettre de lire la base chiffrée"
        );
    }

    let _ = std::fs::remove_file(&path);
}
