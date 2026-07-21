// SPDX-License-Identifier: BUSL-1.1
//! Ouverture de mémoires par agent. Le sidecar en ouvre une par `agent_id`
//! via un [`MemoryProvider`] injecté — le même rôle que côté serveur MCP.

mod error;
#[cfg(feature = "embed")]
mod factory;
mod production;

use async_trait::async_trait;
use basemyai::{AgentId, Memory};

pub use error::ProviderError;
#[cfg(feature = "embed")]
pub use factory::build;
pub use production::FileProvider;
#[cfg(feature = "test-util")]
pub use production::InMemoryProvider;

/// Ouvre la mémoire d'un agent. `Send + Sync` : partagé par le registre.
#[async_trait]
pub trait MemoryProvider: Send + Sync {
    /// Ouvre (et migre si besoin) la mémoire de `agent`.
    ///
    /// # Errors
    /// [`basemyai::MemoryError`] si l'ouverture/migration échoue.
    async fn open(&self, agent: AgentId) -> Result<Memory, basemyai::MemoryError>;
}
