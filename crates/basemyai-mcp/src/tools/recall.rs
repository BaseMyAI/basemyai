// SPDX-License-Identifier: BUSL-1.1
//! Outil `recall` : recall temporel sémantique borné à un agent.

use rmcp::schemars;
use serde::{Deserialize, Serialize};

/// Paramètres de `recall`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RecallParams {
    /// Identifiant de l'agent (tenant).
    pub agent_id: String,
    /// Requête en langage naturel.
    pub query: String,
    /// Nombre maximum de souvenirs à retourner.
    #[serde(default = "default_k")]
    pub k: usize,
    /// Inclure la couche `procedural` (défaut : `false`, ADR-035).
    #[serde(default)]
    pub include_procedural: bool,
    /// Exclure les souvenirs importés (défaut : `false`, ADR-036).
    #[serde(default)]
    pub exclude_imported: bool,
}

fn default_k() -> usize {
    5
}

/// Un souvenir retourné par `recall`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct RecallItem {
    /// UUID du souvenir.
    pub id: String,
    /// Contenu mémorisé.
    pub text: String,
    /// Couche mémoire d'origine.
    pub layer: String,
    /// Similarité cosinus normalisée dans `[0, 1]` (`1` = identique).
    pub score: f32,
    /// Tag wire de provenance (`user`, `consolidation`, `import`, …).
    pub source: String,
    /// Provenance typée (ADR-036).
    pub trust: String,
}

/// Résultat de `recall`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct RecallResult {
    /// Souvenirs pertinents, triés du plus proche au plus lointain.
    pub items: Vec<RecallItem>,
    /// `true` si des éléments ont été retirés pour tenir sous le plafond de taille.
    pub truncated: bool,
}
