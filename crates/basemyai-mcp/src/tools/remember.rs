//! Outil `remember` : mémorise un texte dans une couche pour un agent.

use rmcp::schemars;
use serde::{Deserialize, Serialize};

/// Paramètres de `remember`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RememberParams {
    /// Identifiant de l'agent (tenant) propriétaire du souvenir.
    pub agent_id: String,
    /// Texte à mémoriser.
    pub text: String,
    /// Couche mémoire : `short_term` | `episodic` | `procedural` | `semantic`.
    #[serde(default = "default_layer")]
    pub layer: String,
}

fn default_layer() -> String {
    "semantic".to_string()
}

/// Résultat de `remember`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct RememberResult {
    /// UUID du souvenir créé.
    pub id: String,
}
