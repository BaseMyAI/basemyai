//! `.bmai` container metadata contract.

use basemyai::schema;
use basemyai_core::Store;

#[tokio::test]
async fn schema_writes_bmai_container_metadata() {
    let store = Store::open_in_memory().await.expect("store opens");
    store.migrate(&schema()).await.expect("schema migrates");

    let conn = store.connect();
    let mut rows = conn
        .query(
            "SELECT key, value FROM bmai_meta WHERE key IN ('format', 'format_version', 'storage_engine')",
            (),
        )
        .await
        .expect("metadata query succeeds");

    let mut values = std::collections::BTreeMap::new();
    while let Some(row) = rows.next().await.expect("row reads") {
        values.insert(
            row.get::<String>(0).expect("key column"),
            row.get::<String>(1).expect("value column"),
        );
    }

    assert_eq!(values.get("format").map(String::as_str), Some("basemyai-memory"));
    assert_eq!(values.get("format_version").map(String::as_str), Some("1"));
    assert_eq!(values.get("storage_engine").map(String::as_str), Some("libsql"));
}
