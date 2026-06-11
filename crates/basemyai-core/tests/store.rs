//! Store libSQL réel : migrations idempotentes, roundtrip, vecteur natif.

use basemyai_core::{libsql, EncryptionKey, Filter, Migration, Store, Value};

const SCHEMA: [Migration; 1] = [Migration {
    version: 1,
    up_sql: "CREATE TABLE note (id INTEGER PRIMARY KEY, body TEXT NOT NULL);",
}];

const KEEP_SCHEMA: [Migration; 1] = [Migration {
    version: 1,
    up_sql: "CREATE TABLE keep_flag (id TEXT PRIMARY KEY, keep INTEGER NOT NULL);",
}];

#[tokio::test]
async fn migrate_applies_schema_and_is_idempotent() {
    let store = Store::open_in_memory().await.expect("open in-memory");
    store.migrate(&SCHEMA).await.expect("first migrate");
    store.migrate(&SCHEMA).await.expect("re-migrate is a no-op");

    let conn = store.connect();
    conn.execute("INSERT INTO note (body) VALUES ('hello')", ()).await.expect("insert");
    let mut rows = conn.query("SELECT body FROM note WHERE id = 1", ()).await.expect("select");
    let row = rows.next().await.expect("row").expect("one row");
    let body: String = row.get(0).expect("get body");
    assert_eq!(body, "hello");
}

#[tokio::test]
async fn native_vector_knn_returns_nearest() {
    let store = Store::open_in_memory().await.expect("open");
    store.ensure_vector_table("emb", 3).await.expect("vector table");
    store.vector_upsert("emb", "a", &[1.0, 2.0, 3.0]).await.expect("upsert a");
    store.vector_upsert("emb", "b", &[9.0, 9.0, 9.0]).await.expect("upsert b");

    let hits = store.vector_knn("emb", &[1.0, 2.0, 3.0], 1, None).await.expect("knn");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, "a", "le plus proche de [1,2,3] est 'a'");
}

#[tokio::test]
async fn knn_reports_real_cosine_distance() {
    let store = Store::open_in_memory().await.expect("open");
    store.ensure_vector_table("emb", 3).await.expect("vector table");
    store.vector_upsert("emb", "same", &[1.0, 0.0, 0.0]).await.expect("upsert same");
    store.vector_upsert("emb", "orth", &[0.0, 1.0, 0.0]).await.expect("upsert orth");

    let hits = store.vector_knn("emb", &[1.0, 0.0, 0.0], 2, None).await.expect("knn");
    assert_eq!(hits.len(), 2);
    // Trié par distance croissante : le vecteur identique d'abord (~0), l'orthogonal ensuite (~1).
    assert_eq!(hits[0].id, "same");
    assert!(hits[0].distance < 0.001, "distance au vecteur identique ~0, vu {}", hits[0].distance);
    assert!(hits[1].distance > 0.9, "distance à l'orthogonal ~1, vu {}", hits[1].distance);
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
        store.vector_upsert("emb", &id, &[1.0 - drift, drift, 0.0]).await.expect("upsert");
        let keep = i64::from(i >= 10);
        store
            .connect()
            .execute("INSERT INTO keep_flag (id, keep) VALUES (?1, ?2)", libsql::params![id, keep])
            .await
            .expect("flag");
    }

    let filter = Filter::new(
        "t.id IN (SELECT id FROM keep_flag WHERE keep = ?)",
        vec![Value::Integer(1)],
    );
    let hits = store.vector_knn("emb", &[1.0, 0.0, 0.0], 5, Some(&filter)).await.expect("knn");
    assert_eq!(hits.len(), 5, "le sur-échantillonnage doit garantir k=5 malgré le filtre");
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
        let store = Store::open(&path, Some(EncryptionKey::new("correct-horse"))).await.expect("open encrypted");
        store.ensure_vector_table("emb", 3).await.expect("table");
        store.vector_upsert("emb", "a", &[1.0, 2.0, 3.0]).await.expect("upsert");
    }

    // Rouvre avec la bonne clé : la donnée est relisible.
    {
        let store = Store::open(&path, Some(EncryptionKey::new("correct-horse"))).await.expect("reopen ok");
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
        assert!(!usable, "une mauvaise clé ne doit jamais permettre de lire la base chiffrée");
    }

    let _ = std::fs::remove_file(&path);
}
