//! Fail-fast : libSQL compile sur Windows ? roundtrip + vecteur natif en mémoire ?

use libsql::Builder;

#[tokio::test]
async fn libsql_builds_and_native_vector_works() {
    let db = Builder::new_local(":memory:").build().await.expect("build in-memory db");
    let conn = db.connect().expect("connect");

    conn.execute_batch(
        "CREATE TABLE item (id INTEGER PRIMARY KEY, emb F32_BLOB(3));\n\
         CREATE INDEX item_idx ON item(libsql_vector_idx(emb, 'metric=cosine'));",
    )
    .await
    .expect("schema + native vector index");

    conn.execute("INSERT INTO item (id, emb) VALUES (1, vector('[1,2,3]'))", ())
        .await
        .expect("insert vector");
    conn.execute("INSERT INTO item (id, emb) VALUES (2, vector('[9,9,9]'))", ())
        .await
        .expect("insert vector 2");

    let mut rows = conn
        .query(
            "SELECT id FROM vector_top_k('item_idx', vector('[1,2,3]'), 1)",
            (),
        )
        .await
        .expect("vector_top_k query");
    let row = rows.next().await.expect("row result").expect("at least one row");
    let id: i64 = row.get(0).expect("get id");
    assert_eq!(id, 1, "le plus proche de [1,2,3] doit être l'item 1");
}
