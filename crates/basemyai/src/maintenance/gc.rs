// SPDX-License-Identifier: BUSL-1.1
//! GC des mémoires expirées (ADR-005/ADR-008). La sémantique `valid_until`
//! est portée par `basemyai`, jamais par le core.

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
        let now = now_unix();
        let txn = store.begin_write().await?;
        txn.execute(
            "DELETE FROM memory_fts \
             WHERE id IN (\
               SELECT id FROM memory WHERE valid_until IS NOT NULL AND valid_until <= ?1\
             )",
            basemyai_core::libsql::params![now],
        )
        .await
        .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
        txn.execute(
            "DELETE FROM memory WHERE valid_until IS NOT NULL AND valid_until <= ?1",
            basemyai_core::libsql::params![now],
        )
        .await
        .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
        txn.commit().await?;
        Ok(())
    }
}
