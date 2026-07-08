// SPDX-License-Identifier: BUSL-1.1
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

use std::sync::Arc;

use crate::storage::MemoryStore;
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
    engine: Arc<dyn MemoryStore>,
    agent: AgentId,
}

impl Graph {
    /// Construit une façade graphe sur le moteur d'une mémoire déjà migrée
    /// (le schéma graphe est posé par `schema` en version 2).
    #[must_use]
    pub fn new(engine: Arc<dyn MemoryStore>, agent: AgentId) -> Self {
        Self { engine, agent }
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
        self.engine
            .graph_upsert_entity(&self.agent, id, kind, label, validity)
            .await
    }

    /// Crée (ou met à jour le poids d') une relation orientée `src → dst`,
    /// valide dès maintenant.
    ///
    /// # Errors
    /// [`MemoryError::Core`](crate::MemoryError::Core) en cas d'échec SQL.
    pub async fn add_edge(&self, src: &str, relation: &str, dst: &str, weight: f64) -> Result<()> {
        let now = now_unix();
        self.engine
            .graph_upsert_edge(&self.agent, src, relation, dst, weight, now)
            .await
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
        self.engine.graph_traverse(&self.agent, start, max_depth, now).await
    }
}
