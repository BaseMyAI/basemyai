//! Contrats du socle : types stables, indépendants des backends natifs.

use basemyai_core::{Device, EncryptionKey, EngineKind, Filter, StorageEngine, Store, Value};

#[test]
fn device_defaults_to_cpu() {
    assert_eq!(Device::default(), Device::Cpu);
}

#[test]
fn encryption_key_debug_never_leaks_the_secret() {
    let key = EncryptionKey::new("super-secret-value");
    let shown = format!("{key:?}");
    assert!(!shown.contains("super-secret-value"), "le secret a fuité dans Debug");
    assert_eq!(shown, "EncryptionKey(***)");
}

#[test]
fn filter_carries_parameterized_clause() {
    let filter = Filter::new("col = ?", vec![Value::Integer(7)]);
    assert_eq!(filter.where_sql, "col = ?");
    assert_eq!(filter.params.len(), 1);
}

#[tokio::test]
async fn store_reports_libsql_capabilities() {
    let store = Store::open_in_memory().await.expect("store opens");
    let caps = store.capabilities();

    assert_eq!(caps.kind, EngineKind::Libsql);
    assert!(caps.vectors);
    assert!(caps.full_text);
    assert!(caps.recursive_queries);
    assert!(caps.transactions);
    assert!(!caps.encrypted);
}
