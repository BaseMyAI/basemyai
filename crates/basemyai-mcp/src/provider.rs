// SPDX-License-Identifier: BUSL-1.1
//! Ouverture de mémoires par agent. Le serveur MCP sert plusieurs agents ; une
//! [`Memory`] est scellée par un `agent_id` à sa construction (ADR-006), donc le
//! pool en ouvre une par agent via un [`MemoryProvider`] injecté.
//!
//! Deux implémentations :
//! - [`FileProvider`] : production. Un store natif chiffré partagé
//!   (ADR-032) ; tous les agents partagent **un** store, l'isolation reste
//!   garantie structurellement par préfixe de clé. L'embedder (Candle,
//!   lourd) est **partagé** via `Arc` — un seul modèle en mémoire pour tous
//!   les agents.
//! - [`InMemoryProvider`] (feature `test-util`) : tests/spikes, sans CMake ni
//!   modèle.

use basemyai::{AgentId, Memory};

use crate::error::Result;

/// Ouvre la mémoire d'un agent donné. `Send + Sync` : partagé par le pool.
#[async_trait::async_trait]
pub trait MemoryProvider: Send + Sync {
    /// Ouvre (et migre si besoin) la mémoire de `agent`.
    ///
    /// # Errors
    /// [`crate::McpError::Memory`] si l'ouverture/migration échoue.
    async fn open(&self, agent: AgentId) -> Result<Memory>;
}

/// Provider de production (ADR-032) : un store natif chiffré partagé,
/// embedder partagé. Contrairement à un pool de connexions par agent, le
/// moteur natif est mono-écrivain exclusif (ADR-025) : le store est ouvert
/// **une seule fois** ici et partagé (`Arc`) entre tous les agents — jamais
/// rouvert par agent.
pub struct FileProvider {
    store: std::sync::Arc<basemyai::storage::NativeMemoryStore>,
    embedder: std::sync::Arc<dyn basemyai_core::Embedder>,
}

impl FileProvider {
    /// Ouvre (au besoin crée) un store natif chiffré à `store_path`.
    ///
    /// # Errors
    /// Erreur de stockage si l'ouverture (recovery WAL, chargement des méta
    /// d'index) échoue, ou si la clé est fausse.
    pub async fn open(
        store_path: std::path::PathBuf,
        key: basemyai_core::EncryptionKey,
        embedder: std::sync::Arc<dyn basemyai_core::Embedder>,
    ) -> Result<Self> {
        use basemyai::MemoryError;

        if let Some(parent) = store_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                crate::McpError::Memory(MemoryError::Core(basemyai_core::CoreError::Storage(format!(
                    "création du répertoire parent de '{}' : {e}",
                    store_path.display()
                ))))
            })?;
        }
        let path = store_path.clone();
        let store =
            tokio::task::spawn_blocking(move || basemyai::storage::NativeMemoryStore::open_with_key(&path, &key))
                .await
                .map_err(|e| {
                    crate::McpError::Memory(MemoryError::Core(basemyai_core::CoreError::Storage(format!(
                        "ouverture du store natif interrompue : {e}"
                    ))))
                })??;
        Ok(Self {
            store: std::sync::Arc::new(store),
            embedder,
        })
    }
}

#[async_trait::async_trait]
impl MemoryProvider for FileProvider {
    async fn open(&self, agent: AgentId) -> Result<Memory> {
        let embedder = Box::new(SharedEmbedder(std::sync::Arc::clone(&self.embedder)));
        Ok(Memory::from_native_store(std::sync::Arc::clone(&self.store), embedder, agent).await?)
    }
}

/// Adaptateur : expose un `Arc<dyn Embedder>` partagé comme un `Box<dyn Embedder>`
/// propre à chaque [`Memory`], sans cloner le modèle sous-jacent (Candle, lourd).
struct SharedEmbedder(std::sync::Arc<dyn basemyai_core::Embedder>);

impl basemyai_core::Embedder for SharedEmbedder {
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

/// Provider de test : base éphémère non chiffrée + embedder déterministe.
/// **Jamais en production** (vecteurs non sémantiques, mémoire éphémère).
#[cfg(feature = "test-util")]
#[derive(Default)]
pub struct InMemoryProvider;

#[cfg(feature = "test-util")]
impl InMemoryProvider {
    /// Construit le provider de test.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[cfg(feature = "test-util")]
#[async_trait::async_trait]
impl MemoryProvider for InMemoryProvider {
    async fn open(&self, agent: AgentId) -> Result<Memory> {
        Ok(Memory::open_in_memory(agent.as_str()).await?)
    }
}
