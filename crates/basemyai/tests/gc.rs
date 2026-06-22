//! Tests d'intégration du GC des souvenirs expirés et de son miroir FTS.

use basemyai::ExpiredMemoryGc;
use basemyai_core::{MaintenanceTask, Store};

async fn insert(store: &Store, id: &str, agent: &str, valid_until: Option<i64>) {
    let conn = store.connect();
    conn.execute(
        "INSERT INTO memory \
         (id, agent_id, layer, content, valid_from, valid_until, emb, importance, last_access) \
         VALUES (?1, ?2, 'semantic', ?1, 0, ?3, NULL, 0, NULL)",
        basemyai_core::libsql::params![id, agent, valid_until],
    )
    .await
    .expect("insert memory");
    conn.execute(
        "INSERT INTO memory_fts (id, agent_id, content) VALUES (?1, ?2, ?1)",
        basemyai_core::libsql::params![id, agent],
    )
    .await
    .expect("insert memory_fts");
}

async fn table_count(store: &Store, table: &str, id: &str) -> i64 {
    let conn = store.connect();
    let sql = format!("SELECT COUNT(*) FROM {table} WHERE id = ?1");
    let mut rows = conn
        .query(&sql, basemyai_core::libsql::params![id])
        .await
        .expect("query count");
    let row = rows.next().await.expect("row").expect("one count row");
    row.get::<i64>(0).expect("count")
}

fn now_unix() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).expect("clock").as_secs()).expect("fits i64")
}

#[tokio::test]
async fn expired_gc_deletes_matching_fts_rows_atomically() {
    let store = Store::open_in_memory().await.expect("open");
    store.migrate(&basemyai::schema()).await.expect("migrate");

    let now = now_unix();
    insert(&store, "expired", "a", Some(now - 10)).await;
    insert(&store, "live", "a", Some(now + 10_000)).await;
    insert(&store, "forever", "a", None).await;

    ExpiredMemoryGc.run(&store).await.expect("gc");

    assert_eq!(table_count(&store, "memory", "expired").await, 0);
    assert_eq!(table_count(&store, "memory_fts", "expired").await, 0);
    assert_eq!(table_count(&store, "memory", "live").await, 1);
    assert_eq!(table_count(&store, "memory_fts", "live").await, 1);
    assert_eq!(table_count(&store, "memory", "forever").await, 1);
    assert_eq!(table_count(&store, "memory_fts", "forever").await, 1);
}
