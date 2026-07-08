// SPDX-License-Identifier: BUSL-1.1
//! Boucle de maintenance async. Le core **fait tourner la boucle** ; les tâches
//! sont **injectées par le consommateur** (mécanisme au core, sens au
//! consommateur) et sont **auto-suffisantes** — chacune possède déjà ce dont
//! elle a besoin (`Arc<Memory>`, backend natif, etc.), le worker ne leur passe
//! aucun store partagé (ADR-032 : avant la suppression de libSQL, `run`
//! recevait un `&Store` — `ConsolidationTask`, la seule tâche restante, l'a
//! toujours ignoré).

use std::sync::Arc;
use std::time::Duration;

use crate::Result;

/// Une tâche de maintenance fournie par le consommateur.
#[async_trait::async_trait]
pub trait MaintenanceTask: Send + Sync {
    /// Nom lisible (tracing/debug).
    fn name(&self) -> &str;

    /// Exécute la tâche.
    ///
    /// # Errors
    /// Propage toute erreur ; la boucle logue et continue.
    async fn run(&self) -> Result<()>;
}

/// Planifie et exécute des [`MaintenanceTask`] en tâche de fond, sans bloquer
/// le chemin critique.
#[derive(Default)]
pub struct MaintenanceWorker {
    tasks: Vec<(Duration, Arc<dyn MaintenanceTask>)>,
}

impl MaintenanceWorker {
    /// Crée un worker sans tâche. Utiliser [`register`](Self::register) pour en ajouter.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Enregistre une tâche à exécuter tous les `every`.
    #[must_use]
    pub fn register(mut self, every: Duration, task: Arc<dyn MaintenanceTask>) -> Self {
        self.tasks.push((every, task));
        self
    }

    /// Démarre la boucle de fond. Consomme le worker.
    ///
    /// Une boucle `tokio::spawn` par tâche : `sleep(every)` puis `run`. Une
    /// erreur est loguée (`warn`) et n'interrompt pas la boucle.
    pub fn start(self) {
        for (every, task) in self.tasks {
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(every).await;
                    if let Err(e) = task.run().await {
                        tracing::warn!(task = task.name(), error = %e, "maintenance task failed");
                    }
                }
            });
        }
    }
}
