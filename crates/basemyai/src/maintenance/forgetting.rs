//! Oubli adaptatif (VISION §5.2), au-delà du GC temporel V1. Éviction par score
//! combiné **importance × récence** (et, à terme, « surprise »), **décroissance**
//! progressive de l'importance, et plafond de capacité par agent.
//!
//! Implémenté comme [`MaintenanceTask`] injectée dans le worker agnostique du
//! core (VISION §4.3) : le core fait tourner la boucle, `basemyai` porte la
//! politique. Ne bloque jamais le chemin critique.

use basemyai_core::{MaintenanceTask, Result, Store};

use crate::now_unix;

/// Politique d'oubli adaptatif, enregistrée dans le `MaintenanceWorker`.
#[derive(Debug, Clone, Copy)]
pub struct AdaptiveForgetting {
    /// Nombre maximum de souvenirs conservés par agent (les moins bien notés
    /// au-delà sont évincés).
    pub capacity_per_agent: usize,
    /// Demi-vie de la récence en secondes (sert à pondérer le score de rétention).
    pub recency_half_life_secs: i64,
}

#[async_trait::async_trait]
impl MaintenanceTask for AdaptiveForgetting {
    fn name(&self) -> &str {
        "adaptive-forgetting"
    }

    async fn run(&self, store: &Store) -> Result<()> {
        let now = now_unix();
        let half_life = self.recency_half_life_secs;
        // Plafond de capacité par agent (lié en paramètre, jamais interpolé).
        let capacity = i64::try_from(self.capacity_per_agent).unwrap_or(i64::MAX);

        // Score de rétention par souvenir :
        //   age       = max(0, now - COALESCE(last_access, valid_from))
        //   recency   = H / (H + age)            (décroissance hyperbolique)
        //   retention = importance + recency
        //
        // On évite volontairement `0.5^(age/H)` : d'une part cette build de
        // libSQL n'embarque pas les fonctions mathématiques (`pow`/`exp`), d'autre
        // part l'exponentielle sous-déborde en f64 dès que `age` dépasse quelques
        // centaines de demi-vies (deux souvenirs anciens deviennent alors
        // indistinguables à `0.0`). La forme hyperbolique `H / (H + age)` ne
        // dépend d'aucune fonction native, reste dans `(0, 1]`, vaut `1` à
        // `age = 0` et `0.5` à `age = H` (« demi-vie » préservée), et **dégrade
        // gracieusement** : elle distingue encore deux grands âges. Strictement
        // décroissante en `age`, donc l'ordre par récence (et l'additivité avec
        // `importance`) est respecté.
        //
        // On classe par agent (`PARTITION BY agent_id`) du meilleur au moins bon
        // score, `id` départageant les ex æquo, puis on évince tout ce qui dépasse
        // `capacity` (rang > capacity). Une seule requête, fonction de fenêtrage
        // native libSQL/SQLite.
        let txn = store.begin_write().await?;
        let evicted_ids = "\
            SELECT id FROM (\
              SELECT id, ROW_NUMBER() OVER (\
                PARTITION BY agent_id \
                ORDER BY importance \
                  + CAST(?2 AS REAL) / (\
                      CAST(?2 AS REAL) \
                      + max(0, CAST(?1 AS REAL) - COALESCE(last_access, valid_from))\
                    ) DESC, id\
              ) AS rn FROM memory\
            ) WHERE rn > ?3";
        txn.execute(
            &format!("DELETE FROM memory_fts WHERE id IN ({evicted_ids})"),
            basemyai_core::libsql::params![now, half_life, capacity],
        )
        .await
        .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
        txn.execute(
            &format!("DELETE FROM memory WHERE id IN ({evicted_ids})"),
            basemyai_core::libsql::params![now, half_life, capacity],
        )
        .await
        .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
        txn.commit().await?;
        Ok(())
    }
}
