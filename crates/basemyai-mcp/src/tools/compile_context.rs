// SPDX-License-Identifier: BUSL-1.1
//! Outil `compile_context` : compile un recall hybride en contexte borné et
//! traçable (Context Engine, R1.8). Même contrat de chaînes wire que les
//! bindings Python/Node (`bindings/*/src/types.rs`) et la CLI
//! (`basemyai-cli/src/commands/compile_context.rs`), pour ne pas diverger.

use rmcp::schemars;
use serde::{Deserialize, Serialize};

/// Paramètres de `compile_context`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CompileContextParams {
    /// Identifiant de l'agent (tenant).
    pub agent_id: String,
    /// Requête en langage naturel transmise au recall hybride.
    pub query: String,
    /// Budget de tokens estimé, dur (jamais dépassé).
    #[schemars(schema_with = "crate::tools::count_schema")]
    pub token_budget: usize,
    /// Taille du pool de candidats du recall hybride sous-jacent.
    #[serde(default = "default_candidate_limit")]
    #[schemars(schema_with = "crate::tools::count_schema")]
    pub candidate_limit: usize,
    /// Inclut explicitement la couche `procedural` dans le recall (défaut : `false`).
    #[serde(default)]
    pub include_procedural: bool,
    /// `allow_all` | `exclude_imported` (défaut) | `user_and_consolidation_only`.
    #[serde(default = "default_source_policy")]
    pub source_policy: String,
    /// `balanced` (défaut) | `conversation` | `coding` | `execution` | `safety_critical`.
    #[serde(default = "default_profile")]
    pub profile: String,
    /// `text` | `markdown` (défaut) | `json`.
    #[serde(default = "default_render_format")]
    pub render_format: String,
    /// Conserve une trace détaillée et bornée (raisons d'inclusion/exclusion,
    /// contributions de retrieval, clusters de déduplication, avertissements).
    #[serde(default)]
    pub explain: bool,
}

fn default_candidate_limit() -> usize {
    64
}

fn default_source_policy() -> String {
    "exclude_imported".to_string()
}

fn default_profile() -> String {
    "balanced".to_string()
}

fn default_render_format() -> String {
    "markdown".to_string()
}

/// Contribution d'un souvenir rappelé au candidat compilé (avant filtrage).
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct RetrievalContributionOut {
    pub memory_id: String,
    #[schemars(schema_with = "crate::tools::count_schema")]
    pub retrieval_rank: usize,
    pub retrieval_score: f32,
}

impl From<basemyai::RetrievalContribution> for RetrievalContributionOut {
    fn from(c: basemyai::RetrievalContribution) -> Self {
        Self {
            memory_id: c.memory_id,
            retrieval_rank: c.retrieval_rank,
            retrieval_score: c.retrieval_score,
        }
    }
}

/// Item sélectionné par le Context Engine.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ContextItemOut {
    pub text: String,
    pub source_memory_ids: Vec<String>,
    pub layer: String,
    pub trust: String,
    /// `fact` | `constraint` | `procedure` | `event` | `reference` | `uncertain_data`.
    pub role: String,
    pub valid_from: i64,
    pub valid_until: Option<i64>,
    /// `current` | `scheduled` | `expired`.
    pub temporal_status: String,
    pub retrieval_score: f32,
    #[schemars(schema_with = "crate::tools::count_schema")]
    pub retrieval_rank: usize,
    pub retrieval_contributions: Vec<RetrievalContributionOut>,
    #[schemars(schema_with = "crate::tools::count_schema")]
    pub estimated_tokens: usize,
    pub utility_score: f64,
    pub value_per_token: f64,
    pub freshness_score: f64,
    /// `section_reservation` | `value_per_token` | `local_replacement`.
    pub inclusion_reason: String,
}

impl From<basemyai::ContextItem> for ContextItemOut {
    fn from(item: basemyai::ContextItem) -> Self {
        Self {
            text: item.text,
            source_memory_ids: item.source_memory_ids,
            layer: item.layer.table().to_string(),
            trust: item.trust.as_str().to_string(),
            role: item.role.as_str().to_string(),
            valid_from: item.validity.valid_from,
            valid_until: item.validity.valid_until,
            temporal_status: temporal_status(item.temporal_status).to_string(),
            retrieval_score: item.retrieval_score,
            retrieval_rank: item.retrieval_rank,
            retrieval_contributions: item
                .retrieval_contributions
                .into_iter()
                .map(RetrievalContributionOut::from)
                .collect(),
            estimated_tokens: item.estimated_tokens,
            utility_score: item.utility_score,
            value_per_token: item.value_per_token,
            freshness_score: item.freshness_score,
            inclusion_reason: inclusion_reason(item.inclusion_reason).to_string(),
        }
    }
}

/// Section sémantique du bundle final.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ContextSectionOut {
    /// `working_context` | `current_facts` | `procedures` | `recent_events`.
    pub kind: String,
    pub items: Vec<ContextItemOut>,
}

impl From<basemyai::ContextSection> for ContextSectionOut {
    fn from(section: basemyai::ContextSection) -> Self {
        Self {
            kind: section_kind(section.kind).to_string(),
            items: section.items.into_iter().map(ContextItemOut::from).collect(),
        }
    }
}

/// Citation entre un fragment du bundle et un souvenir persisté.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ContextCitationOut {
    pub memory_id: String,
    pub section: String,
}

impl From<basemyai::ContextCitation> for ContextCitationOut {
    fn from(c: basemyai::ContextCitation) -> Self {
        Self {
            memory_id: c.memory_id,
            section: section_kind(c.section).to_string(),
        }
    }
}

/// Candidat écarté (présent uniquement si `explain: true`).
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ExcludedMemoryOut {
    pub memory_id: String,
    /// `source_filtered` | `not_currently_valid` | `token_budget` | `profile_quota`.
    pub reason: String,
    pub temporal_status: String,
    pub role: String,
    pub retrieval_contribution: RetrievalContributionOut,
}

impl From<basemyai::ExcludedMemory> for ExcludedMemoryOut {
    fn from(e: basemyai::ExcludedMemory) -> Self {
        Self {
            memory_id: e.memory_id,
            reason: exclusion_reason(e.reason).to_string(),
            temporal_status: temporal_status(e.temporal_status).to_string(),
            role: e.role.as_str().to_string(),
            retrieval_contribution: RetrievalContributionOut::from(e.retrieval_contribution),
        }
    }
}

/// Une paire absorbée -> représentant (déduplication exacte).
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct MergedMemoryOut {
    pub memory_id: String,
    pub representative_memory_id: String,
}

impl From<basemyai::MergedMemory> for MergedMemoryOut {
    fn from(m: basemyai::MergedMemory) -> Self {
        Self {
            memory_id: m.memory_id,
            representative_memory_id: m.representative_memory_id,
        }
    }
}

/// Cluster complet produit par la déduplication exacte.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct DedupClusterOut {
    pub representative_memory_id: String,
    pub memory_ids: Vec<String>,
}

impl From<basemyai::DedupCluster> for DedupClusterOut {
    fn from(c: basemyai::DedupCluster) -> Self {
        Self {
            representative_memory_id: c.representative_memory_id,
            memory_ids: c.memory_ids,
        }
    }
}

/// Avertissement conservateur, fondé uniquement sur des métadonnées explicites.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ContextWarningOut {
    /// `incompatible_metadata`.
    pub kind: String,
    pub memory_ids: Vec<String>,
}

impl From<basemyai::ContextWarning> for ContextWarningOut {
    fn from(w: basemyai::ContextWarning) -> Self {
        match w {
            basemyai::ContextWarning::IncompatibleMetadata { memory_ids } => Self {
                kind: "incompatible_metadata".to_string(),
                memory_ids,
            },
            _ => Self {
                kind: "unknown".to_string(),
                memory_ids: Vec::new(),
            },
        }
    }
}

/// Toujours présent : compteurs de la compilation, calculés avant troncature de la trace.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ContextTraceSummaryOut {
    #[schemars(schema_with = "crate::tools::count_schema")]
    pub included_items: usize,
    #[schemars(schema_with = "crate::tools::count_schema")]
    pub included_memories: usize,
    #[schemars(schema_with = "crate::tools::count_schema")]
    pub excluded_memories: usize,
    #[schemars(schema_with = "crate::tools::count_schema")]
    pub dedup_clusters: usize,
    #[schemars(schema_with = "crate::tools::count_schema")]
    pub warnings: usize,
}

impl From<basemyai::ContextTraceSummary> for ContextTraceSummaryOut {
    fn from(s: basemyai::ContextTraceSummary) -> Self {
        Self {
            included_items: s.included_items,
            included_memories: s.included_memories,
            excluded_memories: s.excluded_memories,
            dedup_clusters: s.dedup_clusters,
            warnings: s.warnings,
        }
    }
}

/// Résumé toujours présent ; `events` seulement lorsque `explain: true` a été demandé.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ContextTraceOut {
    /// `compact` | `detailed`.
    pub level: String,
    pub summary: ContextTraceSummaryOut,
    /// Vide en mode `compact`. Borné en mode `detailed` (voir `truncated`).
    pub events: Vec<serde_json::Value>,
    #[schemars(schema_with = "crate::tools::count_schema")]
    pub total_events: usize,
    pub truncated: bool,
}

impl From<basemyai::ContextTrace> for ContextTraceOut {
    fn from(trace: basemyai::ContextTrace) -> Self {
        Self {
            level: match trace.level {
                basemyai::ContextTraceLevel::Compact => "compact".to_string(),
                basemyai::ContextTraceLevel::Detailed => "detailed".to_string(),
                _ => "unknown".to_string(),
            },
            summary: ContextTraceSummaryOut::from(trace.summary),
            events: trace.events.into_iter().map(trace_event_json).collect(),
            total_events: trace.total_events,
            truncated: trace.truncated,
        }
    }
}

fn trace_event_json(event: basemyai::ContextTraceEvent) -> serde_json::Value {
    match event {
        basemyai::ContextTraceEvent::Included {
            memory_id,
            role,
            reason,
            contributions,
        } => serde_json::json!({
            "kind": "included",
            "memory_id": memory_id,
            "role": role.as_str(),
            "inclusion_reason": inclusion_reason(reason),
            "contributions": contributions.into_iter().map(|c| serde_json::json!({
                "memory_id": c.memory_id,
                "retrieval_rank": c.retrieval_rank,
                "retrieval_score": c.retrieval_score,
            })).collect::<Vec<_>>(),
        }),
        basemyai::ContextTraceEvent::Excluded(excluded) => serde_json::json!({
            "kind": "excluded",
            "memory_id": excluded.memory_id,
            "reason": exclusion_reason(excluded.reason),
            "temporal_status": temporal_status(excluded.temporal_status),
            "role": excluded.role.as_str(),
        }),
        basemyai::ContextTraceEvent::Deduplicated(cluster) => serde_json::json!({
            "kind": "deduplicated",
            "representative_memory_id": cluster.representative_memory_id,
            "memory_ids": cluster.memory_ids,
        }),
        basemyai::ContextTraceEvent::Warning(warning) => {
            let mut value = serde_json::to_value(ContextWarningOut::from(warning)).unwrap_or_default();
            if let serde_json::Value::Object(map) = &mut value {
                map.insert("kind_group".to_string(), serde_json::json!("warning"));
            }
            value
        }
        _ => serde_json::json!({ "kind": "unknown" }),
    }
}

/// Résultat de `compile_context`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct CompileContextResult {
    /// Contenu compilé dans le format demandé (`render_format`).
    pub rendered: String,
    #[schemars(schema_with = "crate::tools::count_schema")]
    pub estimated_tokens: usize,
    pub profile: String,
    pub render_format: String,
    pub sections: Vec<ContextSectionOut>,
    pub citations: Vec<ContextCitationOut>,
    pub merged: Vec<MergedMemoryOut>,
    pub excluded: Vec<ExcludedMemoryOut>,
    pub dedup_clusters: Vec<DedupClusterOut>,
    pub warnings: Vec<ContextWarningOut>,
    pub trace: ContextTraceOut,
}

impl From<basemyai::ContextBundle> for CompileContextResult {
    fn from(bundle: basemyai::ContextBundle) -> Self {
        Self {
            rendered: bundle.rendered,
            estimated_tokens: bundle.estimated_tokens,
            profile: bundle.profile.as_str().to_string(),
            render_format: bundle.render_format.as_str().to_string(),
            sections: bundle.sections.into_iter().map(ContextSectionOut::from).collect(),
            citations: bundle.citations.into_iter().map(ContextCitationOut::from).collect(),
            merged: bundle.merged.into_iter().map(MergedMemoryOut::from).collect(),
            excluded: bundle.excluded.into_iter().map(ExcludedMemoryOut::from).collect(),
            dedup_clusters: bundle.dedup_clusters.into_iter().map(DedupClusterOut::from).collect(),
            warnings: bundle.warnings.into_iter().map(ContextWarningOut::from).collect(),
            trace: ContextTraceOut::from(bundle.trace),
        }
    }
}

pub(crate) fn parse_source_policy(value: &str) -> Result<basemyai::ContextSourcePolicy, String> {
    match value {
        "allow_all" => Ok(basemyai::ContextSourcePolicy::AllowAll),
        "exclude_imported" => Ok(basemyai::ContextSourcePolicy::ExcludeImported),
        "user_and_consolidation_only" => Ok(basemyai::ContextSourcePolicy::UserAndConsolidationOnly),
        _ => Err(format!(
            "source_policy must be 'allow_all', 'exclude_imported', or \
             'user_and_consolidation_only', got {value:?}"
        )),
    }
}

pub(crate) fn parse_profile(value: &str) -> Result<basemyai::ContextProfile, String> {
    match value {
        "balanced" => Ok(basemyai::ContextProfile::Balanced),
        "conversation" => Ok(basemyai::ContextProfile::Conversation),
        "coding" => Ok(basemyai::ContextProfile::Coding),
        "execution" => Ok(basemyai::ContextProfile::Execution),
        "safety_critical" => Ok(basemyai::ContextProfile::SafetyCritical),
        _ => Err(format!(
            "profile must be 'balanced', 'conversation', 'coding', 'execution', or \
             'safety_critical', got {value:?}"
        )),
    }
}

pub(crate) fn parse_render_format(value: &str) -> Result<basemyai::ContextRenderFormat, String> {
    match value {
        "text" => Ok(basemyai::ContextRenderFormat::Text),
        "markdown" => Ok(basemyai::ContextRenderFormat::Markdown),
        "json" => Ok(basemyai::ContextRenderFormat::Json),
        _ => Err(format!(
            "render_format must be 'text', 'markdown', or 'json', got {value:?}"
        )),
    }
}

fn section_kind(kind: basemyai::ContextSectionKind) -> &'static str {
    match kind {
        basemyai::ContextSectionKind::WorkingContext => "working_context",
        basemyai::ContextSectionKind::CurrentFacts => "current_facts",
        basemyai::ContextSectionKind::Procedures => "procedures",
        basemyai::ContextSectionKind::RecentEvents => "recent_events",
        _ => "unknown",
    }
}

fn temporal_status(status: basemyai::ContextTemporalStatus) -> &'static str {
    match status {
        basemyai::ContextTemporalStatus::Current => "current",
        basemyai::ContextTemporalStatus::Scheduled => "scheduled",
        basemyai::ContextTemporalStatus::Expired => "expired",
        _ => "unknown",
    }
}

fn exclusion_reason(reason: basemyai::ExclusionReason) -> &'static str {
    match reason {
        basemyai::ExclusionReason::SourceFiltered => "source_filtered",
        basemyai::ExclusionReason::NotCurrentlyValid => "not_currently_valid",
        basemyai::ExclusionReason::TokenBudget => "token_budget",
        basemyai::ExclusionReason::ProfileQuota => "profile_quota",
        _ => "unknown",
    }
}

fn inclusion_reason(reason: basemyai::InclusionReason) -> &'static str {
    match reason {
        basemyai::InclusionReason::SectionReservation => "section_reservation",
        basemyai::InclusionReason::ValuePerToken => "value_per_token",
        basemyai::InclusionReason::LocalReplacement => "local_replacement",
        _ => "unknown",
    }
}
