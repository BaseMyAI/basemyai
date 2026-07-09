//! Helpers de tests d'intégration — ouverture de stores natifs via
//! `test-util` (`open` / `open_encrypted` / `open_ephemeral`).

#![allow(dead_code)]

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

const DIM: usize = 384;

/// Embedder déterministe sans modèle — pour les tests d'intégration mémoire.
pub(crate) struct FakeEmbedder;

impl FakeEmbedder {
    pub(crate) fn vec_for(text: &str) -> Vec<f32> {
        let mut v = vec![0.0_f32; DIM];
        for (i, b) in text.bytes().enumerate() {
            v[i % DIM] += f32::from(b) + 1.0;
        }
        v[0] += 1.0;
        v
    }
}

impl basemyai_core::Embedder for FakeEmbedder {
    fn embed(&self, text: &str) -> basemyai_core::Result<Vec<f32>> {
        Ok(Self::vec_for(text))
    }

    fn embed_batch(&self, texts: &[String]) -> basemyai_core::Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| Self::vec_for(t)).collect())
    }

    fn model_id(&self) -> &str {
        "fake-deterministic"
    }

    fn dim(&self) -> usize {
        DIM
    }
}

pub(crate) fn agent(id: &str) -> basemyai::AgentId {
    basemyai::AgentId::new(id).expect("non-empty agent id")
}
