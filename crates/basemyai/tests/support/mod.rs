//! Helpers de tests d'intégration pour ouvrir un store natif via l'API
//! production (`open` / `open_encrypted`) sans dépendre des helpers
//! `test-util`.

use std::sync::{LazyLock, Mutex};

use basemyai::storage::NativeMemoryStore;

static TEMP_DIR_GUARDS: LazyLock<Mutex<Vec<tempfile::TempDir>>> = LazyLock::new(|| Mutex::new(Vec::new()));

fn keep_tempdir_alive(dir: tempfile::TempDir) {
    // Garde les répertoires temporaires vivants jusqu'à la fin du processus
    // de test pour éviter leur suppression pendant que le store est encore
    // ouvert.
    TEMP_DIR_GUARDS.lock().expect("tempdir guard mutex poisoned").push(dir);
}

pub(crate) fn open_native_store() -> NativeMemoryStore {
    let dir = tempfile::tempdir().expect("create tempdir");
    let store = NativeMemoryStore::open(dir.path()).expect("open native store");
    keep_tempdir_alive(dir);
    store
}

#[allow(dead_code)]
pub(crate) fn open_encrypted_native_store(key: &str) -> NativeMemoryStore {
    let dir = tempfile::tempdir().expect("create tempdir");
    let store = NativeMemoryStore::open_encrypted(dir.path(), key).expect("open encrypted native store");
    keep_tempdir_alive(dir);
    store
}
