// SPDX-License-Identifier: BUSL-1.1
//! Outil `stats` : compte des souvenirs valides d'un agent, par couche.

use rmcp::schemars;
use serde::{Deserialize, Serialize};

/// Paramètres de `stats`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct StatsParams {
    /// Identifiant de l'agent (tenant).
    pub agent_id: String,
}

/// Résultat de `stats`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct StatsResult {
    /// Souvenirs valides en couche `short_term`.
    #[schemars(schema_with = "crate::tools::count_schema")]
    pub short_term: usize,
    /// Souvenirs valides en couche `episodic`.
    #[schemars(schema_with = "crate::tools::count_schema")]
    pub episodic: usize,
    /// Souvenirs valides en couche `procedural`.
    #[schemars(schema_with = "crate::tools::count_schema")]
    pub procedural: usize,
    /// Souvenirs valides en couche `semantic`.
    #[schemars(schema_with = "crate::tools::count_schema")]
    pub semantic: usize,
    /// Total des souvenirs valides.
    #[schemars(schema_with = "crate::tools::count_schema")]
    pub total: usize,
}
