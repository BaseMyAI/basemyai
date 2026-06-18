//! Ouverture de mémoires par agent. Le serveur MCP sert plusieurs agents ; une
//! [`Memory`] est scellée par un `agent_id` à sa construction (ADR-006), donc le
//! pool en ouvre une par agent via un [`MemoryProvider`] injecté.
//!
//! Deux implémentations :
//! - [`EncryptedFileProvider`] (feature `crypto`) : production. Tous les agents
//!   partagent **un** fichier libSQL chiffré ; l'isolation reste garantie au
//!   niveau SQL par `agent_id`. L'embedder (Candle, lourd) est **partagé** via
//!   `Arc` — un seul modèle en mémoire pour tous les agents.
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

/// Provider de production : un fichier libSQL chiffré partagé, embedder partagé.
#[cfg(feature = "crypto")]
pub struct EncryptedFileProvider {
    store_path: std::path::PathBuf,
    key: basemyai_core::EncryptionKey,
    embedder: std::sync::Arc<dyn basemyai_core::Embedder>,
}

#[cfg(feature = "crypto")]
impl EncryptedFileProvider {
    /// Construit le provider à partir du chemin de base chiffrée, de la clé de
    /// chiffrement et d'un embedder partagé (résolu par le setup hardware-aware).
    #[must_use]
    pub fn new(
        store_path: std::path::PathBuf,
        key: basemyai_core::EncryptionKey,
        embedder: std::sync::Arc<dyn basemyai_core::Embedder>,
    ) -> Self {
        Self {
            store_path,
            key,
            embedder,
        }
    }
}

#[cfg(feature = "crypto")]
#[async_trait::async_trait]
impl MemoryProvider for EncryptedFileProvider {
    async fn open(&self, agent: AgentId) -> Result<Memory> {
        use basemyai::MemoryError;
        let store = basemyai_core::Store::open(&self.store_path, Some(self.key.clone()))
            .await
            .map_err(|e| crate::McpError::Memory(MemoryError::from(e)))?;
        let embedder = Box::new(SharedEmbedder(std::sync::Arc::clone(&self.embedder)));
        Ok(Memory::open(store, embedder, agent).await?)
    }
}

/// Adaptateur : expose un `Arc<dyn Embedder>` partagé comme un `Box<dyn Embedder>`
/// propre à chaque [`Memory`], sans cloner le modèle sous-jacent (Candle, lourd).
#[cfg(feature = "crypto")]
struct SharedEmbedder(std::sync::Arc<dyn basemyai_core::Embedder>);

#[cfg(feature = "crypto")]
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

/// Provider de test : base `:memory:` non chiffrée + embedder déterministe.
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
