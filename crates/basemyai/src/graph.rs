//! Graphe entités/relations (VISION §4.1, Phase 2 — Cognition).
//!
//! « Alice travaille chez Acme, qui a racheté Beta » est trois faits liés. Le
//! vecteur seul les noie en chunks indépendants ; le graphe permet de traverser
//! *Alice → employeur → acquisitions*. On le modélise en **tables `entity` /
//! `edge` + CTE récursives** dans le même fichier libSQL — pas de Kuzu/Neo4j
//! (ADR-011, alternatives rejetées).
//!
//! Tout est scellé par un [`AgentId`] (isolation ADR-006) et filtré dans le
//! temps (`valid_until`, ADR-005). C'est du *sens* : ce module vit dans
//! `basemyai`, jamais dans le core agnostique (ADR-001).

use basemyai_core::libsql::{self, Connection};
use basemyai_core::{CoreError, Store};

use crate::temporal::Validity;
use crate::{AgentId, Result, now_unix};

/// Une entité atteinte par une traversée du graphe, avec sa profondeur (nombre
/// de sauts depuis le point de départ).
#[derive(Debug, Clone, PartialEq)]
pub struct Reached {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub depth: u32,
}

/// Façade graphe d'un agent : nœuds (`entity`) et arêtes (`edge`) partagent le
/// fichier libSQL de la mémoire, scellés par `agent`. Toute lecture/écriture est
/// bornée à `agent_id` au niveau SQL.
pub struct Graph {
    conn: Connection,
    agent: AgentId,
}

impl Graph {
    /// Construit une façade graphe sur le store d'une mémoire déjà migrée
    /// (le schéma graphe est posé par [`schema`](crate::schema) en version 2).
    #[must_use]
    pub fn new(store: &Store, agent: AgentId) -> Self {
        Self { conn: store.connect(), agent }
    }

    /// L'agent propriétaire de ce graphe.
    #[must_use]
    pub fn agent(&self) -> &AgentId {
        &self.agent
    }

    /// Insère ou met à jour une entité (nœud), valide dès maintenant.
    ///
    /// # Errors
    /// [`MemoryError::Core`](crate::MemoryError::Core) en cas d'échec SQL.
    pub async fn add_entity(&self, id: &str, kind: &str, label: &str) -> Result<()> {
        self.add_entity_with(id, kind, label, Validity::since(now_unix())).await
    }

    /// Insère ou met à jour une entité avec une fenêtre de validité explicite.
    ///
    /// # Errors
    /// [`MemoryError::Core`](crate::MemoryError::Core) en cas d'échec SQL.
    pub async fn add_entity_with(&self, id: &str, kind: &str, label: &str, validity: Validity) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO entity (id, agent_id, kind, label, valid_from, valid_until) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                 ON CONFLICT(id) DO UPDATE SET \
                   kind = excluded.kind, label = excluded.label, \
                   valid_from = excluded.valid_from, valid_until = excluded.valid_until \
                 WHERE entity.agent_id = excluded.agent_id",
                libsql::params![
                    id,
                    self.agent.as_str(),
                    kind,
                    label,
                    validity.valid_from,
                    validity.valid_until,
                ],
            )
            .await
            .map_err(storage)?;
        Ok(())
    }

    /// Crée (ou met à jour le poids d') une relation orientée `src → dst`,
    /// valide dès maintenant.
    ///
    /// # Errors
    /// [`MemoryError::Core`](crate::MemoryError::Core) en cas d'échec SQL.
    pub async fn add_edge(&self, src: &str, relation: &str, dst: &str, weight: f64) -> Result<()> {
        let now = now_unix();
        self.conn
            .execute(
                "INSERT INTO edge (src, dst, agent_id, relation, weight, valid_from, valid_until) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL) \
                 ON CONFLICT(src, dst, relation) DO UPDATE SET weight = excluded.weight \
                 WHERE edge.agent_id = excluded.agent_id",
                libsql::params![src, dst, self.agent.as_str(), relation, weight, now],
            )
            .await
            .map_err(storage)?;
        Ok(())
    }

    /// Traversée multi-sauts depuis `start` en suivant les arêtes orientées, par
    /// **CTE récursive** (`WITH RECURSIVE`). Ne retourne que les entités
    /// *encore valides* à l'instant courant, bornées à cet agent, jusqu'à
    /// `max_depth` sauts. Le point de départ lui-même est exclu du résultat.
    ///
    /// `UNION` (et non `UNION ALL`) déduplique les `(nœud, profondeur)` ; combiné
    /// à la borne `max_depth`, la traversée termine même sur un graphe cyclique.
    ///
    /// # Errors
    /// [`MemoryError::Core`](crate::MemoryError::Core) en cas d'échec SQL.
    pub async fn traverse(&self, start: &str, max_depth: u32) -> Result<Vec<Reached>> {
        let now = now_unix();
        let sql = "\
            WITH RECURSIVE reach(node, depth) AS ( \
                SELECT ?1, 0 \
                UNION \
                SELECT e.dst, r.depth + 1 \
                FROM edge e JOIN reach r ON e.src = r.node \
                WHERE e.agent_id = ?2 \
                  AND (e.valid_until IS NULL OR e.valid_until > ?3) \
                  AND r.depth < ?4 \
            ) \
            SELECT e.id, e.kind, e.label, MIN(r.depth) AS d \
            FROM reach r \
            JOIN entity e ON e.id = r.node \
            WHERE r.node <> ?1 \
              AND e.agent_id = ?2 \
              AND (e.valid_until IS NULL OR e.valid_until > ?3) \
            GROUP BY e.id, e.kind, e.label \
            ORDER BY d, e.id";

        let mut rows = self
            .conn
            .query(sql, libsql::params![start, self.agent.as_str(), now, i64::from(max_depth)])
            .await
            .map_err(storage)?;

        let mut out = Vec::new();
        while let Some(row) = rows.next().await.map_err(storage)? {
            let depth: i64 = row.get(3).map_err(storage)?;
            out.push(Reached {
                id: row.get::<String>(0).map_err(storage)?,
                kind: row.get::<String>(1).map_err(storage)?,
                label: row.get::<String>(2).map_err(storage)?,
                depth: u32::try_from(depth).unwrap_or(u32::MAX),
            });
        }
        Ok(out)
    }
}

fn storage(e: libsql::Error) -> crate::MemoryError {
    CoreError::Storage(e.to_string()).into()
}
