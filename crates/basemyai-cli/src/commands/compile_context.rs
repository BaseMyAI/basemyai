// SPDX-License-Identifier: BUSL-1.1
//! `basemyai context` : compile un recall hybride en contexte borné et
//! traçable via `basemyai::Memory::compile_context` (Context Engine, R1.8).
//! Miroir CLI du contrat Rust — mêmes valeurs de chaînes wire que les
//! bindings Python/Node (`bindings/*/src/types.rs`), pour ne pas diverger.

use basemyai::{
    ContextBundle, ContextItem, ContextProfile, ContextRenderFormat, ContextRequest, ContextSection,
    ContextSectionKind, ContextSourcePolicy, ContextTemporalStatus, ContextTrace, ContextTraceEvent, ContextTraceLevel,
    ContextWarning, ExclusionReason, InclusionReason, Memory,
};

use crate::error::CliError;
use crate::output::Format;

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run(
    memory: &Memory,
    query: &str,
    token_budget: usize,
    candidate_limit: usize,
    include_procedural: bool,
    source_policy: ContextSourcePolicy,
    profile: ContextProfile,
    render_format: ContextRenderFormat,
    explain: bool,
    format: Format,
) -> Result<(), CliError> {
    let spinner = if format.is_text() {
        crate::ui::progress::spinner("Compiling context...")
    } else {
        crate::ui::progress::Spinner::Disabled
    };

    let mut request = ContextRequest::new(query, token_budget)
        .candidate_limit(candidate_limit)
        .source_policy(source_policy)
        .profile(profile)
        .render_format(render_format);
    if include_procedural {
        request = request.include_procedural();
    }
    if explain {
        request = request.explain();
    }

    let bundle = memory.compile_context(request).await?;
    spinner.finish_and_clear();
    print_bundle(&bundle, format);
    Ok(())
}

fn print_bundle(bundle: &ContextBundle, format: Format) {
    format.print(
        || {
            println!("{}", bundle.rendered);
        },
        || bundle_json(bundle),
    );
}

fn bundle_json(bundle: &ContextBundle) -> serde_json::Value {
    serde_json::json!({
        "rendered": bundle.rendered,
        "estimatedTokens": bundle.estimated_tokens,
        "profile": bundle.profile.as_str(),
        "renderFormat": bundle.render_format.as_str(),
        "compiledAt": bundle.compiled_at,
        "totalUtility": bundle.total_utility,
        "sections": bundle.sections.iter().map(section_json).collect::<Vec<_>>(),
        "citations": bundle.citations.iter().map(|c| serde_json::json!({
            "memoryId": c.memory_id,
            "section": section_kind(c.section),
        })).collect::<Vec<_>>(),
        "merged": bundle.merged.iter().map(|m| serde_json::json!({
            "memoryId": m.memory_id,
            "representativeMemoryId": m.representative_memory_id,
        })).collect::<Vec<_>>(),
        "excluded": bundle.excluded.iter().map(|e| serde_json::json!({
            "memoryId": e.memory_id,
            "reason": exclusion_reason(e.reason),
            "temporalStatus": temporal_status(e.temporal_status),
            "role": e.role.as_str(),
            "retrievalContribution": {
                "memoryId": e.retrieval_contribution.memory_id,
                "retrievalRank": e.retrieval_contribution.retrieval_rank,
                "retrievalScore": e.retrieval_contribution.retrieval_score,
            },
        })).collect::<Vec<_>>(),
        "dedupClusters": bundle.dedup_clusters.iter().map(|d| serde_json::json!({
            "representativeMemoryId": d.representative_memory_id,
            "memoryIds": d.memory_ids,
        })).collect::<Vec<_>>(),
        "warnings": bundle.warnings.iter().map(warning_json).collect::<Vec<_>>(),
        "trace": trace_json(&bundle.trace),
    })
}

fn section_json(section: &ContextSection) -> serde_json::Value {
    serde_json::json!({
        "kind": section_kind(section.kind),
        "items": section.items.iter().map(item_json).collect::<Vec<_>>(),
    })
}

fn item_json(item: &ContextItem) -> serde_json::Value {
    serde_json::json!({
        "text": item.text,
        "sourceMemoryIds": item.source_memory_ids,
        "layer": item.layer.table(),
        "trust": item.trust.as_str(),
        "role": item.role.as_str(),
        "validFrom": item.validity.valid_from,
        "validUntil": item.validity.valid_until,
        "temporalStatus": temporal_status(item.temporal_status),
        "retrievalScore": item.retrieval_score,
        "retrievalRank": item.retrieval_rank,
        "retrievalContributions": item.retrieval_contributions.iter().map(|c| serde_json::json!({
            "memoryId": c.memory_id,
            "retrievalRank": c.retrieval_rank,
            "retrievalScore": c.retrieval_score,
        })).collect::<Vec<_>>(),
        "estimatedTokens": item.estimated_tokens,
        "utilityScore": item.utility_score,
        "valuePerToken": item.value_per_token,
        "freshnessScore": item.freshness_score,
        "inclusionReason": inclusion_reason(item.inclusion_reason),
    })
}

fn warning_json(warning: &ContextWarning) -> serde_json::Value {
    match warning {
        ContextWarning::IncompatibleMetadata { memory_ids } => serde_json::json!({
            "kind": "incompatible_metadata",
            "memoryIds": memory_ids,
        }),
        _ => serde_json::json!({ "kind": "unknown", "memoryIds": Vec::<String>::new() }),
    }
}

fn trace_json(trace: &ContextTrace) -> serde_json::Value {
    serde_json::json!({
        "level": match trace.level {
            ContextTraceLevel::Compact => "compact",
            ContextTraceLevel::Detailed => "detailed",
            _ => "unknown",
        },
        "summary": {
            "includedItems": trace.summary.included_items,
            "includedMemories": trace.summary.included_memories,
            "excludedMemories": trace.summary.excluded_memories,
            "dedupClusters": trace.summary.dedup_clusters,
            "warnings": trace.summary.warnings,
        },
        "events": trace.events.iter().map(trace_event_json).collect::<Vec<_>>(),
        "totalEvents": trace.total_events,
        "truncated": trace.truncated,
    })
}

fn trace_event_json(event: &ContextTraceEvent) -> serde_json::Value {
    match event {
        ContextTraceEvent::Included {
            memory_id,
            role,
            reason,
            contributions,
        } => serde_json::json!({
            "kind": "included",
            "memoryId": memory_id,
            "role": role.as_str(),
            "inclusionReason": inclusion_reason(*reason),
            "contributions": contributions.iter().map(|c| serde_json::json!({
                "memoryId": c.memory_id,
                "retrievalRank": c.retrieval_rank,
                "retrievalScore": c.retrieval_score,
            })).collect::<Vec<_>>(),
        }),
        ContextTraceEvent::Excluded(excluded) => serde_json::json!({
            "kind": "excluded",
            "memoryId": excluded.memory_id,
            "reason": exclusion_reason(excluded.reason),
            "temporalStatus": temporal_status(excluded.temporal_status),
            "role": excluded.role.as_str(),
        }),
        ContextTraceEvent::Deduplicated(cluster) => serde_json::json!({
            "kind": "deduplicated",
            "representativeMemoryId": cluster.representative_memory_id,
            "memoryIds": cluster.memory_ids,
        }),
        ContextTraceEvent::Warning(warning) => {
            let mut value = warning_json(warning);
            value["kind"] = serde_json::json!("warning");
            value
        }
        _ => serde_json::json!({ "kind": "unknown" }),
    }
}

fn section_kind(kind: ContextSectionKind) -> &'static str {
    match kind {
        ContextSectionKind::WorkingContext => "working_context",
        ContextSectionKind::CurrentFacts => "current_facts",
        ContextSectionKind::Procedures => "procedures",
        ContextSectionKind::RecentEvents => "recent_events",
        _ => "unknown",
    }
}

fn temporal_status(status: ContextTemporalStatus) -> &'static str {
    match status {
        ContextTemporalStatus::Current => "current",
        ContextTemporalStatus::Scheduled => "scheduled",
        ContextTemporalStatus::Expired => "expired",
        _ => "unknown",
    }
}

fn exclusion_reason(reason: ExclusionReason) -> &'static str {
    match reason {
        ExclusionReason::SourceFiltered => "source_filtered",
        ExclusionReason::NotCurrentlyValid => "not_currently_valid",
        ExclusionReason::TokenBudget => "token_budget",
        ExclusionReason::ProfileQuota => "profile_quota",
        _ => "unknown",
    }
}

fn inclusion_reason(reason: InclusionReason) -> &'static str {
    match reason {
        InclusionReason::SectionReservation => "section_reservation",
        InclusionReason::ValuePerToken => "value_per_token",
        InclusionReason::LocalReplacement => "local_replacement",
        _ => "unknown",
    }
}
