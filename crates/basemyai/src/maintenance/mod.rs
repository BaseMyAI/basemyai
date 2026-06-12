//! Tâches de maintenance **sémantiques**, injectées dans le worker agnostique
//! du core. Le GC par `valid_until` vit ici (ADR-005/ADR-008) : le core fait
//! tourner la boucle, mais ignore le sens de l'expiration.

mod forgetting;
mod gc;

pub use forgetting::AdaptiveForgetting;
pub use gc::ExpiredMemoryGc;

use std::sync::Arc;

use basemyai_core::{MaintenanceTask, Result, Store};

use crate::{LlmInference, Memory, consolidate};

/// Tâche de fond de consolidation (épisodes → faits + graphe). Stocke ses propres
/// [`Memory`] et [`LlmInference`] ; le `_store` fourni par le worker est ignoré
/// (la mémoire possède son propre store).
pub struct ConsolidationTask {
    memory: Arc<Memory>,
    llm: Arc<dyn LlmInference>,
}

impl ConsolidationTask {
    /// Construit la tâche à partir de références comptées vers la mémoire et le
    /// fournisseur d'inférence.
    pub fn new(memory: Arc<Memory>, llm: Arc<dyn LlmInference>) -> Self {
        Self { memory, llm }
    }
}

#[async_trait::async_trait]
impl MaintenanceTask for ConsolidationTask {
    fn name(&self) -> &str {
        "consolidation"
    }

    /// Lance une passe de consolidation. Ignore `_store` (la mémoire est
    /// auto-suffisante). Mappe [`MemoryError`](crate::MemoryError) vers
    /// [`CoreError::Storage`](basemyai_core::CoreError::Storage) pour
    /// satisfaire l'interface du core.
    async fn run(&self, _store: &Store) -> Result<()> {
        consolidate(&self.memory, self.llm.as_ref())
            .await
            .map(|_| ())
            .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))
    }
}
