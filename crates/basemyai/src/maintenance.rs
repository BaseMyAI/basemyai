//! Tâches de maintenance **sémantiques**, injectées dans le worker agnostique
//! du core. Le GC par `valid_until` vit ici (ADR-005/ADR-008) : le core fait
//! tourner la boucle, mais ignore le sens de l'expiration.

use basemyai_core::{MaintenanceTask, Result, Store};

use crate::now_unix;

/// GC des mémoires expirées : supprime toute ligne dont `valid_until` est
/// passé. La sémantique `valid_until` est portée par `basemyai`, jamais par
/// le core.
#[derive(Debug, Default, Clone, Copy)]
pub struct ExpiredMemoryGc;

#[async_trait::async_trait]
impl MaintenanceTask for ExpiredMemoryGc {
    fn name(&self) -> &str {
        "expired-memory-gc"
    }

    async fn run(&self, store: &Store) -> Result<()> {
        let conn = store.connect();
        conn.execute(
            "DELETE FROM memory WHERE valid_until IS NOT NULL AND valid_until <= ?1",
            basemyai_core::libsql::params![now_unix()],
        )
        .await
        .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
        Ok(())
    }
}
