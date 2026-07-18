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

#[test]
fn passphrase_mode_is_explicit_and_never_falls_back_to_raw_key() {
    let dir = tempfile::tempdir().expect("tempdir");
    let passphrase = basemyai_core::EncryptionKey::passphrase("human contract secret");
    NativeMemoryStore::open_with_key(dir.path(), &passphrase).expect("open passphrase store");

    let raw = basemyai_core::EncryptionKey::raw("human contract secret");
    let Err(err) = NativeMemoryStore::open_with_key(dir.path(), &raw) else {
        panic!("raw key must not open passphrase store");
    };
    assert!(matches!(
        err,
        basemyai::MemoryError::Core(basemyai_core::CoreError::WrongEncryptionKey)
    ));
}
