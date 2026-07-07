// SPDX-License-Identifier: BUSL-1.1
//! Boucle de maintenance async. Le core **fait tourner la boucle** ; les tâches
//! sont **injectées par le consommateur** (mécanisme au core, sens au
//! consommateur). Le GC par `valid_until` est une tâche `basemyai`, pas du core.

use std::sync::Arc;
use std::time::Duration;

use crate::{Result, Store};

/// Une tâche de maintenance fournie par le consommateur.
#[async_trait::async_trait]
pub trait MaintenanceTask: Send + Sync {
    /// Nom lisible (tracing/debug).
    fn name(&self) -> &str;

    /// Exécute la tâche contre le store.
    ///
    /// # Errors
    /// Propage toute erreur du store ; la boucle logue et continue.
    async fn run(&self, store: &Store) -> Result<()>;
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
    pub fn start(self, store: Arc<Store>) {
        for (every, task) in self.tasks {
            let store = Arc::clone(&store);
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(every).await;
                    if let Err(e) = task.run(&store).await {
                        tracing::warn!(task = task.name(), error = %e, "maintenance task failed");
                    }
                }
            });
        }
    }
}
