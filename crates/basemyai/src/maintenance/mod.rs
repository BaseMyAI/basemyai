// SPDX-License-Identifier: BUSL-1.1
//! Tâches de maintenance **sémantiques**, injectées dans le worker agnostique
//! du core (`basemyai_core::MaintenanceWorker`).
//!
//! GC temporel (`valid_until`) reposait sur du SQL spécifique au backend
//! libSQL, retiré du workspace par ADR-033 — supprimé avec lui (item de
//! suivi séparé, hors scope). L'oubli adaptatif (ADR-012 §4), qui reposait
//! lui aussi sur du SQL de fenêtrage (`ROW_NUMBER() OVER (PARTITION BY
//! ...)`), a été **porté** sur le moteur natif par ADR-037 :
//! [`adaptive_forgetting`], scan applicatif + sélection pure au lieu d'une
//! requête fenêtrée. `ConsolidationTask` et `AdaptiveForgettingTask`
//! partagent le même pattern : auto-suffisantes via `Arc<Memory>`, aucun
//! store partagé injecté par le worker.

pub(crate) mod adaptive_forgetting;

pub use adaptive_forgetting::{AdaptiveForgettingPolicy, AdaptiveForgettingTask, ForgettingReport};

use std::sync::Arc;

use basemyai_core::{MaintenanceTask, Result};

use crate::{LlmInference, Memory, consolidate};

/// Tâche de fond de consolidation (épisodes → faits + graphe). Auto-suffisante :
/// possède sa propre [`Memory`] et son [`LlmInference`].
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

    /// Lance une passe de consolidation. Mappe [`MemoryError`](crate::MemoryError)
    /// vers [`CoreError::Storage`](basemyai_core::CoreError::Storage) pour
    /// satisfaire l'interface du core.
    async fn run(&self) -> Result<()> {
        consolidate(&self.memory, self.llm.as_ref())
            .await
            .map(|_| ())
            .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))
    }
}
