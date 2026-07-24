// SPDX-License-Identifier: BUSL-1.1
//! Filtrage et deduplication des resultats du recall.

use std::collections::HashMap;

use super::{
    ContextBundle, ContextItem, ContextRequest, ContextRole, ContextSourcePolicy, ContextWarning, DedupCluster,
    ExcludedMemory, ExclusionReason, InclusionReason, MergedMemory, RetrievalContribution, TokenEstimator, render,
    selection, temporal,
};
use crate::{MemoryLayer, Record, TrustLevel};

#[derive(Debug, PartialEq, Eq, Hash)]
struct DedupKey {
    text: String,
    metadata: MetadataKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct MetadataKey {
    layer: MemoryLayer,
    trust: TrustLevel,
    valid_from: i64,
    valid_until: Option<i64>,
}

struct TextMetadataGroup {
    metadata: MetadataKey,
    memory_ids: Vec<String>,
    incompatible: bool,
}

pub(super) fn compile_records(
    records: Vec<Record>,
    request: &ContextRequest<'_>,
    estimator: &dyn TokenEstimator,
    compiled_at: i64,
) -> ContextBundle {
    let mut candidates = Vec::<ContextItem>::with_capacity(records.len());
    let mut representative_by_key = HashMap::<DedupKey, usize>::with_capacity(records.len());
    let mut text_group_by_key = HashMap::<String, usize>::with_capacity(records.len());
    let mut text_groups = Vec::<TextMetadataGroup>::new();
    let mut merged = Vec::new();
    let mut excluded = Vec::new();

    for (retrieval_rank, record) in records.into_iter().enumerate() {
        let trust = record.trust();
        let role = ContextRole::derive(record.layer, trust);
        let temporal_status = temporal::status(record.validity, compiled_at);
        let retrieval_score = finite_score(record.score);
        let retrieval_contribution = RetrievalContribution {
            memory_id: record.id.clone(),
            retrieval_rank,
            retrieval_score,
        };
        if !source_allowed(trust, request.source_policy) {
            push_exclusion(
                &mut excluded,
                record.id,
                ExclusionReason::SourceFiltered,
                temporal_status,
                role,
                retrieval_contribution,
            );
            continue;
        }

        if temporal_status != super::ContextTemporalStatus::Current {
            push_exclusion(
                &mut excluded,
                record.id,
                ExclusionReason::NotCurrentlyValid,
                temporal_status,
                role,
                retrieval_contribution,
            );
            continue;
        }

        let text = normalize_text(&record.text);
        let normalized_key = text.to_lowercase();
        let metadata = MetadataKey {
            layer: record.layer,
            trust,
            valid_from: record.validity.valid_from,
            valid_until: record.validity.valid_until,
        };
        record_text_metadata(
            &mut text_group_by_key,
            &mut text_groups,
            normalized_key.clone(),
            metadata,
            &record.id,
        );
        let dedup_key = DedupKey {
            text: normalized_key,
            metadata,
        };
        if let Some(index) = representative_by_key.get(&dedup_key).copied() {
            merged.push(MergedMemory {
                memory_id: record.id.clone(),
                representative_memory_id: candidates[index].source_memory_ids[0].clone(),
            });
            candidates[index].source_memory_ids.push(record.id.clone());
            candidates[index].retrieval_contributions.push(retrieval_contribution);
            continue;
        }

        let index = candidates.len();
        representative_by_key.insert(dedup_key, index);
        candidates.push(ContextItem {
            estimated_tokens: 0,
            text,
            source_memory_ids: vec![record.id],
            layer: record.layer,
            trust,
            role,
            validity: record.validity,
            temporal_status,
            retrieval_score,
            retrieval_rank,
            retrieval_contributions: vec![retrieval_contribution],
            utility_score: 0.0,
            value_per_token: 0.0,
            freshness_score: 0.0,
            inclusion_reason: InclusionReason::ValuePerToken,
        });
    }

    let dedup_clusters = candidates
        .iter()
        .filter(|item| item.source_memory_ids.len() > 1)
        .map(|item| DedupCluster {
            representative_memory_id: item.source_memory_ids[0].clone(),
            memory_ids: item.source_memory_ids.clone(),
        })
        .collect();
    let warnings = text_groups
        .into_iter()
        .filter(|group| group.incompatible)
        .map(|group| ContextWarning::IncompatibleMetadata {
            memory_ids: group.memory_ids,
        })
        .collect();

    let outcome = selection::select_under_budget(
        candidates,
        request.token_budget,
        estimator,
        compiled_at,
        request.profile,
        request.render_format,
    );
    for rejected in outcome.rejected {
        for contribution in rejected.item.retrieval_contributions {
            push_exclusion(
                &mut excluded,
                contribution.memory_id.clone(),
                rejected.reason,
                rejected.item.temporal_status,
                rejected.item.role,
                contribution,
            );
        }
    }

    render::build_bundle(
        render::BundleInputs {
            items: outcome.selected,
            merged,
            excluded,
            dedup_clusters,
            warnings,
            compiled_at,
            profile: request.profile,
            render_format: request.render_format,
            trace_level: request.trace_level,
        },
        estimator,
    )
}

fn finite_score(score: f32) -> f32 {
    if score.is_finite() { score } else { 0.0 }
}

fn source_allowed(trust: TrustLevel, policy: ContextSourcePolicy) -> bool {
    match policy {
        ContextSourcePolicy::AllowAll => true,
        ContextSourcePolicy::ExcludeImported => trust != TrustLevel::Import,
        ContextSourcePolicy::UserAndConsolidationOnly => {
            matches!(trust, TrustLevel::User | TrustLevel::Consolidation)
        }
    }
}

fn normalize_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn record_text_metadata(
    group_by_key: &mut HashMap<String, usize>,
    groups: &mut Vec<TextMetadataGroup>,
    normalized_text: String,
    metadata: MetadataKey,
    memory_id: &str,
) {
    if let Some(index) = group_by_key.get(&normalized_text).copied() {
        let group = &mut groups[index];
        group.incompatible |= group.metadata != metadata;
        group.memory_ids.push(memory_id.to_string());
        return;
    }

    let index = groups.len();
    group_by_key.insert(normalized_text, index);
    groups.push(TextMetadataGroup {
        metadata,
        memory_ids: vec![memory_id.to_string()],
        incompatible: false,
    });
}

fn push_exclusion(
    exclusions: &mut Vec<ExcludedMemory>,
    memory_id: String,
    reason: ExclusionReason,
    temporal_status: super::ContextTemporalStatus,
    role: ContextRole,
    retrieval_contribution: RetrievalContribution,
) {
    exclusions.push(ExcludedMemory {
        memory_id,
        reason,
        temporal_status,
        role,
        retrieval_contribution,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::ApproximateTokenEstimator;
    use crate::{MemoryLayer, Record, Validity};

    const COMPILED_AT: i64 = 100;

    fn record(id: &str, text: &str, layer: MemoryLayer, source: &str) -> Record {
        Record {
            id: id.to_string(),
            text: text.to_string(),
            layer,
            score: 0.5,
            source: source.to_string(),
            validity: Validity::since(0),
        }
    }

    #[test]
    fn compilation_deduplicates_and_preserves_all_citations() {
        let request = ContextRequest::new("query", 1_000).explain();
        let bundle = compile_records(
            vec![
                record("m1", "BaseMyAI is native-only", MemoryLayer::Semantic, "user"),
                record("m2", "  basemyai   IS native-only  ", MemoryLayer::Semantic, "user"),
            ],
            &request,
            &ApproximateTokenEstimator,
            COMPILED_AT,
        );

        assert_eq!(bundle.sections.len(), 1);
        assert_eq!(bundle.sections[0].items.len(), 1);
        assert_eq!(bundle.sections[0].items[0].source_memory_ids, ["m1", "m2"]);
        assert_eq!(
            bundle.sections[0].items[0]
                .retrieval_contributions
                .iter()
                .map(|contribution| contribution.memory_id.as_str())
                .collect::<Vec<_>>(),
            ["m1", "m2"]
        );
        assert_eq!(bundle.citations.len(), 2);
        assert!(bundle.excluded.is_empty());
        assert_eq!(bundle.merged.len(), 1);
        assert_eq!(bundle.merged[0].memory_id, "m2");
        assert_eq!(bundle.merged[0].representative_memory_id, "m1");
        assert_eq!(bundle.dedup_clusters.len(), 1);
        assert_eq!(bundle.dedup_clusters[0].memory_ids, ["m1", "m2"]);
        assert_eq!(bundle.trace.summary.dedup_clusters, 1);
    }

    #[test]
    fn compilation_does_not_merge_incompatible_metadata() {
        let request = ContextRequest::new("query", 1_000).include_procedural().explain();
        let mut newer = record("newer", "same text", MemoryLayer::Semantic, "user");
        newer.validity = Validity::since(50);
        let bundle = compile_records(
            vec![
                record("semantic", "same text", MemoryLayer::Semantic, "user"),
                record("procedural", "same text", MemoryLayer::Procedural, "user"),
                record("consolidated", "same text", MemoryLayer::Semantic, "consolidation"),
                newer,
            ],
            &request,
            &ApproximateTokenEstimator,
            COMPILED_AT,
        );

        assert_eq!(
            bundle.sections.iter().map(|section| section.items.len()).sum::<usize>(),
            4
        );
        assert!(bundle.merged.is_empty());
        assert!(bundle.excluded.is_empty());
        assert_eq!(bundle.warnings.len(), 1);
    }

    #[test]
    fn compilation_filters_sources_and_reports_the_reason() {
        let request = ContextRequest::new("query", 1_000)
            .source_policy(ContextSourcePolicy::UserAndConsolidationOnly)
            .explain();
        let bundle = compile_records(
            vec![
                record("user", "kept", MemoryLayer::Semantic, "user"),
                record("import", "filtered", MemoryLayer::Semantic, "import"),
                record("unknown", "filtered too", MemoryLayer::Semantic, "custom"),
            ],
            &request,
            &ApproximateTokenEstimator,
            COMPILED_AT,
        );

        assert_eq!(bundle.sections[0].items.len(), 1);
        assert_eq!(bundle.excluded.len(), 2);
        assert!(
            bundle
                .excluded
                .iter()
                .all(|excluded| excluded.reason == ExclusionReason::SourceFiltered)
        );
    }

    #[test]
    fn compilation_never_exceeds_the_estimated_budget() {
        let estimator = ApproximateTokenEstimator;
        let request = ContextRequest::new("query", 20).explain();
        let bundle = compile_records(
            vec![
                record("m1", "a compact fact", MemoryLayer::Semantic, "user"),
                record(
                    "m2",
                    "a much longer fact that cannot fit in the remaining context budget",
                    MemoryLayer::Semantic,
                    "user",
                ),
            ],
            &request,
            &estimator,
            COMPILED_AT,
        );

        assert!(bundle.estimated_tokens <= request.token_budget());
        assert!(
            bundle
                .excluded
                .iter()
                .any(|excluded| excluded.reason == ExclusionReason::TokenBudget)
        );
        assert!(
            bundle
                .sections
                .iter()
                .flat_map(|section| &section.items)
                .all(|item| matches!(
                    item.inclusion_reason,
                    InclusionReason::SectionReservation
                        | InclusionReason::ValuePerToken
                        | InclusionReason::LocalReplacement
                ))
        );
    }

    #[test]
    fn compilation_is_deterministic_for_identical_input() {
        let request = ContextRequest::new("query", 1_000)
            .include_procedural()
            .coding_profile()
            .render_json()
            .explain();
        let records = vec![
            record("m1", "fact", MemoryLayer::Semantic, "user"),
            record("m2", "procedure", MemoryLayer::Procedural, "user"),
        ];
        let first = compile_records(records.clone(), &request, &ApproximateTokenEstimator, COMPILED_AT);
        let second = compile_records(records, &request, &ApproximateTokenEstimator, COMPILED_AT);
        assert_eq!(first, second);
    }

    #[test]
    fn compilation_rejects_non_current_records_defensively() {
        let request = ContextRequest::new("query", 1_000).explain();
        let mut scheduled = record("scheduled", "future", MemoryLayer::Semantic, "user");
        scheduled.validity = Validity::since(COMPILED_AT + 1);
        let mut expired = record("expired", "past", MemoryLayer::Semantic, "user");
        expired.validity = Validity {
            valid_from: 0,
            valid_until: Some(COMPILED_AT),
        };

        let bundle = compile_records(
            vec![scheduled, expired],
            &request,
            &ApproximateTokenEstimator,
            COMPILED_AT,
        );

        assert!(bundle.sections.is_empty());
        assert_eq!(bundle.excluded.len(), 2);
        let scheduled = bundle
            .excluded
            .iter()
            .find(|excluded| excluded.memory_id == "scheduled")
            .expect("scheduled exclusion");
        let expired = bundle
            .excluded
            .iter()
            .find(|excluded| excluded.memory_id == "expired")
            .expect("expired exclusion");
        assert_eq!(scheduled.reason, ExclusionReason::NotCurrentlyValid);
        assert_eq!(
            scheduled.temporal_status,
            super::super::ContextTemporalStatus::Scheduled
        );
        assert_eq!(expired.reason, ExclusionReason::NotCurrentlyValid);
        assert_eq!(expired.temporal_status, super::super::ContextTemporalStatus::Expired);
    }

    #[test]
    fn compact_trace_keeps_counts_without_individual_events() {
        let request = ContextRequest::new("query", 1_000);
        let bundle = compile_records(
            vec![record("m1", "fact", MemoryLayer::Semantic, "user")],
            &request,
            &ApproximateTokenEstimator,
            COMPILED_AT,
        );

        assert_eq!(bundle.trace.summary.included_items, 1);
        assert_eq!(bundle.trace.total_events, 1);
        assert!(bundle.trace.events.is_empty());
        assert!(bundle.excluded.is_empty());
        assert!(bundle.merged.is_empty());
    }

    #[test]
    fn detailed_trace_is_strictly_bounded_and_reports_truncation() {
        let request = ContextRequest::new("query", 1_000)
            .source_policy(ContextSourcePolicy::UserAndConsolidationOnly)
            .explain();
        let records = (0..160)
            .map(|index| {
                record(
                    &format!("import-{index:03}"),
                    &format!("filtered {index}"),
                    MemoryLayer::Semantic,
                    "import",
                )
            })
            .collect();
        let bundle = compile_records(records, &request, &ApproximateTokenEstimator, COMPILED_AT);

        assert_eq!(bundle.trace.summary.excluded_memories, 160);
        assert_eq!(bundle.trace.total_events, 160);
        assert_eq!(bundle.trace.events.len(), super::super::MAX_CONTEXT_TRACE_EVENTS);
        assert!(bundle.trace.truncated);
        assert_eq!(bundle.excluded.len(), 160);
    }

    #[test]
    fn conflict_warnings_never_infer_contradictions_from_free_text() {
        let request = ContextRequest::new("query", 1_000).explain();
        let bundle = compile_records(
            vec![
                record("enabled", "feature is enabled", MemoryLayer::Semantic, "user"),
                record("disabled", "feature is disabled", MemoryLayer::Semantic, "user"),
            ],
            &request,
            &ApproximateTokenEstimator,
            COMPILED_AT,
        );

        assert!(bundle.warnings.is_empty());
    }
}
