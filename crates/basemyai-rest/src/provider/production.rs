// SPDX-License-Identifier: BUSL-1.1
//! Provider de production (ADR-032) : un store natif chiffré partagé,
//! embedder partagé. Contrairement à un pool de connexions par agent, le
//! moteur natif est mono-écrivain exclusif (ADR-025) : le store est ouvert
//! **une seule fois** ici et partagé (`Arc`) entre tous les agents — jamais
//! rouvert par agent (la séparation par agent reste la responsabilité de
//! `context::MemoryRegistry`, en amont).

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use basemyai::{AgentId, Memory};
use basemyai_core::{Embedder, EncryptionKey};

use super::MemoryProvider;
use super::error::ProviderError;

pub struct FileProvider {
    store: Arc<basemyai::storage::NativeMemoryStore>,
    embedder: Arc<dyn Embedder>,
}

impl FileProvider {
    /// Ouvre (au besoin crée) un store natif chiffré à `store_path`.
    ///
    /// # Errors
    /// [`ProviderError::DataDirectory`] si le répertoire parent ne peut être
    /// créé ; [`ProviderError::Memory`] si l'ouverture (recovery WAL,
    /// chargement des méta d'index) échoue ou si la clé est fausse.
    pub async fn open(
        store_path: PathBuf,
        key: EncryptionKey,
        embedder: Arc<dyn Embedder>,
    ) -> Result<Self, ProviderError> {
        if let Some(parent) = store_path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| ProviderError::DataDirectory {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let path = store_path.clone();
        let store =
            tokio::task::spawn_blocking(move || basemyai::storage::NativeMemoryStore::open_with_key(&path, &key))
                .await
                .map_err(|e| {
                    ProviderError::Memory(basemyai::MemoryError::Core(basemyai_core::CoreError::Storage(format!(
                        "native store open interrupted: {e}"
                    ))))
                })??;
        Ok(Self {
            store: Arc::new(store),
            embedder,
        })
    }
}

#[async_trait]
impl MemoryProvider for FileProvider {
    async fn open(&self, agent: AgentId) -> Result<Memory, basemyai::MemoryError> {
        let embedder = Box::new(SharedEmbedder(Arc::clone(&self.embedder)));
        Memory::from_native_store(Arc::clone(&self.store), embedder, agent).await
    }
}

/// Adaptateur : `Arc<dyn Embedder>` partagé vu comme `Box<dyn Embedder>` par
/// chaque [`Memory`], sans cloner le modèle sous-jacent.
struct SharedEmbedder(Arc<dyn Embedder>);

impl Embedder for SharedEmbedder {
    fn embed(&self, text: &str) -> basemyai_core::Result<Vec<f32>> {
        self.0.embed(text)
    }
    fn embed_batch(&self, texts: &[String]) -> basemyai_core::Result<Vec<Vec<f32>>> {
        self.0.embed_batch(texts)
    }
    fn model_id(&self) -> &str {
        self.0.model_id()
    }
    fn dim(&self) -> usize {
        self.0.dim()
    }
}

/// Provider de test : base éphémère non chiffrée + embedder déterministe
/// (ni CMake ni Candle). **Jamais en production.**
#[cfg(feature = "test-util")]
#[derive(Default)]
pub struct InMemoryProvider;

#[cfg(feature = "test-util")]
impl InMemoryProvider {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[cfg(feature = "test-util")]
#[async_trait]
impl MemoryProvider for InMemoryProvider {
    async fn open(&self, agent: AgentId) -> Result<Memory, basemyai::MemoryError> {
        Memory::open_in_memory(agent.as_str()).await
    }
}
