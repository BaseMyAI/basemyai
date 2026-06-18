//! Ouverture de mémoires par agent (même rôle que dans le serveur MCP). Le pool
//! du sidecar en ouvre une par `agent_id` via un [`MemoryProvider`] injecté.

use basemyai::{AgentId, Memory};

/// Ouvre la mémoire d'un agent. `Send + Sync` : partagé par l'état applicatif.
#[async_trait::async_trait]
pub trait MemoryProvider: Send + Sync {
    /// Ouvre (et migre si besoin) la mémoire de `agent`.
    ///
    /// # Errors
    /// [`basemyai::MemoryError`] si l'ouverture/migration échoue.
    async fn open(&self, agent: AgentId) -> Result<Memory, basemyai::MemoryError>;
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
    /// Construit le provider (chemin base chiffrée, clé, embedder partagé).
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
    async fn open(&self, agent: AgentId) -> Result<Memory, basemyai::MemoryError> {
        let store = basemyai_core::Store::open(&self.store_path, Some(self.key.clone()))
            .await
            .map_err(basemyai::MemoryError::from)?;
        Memory::open(
            store,
            Box::new(SharedEmbedder(std::sync::Arc::clone(&self.embedder))),
            agent,
        )
        .await
    }
}

/// Adaptateur : `Arc<dyn Embedder>` partagé vu comme `Box<dyn Embedder>` par
/// chaque [`Memory`], sans cloner le modèle sous-jacent.
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
    async fn open(&self, agent: AgentId) -> Result<Memory, basemyai::MemoryError> {
        Memory::open_in_memory(agent.as_str()).await
    }
}
