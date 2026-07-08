//! `.bmai` container metadata contract (moteur natif, ADR-033).

use basemyai::storage::BMAI_FORMAT_VERSION;
mod support;

#[tokio::test]
async fn native_store_writes_bmai_container_metadata() {
    let store = support::open_native_store();
    let meta = store.container_metadata().await.expect("metadata read");
    let get = |key: &str| meta.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());

    assert_eq!(get("format").as_deref(), Some("basemyai-memory"));
    assert_eq!(
        get("format_version").as_deref(),
        Some(BMAI_FORMAT_VERSION.to_string().as_str())
    );
    assert_eq!(get("storage_engine").as_deref(), Some("native"));
}
