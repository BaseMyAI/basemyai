// SPDX-License-Identifier: BUSL-1.1
//! `POST /compile_context` : surface REST du Context Engine (`basemyai::context`).

use axum::Json;
use axum::extract::State;
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};

use crate::context::{AppState, RequestContext};
use crate::http::error::RestError;
use crate::http::extract::{JsonBody, validate_query};

#[derive(Deserialize)]
pub(super) struct CompileContextRequest {
    agent_id: String,
    query: String,
    token_budget: usize,
    #[serde(default = "default_candidate_limit")]
    candidate_limit: usize,
    #[serde(default)]
    include_procedural: bool,
    /// `allow_all` | `exclude_imported` (défaut) | `user_and_consolidation_only`.
    #[serde(default = "default_source_policy")]
    source_policy: String,
    /// `balanced` (défaut) | `conversation` | `coding` | `execution` | `safety_critical`.
    #[serde(default = "default_profile")]
    profile: String,
    /// `text` | `markdown` (défaut) | `json`.
    #[serde(default = "default_render_format")]
    render_format: String,
    #[serde(default)]
    explain: bool,
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

#[derive(Serialize)]
struct RetrievalContributionDto {
    memory_id: String,
    retrieval_rank: usize,
    retrieval_score: f32,
}

impl From<basemyai::RetrievalContribution> for RetrievalContributionDto {
    fn from(c: basemyai::RetrievalContribution) -> Self {
        Self {
            memory_id: c.memory_id,
            retrieval_rank: c.retrieval_rank,
            retrieval_score: c.retrieval_score,
        }
    }
}

#[derive(Serialize)]
struct ContextItemDto {
    text: String,
    source_memory_ids: Vec<String>,
    layer: String,
    trust: String,
    role: String,
    valid_from: i64,
    valid_until: Option<i64>,
    temporal_status: String,
    retrieval_score: f32,
    retrieval_rank: usize,
    retrieval_contributions: Vec<RetrievalContributionDto>,
    estimated_tokens: usize,
    utility_score: f64,
    value_per_token: f64,
    freshness_score: f64,
    inclusion_reason: String,
}

impl From<basemyai::ContextItem> for ContextItemDto {
    fn from(item: basemyai::ContextItem) -> Self {
        Self {
            text: item.text,
            source_memory_ids: item.source_memory_ids,
            layer: item.layer.table().to_string(),
            trust: item.trust.as_str().to_string(),
            role: item.role.as_str().to_string(),
            valid_from: item.validity.valid_from,
            valid_until: item.validity.valid_until,
            temporal_status: context_temporal_status(item.temporal_status).to_string(),
            retrieval_score: item.retrieval_score,
            retrieval_rank: item.retrieval_rank,
            retrieval_contributions: item
                .retrieval_contributions
                .into_iter()
                .map(RetrievalContributionDto::from)
                .collect(),
            estimated_tokens: item.estimated_tokens,
            utility_score: item.utility_score,
            value_per_token: item.value_per_token,
            freshness_score: item.freshness_score,
            inclusion_reason: context_inclusion_reason(item.inclusion_reason).to_string(),
        }
    }
}

#[derive(Serialize)]
struct ContextSectionDto {
    kind: String,
    items: Vec<ContextItemDto>,
}

impl From<basemyai::ContextSection> for ContextSectionDto {
    fn from(section: basemyai::ContextSection) -> Self {
        Self {
            kind: context_section_kind(section.kind).to_string(),
            items: section.items.into_iter().map(ContextItemDto::from).collect(),
        }
    }
}

#[derive(Serialize)]
struct ContextCitationDto {
    memory_id: String,
    section: String,
}

impl From<basemyai::ContextCitation> for ContextCitationDto {
    fn from(c: basemyai::ContextCitation) -> Self {
        Self {
            memory_id: c.memory_id,
            section: context_section_kind(c.section).to_string(),
        }
    }
}

#[derive(Serialize)]
struct ExcludedMemoryDto {
    memory_id: String,
    reason: String,
    temporal_status: String,
    role: String,
    retrieval_contribution: RetrievalContributionDto,
}

impl From<basemyai::ExcludedMemory> for ExcludedMemoryDto {
    fn from(e: basemyai::ExcludedMemory) -> Self {
        Self {
            memory_id: e.memory_id,
            reason: context_exclusion_reason(e.reason).to_string(),
            temporal_status: context_temporal_status(e.temporal_status).to_string(),
            role: e.role.as_str().to_string(),
            retrieval_contribution: RetrievalContributionDto::from(e.retrieval_contribution),
        }
    }
}

#[derive(Serialize)]
struct MergedMemoryDto {
    memory_id: String,
    representative_memory_id: String,
}

impl From<basemyai::MergedMemory> for MergedMemoryDto {
    fn from(m: basemyai::MergedMemory) -> Self {
        Self {
            memory_id: m.memory_id,
            representative_memory_id: m.representative_memory_id,
        }
    }
}

#[derive(Serialize)]
struct DedupClusterDto {
    representative_memory_id: String,
    memory_ids: Vec<String>,
}

impl From<basemyai::DedupCluster> for DedupClusterDto {
    fn from(c: basemyai::DedupCluster) -> Self {
        Self {
            representative_memory_id: c.representative_memory_id,
            memory_ids: c.memory_ids,
        }
    }
}

#[derive(Serialize)]
struct ContextWarningDto {
    kind: String,
    memory_ids: Vec<String>,
}

impl From<basemyai::ContextWarning> for ContextWarningDto {
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

#[derive(Serialize)]
struct ContextTraceSummaryDto {
    included_items: usize,
    included_memories: usize,
    excluded_memories: usize,
    dedup_clusters: usize,
    warnings: usize,
}

impl From<basemyai::ContextTraceSummary> for ContextTraceSummaryDto {
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

#[derive(Serialize)]
struct ContextTraceDto {
    level: String,
    summary: ContextTraceSummaryDto,
    events: Vec<serde_json::Value>,
    total_events: usize,
    truncated: bool,
}

impl From<basemyai::ContextTrace> for ContextTraceDto {
    fn from(trace: basemyai::ContextTrace) -> Self {
        Self {
            level: match trace.level {
                basemyai::ContextTraceLevel::Compact => "compact".to_string(),
                basemyai::ContextTraceLevel::Detailed => "detailed".to_string(),
                _ => "unknown".to_string(),
            },
            summary: ContextTraceSummaryDto::from(trace.summary),
            events: trace.events.into_iter().map(context_trace_event_json).collect(),
            total_events: trace.total_events,
            truncated: trace.truncated,
        }
    }
}

fn context_trace_event_json(event: basemyai::ContextTraceEvent) -> serde_json::Value {
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
            "inclusion_reason": context_inclusion_reason(reason),
            "contributions": contributions.into_iter().map(|c| serde_json::json!({
                "memory_id": c.memory_id,
                "retrieval_rank": c.retrieval_rank,
                "retrieval_score": c.retrieval_score,
            })).collect::<Vec<_>>(),
        }),
        basemyai::ContextTraceEvent::Excluded(excluded) => serde_json::json!({
            "kind": "excluded",
            "memory_id": excluded.memory_id,
            "reason": context_exclusion_reason(excluded.reason),
            "temporal_status": context_temporal_status(excluded.temporal_status),
            "role": excluded.role.as_str(),
        }),
        basemyai::ContextTraceEvent::Deduplicated(cluster) => serde_json::json!({
            "kind": "deduplicated",
            "representative_memory_id": cluster.representative_memory_id,
            "memory_ids": cluster.memory_ids,
        }),
        basemyai::ContextTraceEvent::Warning(warning) => {
            let mut value = serde_json::to_value(ContextWarningDto::from(warning)).unwrap_or_default();
            if let serde_json::Value::Object(map) = &mut value {
                map.insert("kind_group".to_string(), serde_json::json!("warning"));
            }
            value
        }
        _ => serde_json::json!({ "kind": "unknown" }),
    }
}

#[derive(Serialize)]
struct CompileContextResponse {
    rendered: String,
    estimated_tokens: usize,
    profile: String,
    render_format: String,
    sections: Vec<ContextSectionDto>,
    citations: Vec<ContextCitationDto>,
    merged: Vec<MergedMemoryDto>,
    excluded: Vec<ExcludedMemoryDto>,
    dedup_clusters: Vec<DedupClusterDto>,
    warnings: Vec<ContextWarningDto>,
    trace: ContextTraceDto,
}

impl From<basemyai::ContextBundle> for CompileContextResponse {
    fn from(bundle: basemyai::ContextBundle) -> Self {
        Self {
            rendered: bundle.rendered,
            estimated_tokens: bundle.estimated_tokens,
            profile: bundle.profile.as_str().to_string(),
            render_format: bundle.render_format.as_str().to_string(),
            sections: bundle.sections.into_iter().map(ContextSectionDto::from).collect(),
            citations: bundle.citations.into_iter().map(ContextCitationDto::from).collect(),
            merged: bundle.merged.into_iter().map(MergedMemoryDto::from).collect(),
            excluded: bundle.excluded.into_iter().map(ExcludedMemoryDto::from).collect(),
            dedup_clusters: bundle.dedup_clusters.into_iter().map(DedupClusterDto::from).collect(),
            warnings: bundle.warnings.into_iter().map(ContextWarningDto::from).collect(),
            trace: ContextTraceDto::from(bundle.trace),
        }
    }
}

fn parse_context_source_policy(value: &str) -> Result<basemyai::ContextSourcePolicy, RestError> {
    match value {
        "allow_all" => Ok(basemyai::ContextSourcePolicy::AllowAll),
        "exclude_imported" => Ok(basemyai::ContextSourcePolicy::ExcludeImported),
        "user_and_consolidation_only" => Ok(basemyai::ContextSourcePolicy::UserAndConsolidationOnly),
        _ => Err(RestError::Validation(format!(
            "source_policy must be 'allow_all', 'exclude_imported', or \
             'user_and_consolidation_only', got {value:?}"
        ))),
    }
}

fn parse_context_profile(value: &str) -> Result<basemyai::ContextProfile, RestError> {
    match value {
        "balanced" => Ok(basemyai::ContextProfile::Balanced),
        "conversation" => Ok(basemyai::ContextProfile::Conversation),
        "coding" => Ok(basemyai::ContextProfile::Coding),
        "execution" => Ok(basemyai::ContextProfile::Execution),
        "safety_critical" => Ok(basemyai::ContextProfile::SafetyCritical),
        _ => Err(RestError::Validation(format!(
            "profile must be 'balanced', 'conversation', 'coding', 'execution', or \
             'safety_critical', got {value:?}"
        ))),
    }
}

fn parse_context_render_format(value: &str) -> Result<basemyai::ContextRenderFormat, RestError> {
    match value {
        "text" => Ok(basemyai::ContextRenderFormat::Text),
        "markdown" => Ok(basemyai::ContextRenderFormat::Markdown),
        "json" => Ok(basemyai::ContextRenderFormat::Json),
        _ => Err(RestError::Validation(format!(
            "render_format must be 'text', 'markdown', or 'json', got {value:?}"
        ))),
    }
}

fn context_section_kind(kind: basemyai::ContextSectionKind) -> &'static str {
    match kind {
        basemyai::ContextSectionKind::WorkingContext => "working_context",
        basemyai::ContextSectionKind::CurrentFacts => "current_facts",
        basemyai::ContextSectionKind::Procedures => "procedures",
        basemyai::ContextSectionKind::RecentEvents => "recent_events",
        _ => "unknown",
    }
}

fn context_temporal_status(status: basemyai::ContextTemporalStatus) -> &'static str {
    match status {
        basemyai::ContextTemporalStatus::Current => "current",
        basemyai::ContextTemporalStatus::Scheduled => "scheduled",
        basemyai::ContextTemporalStatus::Expired => "expired",
        _ => "unknown",
    }
}

fn context_exclusion_reason(reason: basemyai::ExclusionReason) -> &'static str {
    match reason {
        basemyai::ExclusionReason::SourceFiltered => "source_filtered",
        basemyai::ExclusionReason::NotCurrentlyValid => "not_currently_valid",
        basemyai::ExclusionReason::TokenBudget => "token_budget",
        basemyai::ExclusionReason::ProfileQuota => "profile_quota",
        _ => "unknown",
    }
}

fn context_inclusion_reason(reason: basemyai::InclusionReason) -> &'static str {
    match reason {
        basemyai::InclusionReason::SectionReservation => "section_reservation",
        basemyai::InclusionReason::ValuePerToken => "value_per_token",
        basemyai::InclusionReason::LocalReplacement => "local_replacement",
        _ => "unknown",
    }
}

pub(super) async fn compile_context(
    State(state): State<AppState>,
    JsonBody(req): JsonBody<CompileContextRequest>,
) -> Result<impl IntoResponse, RestError> {
    validate_query(&req.query)?;
    let source_policy = parse_context_source_policy(&req.source_policy)?;
    let profile = parse_context_profile(&req.profile)?;
    let render_format = parse_context_render_format(&req.render_format)?;
    let mem = RequestContext::require_agent(&state, &req.agent_id).await?;
    let mut request = basemyai::ContextRequest::new(&req.query, req.token_budget)
        .candidate_limit(req.candidate_limit)
        .source_policy(source_policy)
        .profile(profile)
        .render_format(render_format);
    if req.include_procedural {
        request = request.include_procedural();
    }
    if req.explain {
        request = request.explain();
    }
    let bundle = mem.compile_context(request).await?;
    Ok(Json(CompileContextResponse::from(bundle)))
}
