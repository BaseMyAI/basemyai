//! Outil `recall_graph` : traversée du graphe entités/relations d'un agent.

use rmcp::schemars;
use serde::{Deserialize, Serialize};

/// Paramètres de `recall_graph`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RecallGraphParams {
    /// Identifiant de l'agent (tenant).
    pub agent_id: String,
    /// Identifiant de l'entité de départ de la traversée.
    pub start: String,
    /// Profondeur maximale (nombre de sauts) de la traversée.
    #[serde(default = "default_depth")]
    #[schemars(schema_with = "crate::tools::count_schema")]
    pub max_depth: u32,
}

fn default_depth() -> u32 {
    2
}

/// Une entité atteinte par la traversée.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct EntityItem {
    /// Identifiant de l'entité.
    pub id: String,
    /// Type/catégorie de l'entité.
    pub kind: String,
    /// Libellé humain de l'entité.
    pub label: String,
    /// Profondeur (nombre de sauts depuis le départ).
    #[schemars(schema_with = "crate::tools::count_schema")]
    pub depth: u32,
}

/// Résultat de `recall_graph`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct RecallGraphResult {
    /// Entités atteintes, triées par profondeur croissante.
    pub entities: Vec<EntityItem>,
    /// `true` si des éléments ont été retirés pour tenir sous le plafond de taille.
    pub truncated: bool,
}
