//! Contrat chiffrement : `open` persistant n'existe qu'avec `test-util`.

mod support;

use basemyai::storage::NativeMemoryStore;

#[cfg(feature = "test-util")]
#[test]
fn plaintext_open_available_only_with_test_util() {
    let dir = tempfile::tempdir().expect("tempdir");
    NativeMemoryStore::open(dir.path()).expect("open gated behind test-util");
}

#[test]
fn open_encrypted_is_always_available() {
    let dir = tempfile::tempdir().expect("tempdir");
    NativeMemoryStore::open_encrypted(dir.path(), "contract-test-key").expect("open_encrypted is the production API");
}
