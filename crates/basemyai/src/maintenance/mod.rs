// SPDX-License-Identifier: BUSL-1.1
//! Tâches de maintenance **sémantiques**, injectées dans le worker agnostique
//! du core (`basemyai_core::MaintenanceWorker`).
//!
//! L'oubli adaptatif (ADR-012 §4) et le GC temporel (`valid_until`)
//! reposaient tous deux sur du SQL spécifique au backend libSQL (fenêtrage
//! `ROW_NUMBER() OVER (PARTITION BY ...)` pour le premier, `DELETE ...
//! WHERE valid_until <= ?` pour le second), retirés du workspace par
//! ADR-033. Les deux ont été **portés** sur le moteur natif : l'oubli
//! adaptatif par ADR-037 puis borné en mémoire par ADR-041 §7.3
//! ([`adaptive_forgetting`], deux passes paginées + tas borné à la
//! capacité), le GC temporel par ADR-038 puis indexé par ADR-041 §7.2
//! ([`expired_gc`], scan paginé par curseur sur l'index temporel). Les deux
//! mécanismes opèrent sur des ensembles disjoints par construction (actifs
//! vs. expirés) — voir la doc de [`expired_gc`] pour le détail du
//! non-chevauchement.
//!
//! `ConsolidationTask`, `AdaptiveForgettingTask` et `ExpiredMemoryGcTask`
//! partagent le même pattern : auto-suffisantes via `Arc<Memory>`, aucun
//! store partagé injecté par le worker.

pub(crate) mod adaptive_forgetting;
pub(crate) mod expired_gc;

pub use adaptive_forgetting::{
    AdaptiveForgettingPolicy, AdaptiveForgettingTask, ForgettingReport, run as run_adaptive_forget,
};
pub use expired_gc::{DEFAULT_GC_PAGE_SIZE, ExpiredGcReport, ExpiredMemoryGcTask, run as run_expired_gc};

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
