//! Schémas des outils `consolidate` et `consolidate_apply`.
//!
//! **Politique à niveaux (ADR-018, supersède ADR-017)** : `consolidate` tente,
//! dans l'ordre, le sampling MCP (si le client l'annonce), puis un LLM local
//! (Ollama/LM Studio/AnythingLLM). S'il n'a aucun LLM côté serveur, il renvoie
//! `status: "extraction_required"` avec les épisodes : **l'agent appelant** fait
//! alors l'extraction avec son propre LLM et la persiste via `consolidate_apply`.
//! C'est le vrai « plug-and-play » dans Claude Code (le sampling y est absent et
//! déprécié dans le protocole, SEP-2577).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Paramètres de `consolidate`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ConsolidateParams {
    /// Agent dont les épisodes doivent être consolidés (isolation stricte, ADR-006).
    pub agent_id: String,
}

/// Discriminant de [`ConsolidateResult`].
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConsolidateStatus {
    /// La passe a été exécutée côté serveur (sampling ou LLM local).
    Done,
    /// Aucun LLM côté serveur : l'agent appelant doit extraire puis appeler
    /// `consolidate_apply`.
    ExtractionRequired,
}

/// Résultat de `consolidate` / `consolidate_apply`.
///
/// Struct **plat** (le schéma de sortie MCP exige une racine `type: object`,
/// donc pas d'enum tagué). Les champs présents dépendent de `status` :
/// - `done` : `via` + les cinq compteurs ;
/// - `extraction_required` : `agent_id`, `episodes`, `instructions`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ConsolidateResult {
    /// `done` ou `extraction_required`.
    pub status: ConsolidateStatus,
    /// Canal utilisé (`done`) : `"sampling"`, `"local:<model>"`, `"agent"`, `"none"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    /// Épisodes valides lus et soumis à l'extraction (`done`).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::tools::count_schema")]
    pub episodes_seen: Option<usize>,
    /// Faits sémantiques nouvellement promus (`done`).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::tools::count_schema")]
    pub facts_added: Option<usize>,
    /// Faits ignorés car déjà présents (`done`).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::tools::count_schema")]
    pub facts_skipped: Option<usize>,
    /// Entités insérées/mises à jour dans le graphe (`done`).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::tools::count_schema")]
    pub entities_upserted: Option<usize>,
    /// Relations insérées/mises à jour dans le graphe (`done`).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(schema_with = "crate::tools::count_schema")]
    pub relations_upserted: Option<usize>,
    /// À repasser tel quel à `consolidate_apply` (`extraction_required`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// Épisodes bruts à consolider (`extraction_required`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episodes: Option<Vec<String>>,
    /// Instructions d'extraction + directive d'appel de `consolidate_apply`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

impl ConsolidateResult {
    /// Construit un résultat `done` à partir d'un rapport et du canal utilisé.
    #[must_use]
    pub fn done(via: &str, r: basemyai::ConsolidationReport) -> Self {
        Self {
            status: ConsolidateStatus::Done,
            via: Some(via.to_string()),
            episodes_seen: Some(r.episodes_seen),
            facts_added: Some(r.facts_added),
            facts_skipped: Some(r.facts_skipped),
            entities_upserted: Some(r.entities_upserted),
            relations_upserted: Some(r.relations_upserted),
            agent_id: None,
            episodes: None,
            instructions: None,
        }
    }

    /// Construit la réponse « à toi de jouer » pour l'agent appelant.
    #[must_use]
    pub fn extraction_required(agent_id: &str, input: basemyai::ConsolidationInput) -> Self {
        let instructions = format!(
            "No server-side LLM is configured, so YOU (the calling agent) perform the consolidation. \
             From the `episodes` below, extract: (1) `facts` — durable, atomic, deduplicated statements \
             (array of strings); (2) `entities` — array of objects {{\"id\",\"kind\",\"label\"}}; \
             (3) `relations` — array of objects {{\"src\",\"relation\",\"dst\"}} where src/dst reference \
             entity ids. Then call the `consolidate_apply` tool with agent_id=\"{agent_id}\" and those \
             three arrays. Do not invent facts not supported by the episodes."
        );
        Self {
            status: ConsolidateStatus::ExtractionRequired,
            via: None,
            episodes_seen: None,
            facts_added: None,
            facts_skipped: None,
            entities_upserted: None,
            relations_upserted: None,
            agent_id: Some(agent_id.to_string()),
            episodes: Some(input.episodes),
            instructions: Some(instructions),
        }
    }
}

/// Paramètres de `consolidate_apply` : le résultat d'extraction produit par
/// l'agent, à persister (graphe + promotion des faits, idempotent).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ConsolidateApplyParams {
    /// Agent cible (doit correspondre au `agent_id` de `consolidate`).
    pub agent_id: String,
    /// Faits durables à promouvoir en couche `semantic`.
    #[serde(default)]
    pub facts: Vec<String>,
    /// Entités du graphe.
    #[serde(default)]
    pub entities: Vec<ApplyEntity>,
    /// Relations du graphe (référencent les `id` des entités).
    #[serde(default)]
    pub relations: Vec<ApplyRelation>,
}

/// Entité fournie à `consolidate_apply`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ApplyEntity {
    /// Identifiant stable (référencé par les relations).
    pub id: String,
    /// Type/catégorie (ex. `"person"`, `"project"`).
    pub kind: String,
    /// Libellé lisible.
    pub label: String,
}

/// Relation fournie à `consolidate_apply`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ApplyRelation {
    /// `id` de l'entité source.
    pub src: String,
    /// Type de relation (ex. `"works_on"`).
    pub relation: String,
    /// `id` de l'entité destination.
    pub dst: String,
}

impl ConsolidateApplyParams {
    /// Convertit en [`basemyai::Extraction`] (le schéma du domaine).
    #[must_use]
    pub fn into_extraction(self) -> basemyai::Extraction {
        basemyai::Extraction {
            facts: self.facts,
            entities: self
                .entities
                .into_iter()
                .map(|e| basemyai::ExtractedEntity {
                    id: e.id,
                    kind: e.kind,
                    label: e.label,
                })
                .collect(),
            relations: self
                .relations
                .into_iter()
                .map(|r| basemyai::ExtractedRelation {
                    src: r.src,
                    relation: r.relation,
                    dst: r.dst,
                })
                .collect(),
        }
    }
}
