// SPDX-License-Identifier: BUSL-1.1
//! Utility ranking et selection bornee sous budget.

use std::collections::HashMap;

use super::{
    ContextItem, ContextProfile, ContextRenderFormat, ContextRole, ContextSectionKind, ExclusionReason,
    InclusionReason, MAX_CONTEXT_CANDIDATES, TokenEstimator, render, temporal,
};
use crate::{MemoryLayer, TrustLevel};

#[derive(Debug, Clone)]
struct Candidate {
    item: ContextItem,
    selected: bool,
}

pub(super) struct RejectedItem {
    pub(super) item: ContextItem,
    pub(super) reason: ExclusionReason,
}

pub(super) struct SelectionOutcome {
    pub(super) selected: Vec<ContextItem>,
    pub(super) rejected: Vec<RejectedItem>,
}

pub(super) fn select_under_budget(
    items: Vec<ContextItem>,
    token_budget: usize,
    estimator: &dyn TokenEstimator,
    compiled_at: i64,
    profile: ContextProfile,
    render_format: ContextRenderFormat,
) -> SelectionOutcome {
    let mut candidates: Vec<Candidate> = items
        .into_iter()
        .map(|item| Candidate { item, selected: false })
        .collect();

    score_candidates(&mut candidates, compiled_at, estimator, profile, render_format);
    reserve_sections(&mut candidates, token_budget, estimator, profile, render_format);
    fill_by_value(&mut candidates, token_budget, estimator, profile, render_format);
    improve_by_replacement(&mut candidates, token_budget, estimator, profile, render_format);
    fill_by_value(&mut candidates, token_budget, estimator, profile, render_format);
    enforce_rendered_budget(&mut candidates, token_budget, estimator, render_format);

    let mut selected = Vec::new();
    let mut rejected = Vec::new();
    let selected_by_role = selected_counts(&candidates);
    for candidate in candidates {
        if candidate.selected {
            selected.push(candidate.item);
        } else {
            let reason = if selected_by_role.get(&candidate.item.role).copied().unwrap_or_default()
                >= role_quota(profile, candidate.item.role)
            {
                ExclusionReason::ProfileQuota
            } else {
                ExclusionReason::TokenBudget
            };
            rejected.push(RejectedItem {
                item: candidate.item,
                reason,
            });
        }
    }
    selected.sort_by_key(|item| item.retrieval_rank);
    rejected.sort_by_key(|rejected| rejected.item.retrieval_rank);
    SelectionOutcome { selected, rejected }
}

fn score_candidates(
    candidates: &mut [Candidate],
    compiled_at: i64,
    estimator: &dyn TokenEstimator,
    profile: ContextProfile,
    render_format: ContextRenderFormat,
) {
    let max_score = candidates
        .iter()
        .map(|candidate| f64::from(candidate.item.retrieval_score.max(0.0)))
        .fold(0.0_f64, f64::max);

    for candidate in candidates {
        let relevance = if max_score > 0.0 {
            f64::from(candidate.item.retrieval_score.max(0.0)) / max_score
        } else {
            1.0 / (candidate.item.retrieval_rank.saturating_add(1) as f64)
        };
        let freshness = temporal::freshness_weight(candidate.item.validity, compiled_at);
        let utility = relevance
            * layer_weight(candidate.item.layer)
            * source_weight(candidate.item.trust)
            * profile_weight(profile, candidate.item.role)
            * freshness;
        candidate.item.estimated_tokens =
            estimator.estimate(&render::render_item_refs(&[&candidate.item], render_format));
        candidate.item.freshness_score = freshness;
        candidate.item.utility_score = utility;
        candidate.item.value_per_token = utility / candidate.item.estimated_tokens.max(1) as f64;
    }
}

fn layer_weight(layer: MemoryLayer) -> f64 {
    match layer {
        MemoryLayer::ShortTerm => 1.10,
        MemoryLayer::Semantic => 1.00,
        MemoryLayer::Procedural => 1.05,
        MemoryLayer::Episodic => 0.90,
    }
}

/// Priorite de compilation uniquement : ne certifie jamais la securite d'une source.
fn source_weight(trust: TrustLevel) -> f64 {
    match trust {
        TrustLevel::User => 1.00,
        TrustLevel::Consolidation => 1.05,
        TrustLevel::Import => 0.85,
        TrustLevel::Unknown => 0.75,
    }
}

fn profile_weight(profile: ContextProfile, role: ContextRole) -> f64 {
    match profile {
        ContextProfile::Balanced => 1.0,
        ContextProfile::Conversation => match role {
            ContextRole::Fact => 1.05,
            ContextRole::Constraint => 1.15,
            ContextRole::Procedure => 0.80,
            ContextRole::Event => 1.10,
            ContextRole::Reference => 0.90,
            ContextRole::UncertainData => 0.70,
        },
        ContextProfile::Coding => match role {
            ContextRole::Fact => 1.05,
            ContextRole::Constraint => 1.15,
            ContextRole::Procedure => 1.25,
            ContextRole::Event => 0.75,
            ContextRole::Reference => 1.00,
            ContextRole::UncertainData => 0.65,
        },
        ContextProfile::Execution => match role {
            ContextRole::Fact => 1.00,
            ContextRole::Constraint => 1.25,
            ContextRole::Procedure => 1.20,
            ContextRole::Event => 0.70,
            ContextRole::Reference => 0.80,
            ContextRole::UncertainData => 0.50,
        },
        ContextProfile::SafetyCritical => match role {
            ContextRole::Fact => 1.00,
            ContextRole::Constraint => 1.30,
            ContextRole::Procedure => 1.15,
            ContextRole::Event => 0.65,
            ContextRole::Reference => 1.10,
            ContextRole::UncertainData => 0.25,
        },
    }
}

fn role_quota(profile: ContextProfile, role: ContextRole) -> usize {
    match profile {
        ContextProfile::Balanced => MAX_CONTEXT_CANDIDATES,
        ContextProfile::Conversation => match role {
            ContextRole::Fact => 64,
            ContextRole::Constraint | ContextRole::Event => 32,
            ContextRole::Procedure => 8,
            ContextRole::Reference => 16,
            ContextRole::UncertainData => 4,
        },
        ContextProfile::Coding => match role {
            ContextRole::Fact => 64,
            ContextRole::Constraint | ContextRole::Procedure | ContextRole::Reference => 32,
            ContextRole::Event => 8,
            ContextRole::UncertainData => 2,
        },
        ContextProfile::Execution => match role {
            ContextRole::Fact | ContextRole::Constraint | ContextRole::Procedure => 32,
            ContextRole::Event => 4,
            ContextRole::Reference => 8,
            ContextRole::UncertainData => 2,
        },
        ContextProfile::SafetyCritical => match role {
            ContextRole::Fact | ContextRole::Constraint | ContextRole::Procedure => 32,
            ContextRole::Event => 4,
            ContextRole::Reference => 16,
            ContextRole::UncertainData => 1,
        },
    }
}

fn reserve_sections(
    candidates: &mut [Candidate],
    token_budget: usize,
    estimator: &dyn TokenEstimator,
    profile: ContextProfile,
    render_format: ContextRenderFormat,
) {
    let mut representatives = HashMap::<ContextSectionKind, usize>::new();
    for (index, candidate) in candidates.iter().enumerate() {
        let section = ContextSectionKind::from_layer(candidate.item.layer);
        let replace = representatives
            .get(&section)
            .is_none_or(|current| utility_order(candidate, &candidates[*current]).is_lt());
        if replace {
            representatives.insert(section, index);
        }
    }

    let mut ordered: Vec<usize> = representatives.into_values().collect();
    ordered.sort_by(|left, right| utility_order(&candidates[*left], &candidates[*right]));
    for index in ordered {
        try_select(
            candidates,
            index,
            token_budget,
            estimator,
            profile,
            render_format,
            InclusionReason::SectionReservation,
        );
    }
}

fn fill_by_value(
    candidates: &mut [Candidate],
    token_budget: usize,
    estimator: &dyn TokenEstimator,
    profile: ContextProfile,
    render_format: ContextRenderFormat,
) {
    let mut ordered: Vec<usize> = (0..candidates.len())
        .filter(|index| !candidates[*index].selected)
        .collect();
    ordered.sort_by(|left, right| value_order(&candidates[*left], &candidates[*right]));
    for index in ordered {
        try_select(
            candidates,
            index,
            token_budget,
            estimator,
            profile,
            render_format,
            InclusionReason::ValuePerToken,
        );
    }
}

fn improve_by_replacement(
    candidates: &mut [Candidate],
    token_budget: usize,
    estimator: &dyn TokenEstimator,
    profile: ContextProfile,
    render_format: ContextRenderFormat,
) {
    let mut incoming: Vec<usize> = (0..candidates.len())
        .filter(|index| !candidates[*index].selected)
        .collect();
    incoming.sort_by(|left, right| utility_order(&candidates[*left], &candidates[*right]));

    for incoming_index in incoming {
        if candidates[incoming_index].selected {
            continue;
        }
        let mut outgoing: Vec<usize> = (0..candidates.len())
            .filter(|index| {
                candidates[*index].selected
                    && candidates[*index].item.utility_score < candidates[incoming_index].item.utility_score
                    && removal_preserves_section(candidates, *index, incoming_index)
            })
            .collect();
        outgoing.sort_by(|left, right| {
            candidates[*left]
                .item
                .utility_score
                .total_cmp(&candidates[*right].item.utility_score)
                .then_with(|| {
                    candidates[*right]
                        .item
                        .retrieval_rank
                        .cmp(&candidates[*left].item.retrieval_rank)
                })
        });

        for outgoing_index in outgoing {
            candidates[outgoing_index].selected = false;
            candidates[incoming_index].selected = true;
            candidates[incoming_index].item.inclusion_reason = InclusionReason::LocalReplacement;
            if selection_fits(candidates, token_budget, estimator, profile, render_format) {
                break;
            }
            candidates[incoming_index].selected = false;
            candidates[outgoing_index].selected = true;
        }
    }
}

fn removal_preserves_section(candidates: &[Candidate], outgoing_index: usize, incoming_index: usize) -> bool {
    let outgoing_section = ContextSectionKind::from_layer(candidates[outgoing_index].item.layer);
    let incoming_section = ContextSectionKind::from_layer(candidates[incoming_index].item.layer);
    outgoing_section == incoming_section
        || candidates
            .iter()
            .filter(|candidate| {
                candidate.selected && ContextSectionKind::from_layer(candidate.item.layer) == outgoing_section
            })
            .count()
            > 1
}

fn try_select(
    candidates: &mut [Candidate],
    index: usize,
    token_budget: usize,
    estimator: &dyn TokenEstimator,
    profile: ContextProfile,
    render_format: ContextRenderFormat,
    reason: InclusionReason,
) -> bool {
    if candidates[index].selected {
        return true;
    }
    candidates[index].selected = true;
    candidates[index].item.inclusion_reason = reason;
    if selection_fits(candidates, token_budget, estimator, profile, render_format) {
        true
    } else {
        candidates[index].selected = false;
        false
    }
}

fn selection_fits(
    candidates: &[Candidate],
    token_budget: usize,
    estimator: &dyn TokenEstimator,
    profile: ContextProfile,
    render_format: ContextRenderFormat,
) -> bool {
    quotas_fit(candidates, profile) && estimated_selection_tokens(candidates, estimator, render_format) <= token_budget
}

fn quotas_fit(candidates: &[Candidate], profile: ContextProfile) -> bool {
    selected_counts(candidates)
        .into_iter()
        .all(|(role, count)| count <= role_quota(profile, role))
}

fn selected_counts(candidates: &[Candidate]) -> HashMap<ContextRole, usize> {
    let mut counts = HashMap::new();
    for candidate in candidates.iter().filter(|candidate| candidate.selected) {
        *counts.entry(candidate.item.role).or_insert(0) += 1;
    }
    counts
}

fn estimated_selection_tokens(
    candidates: &[Candidate],
    estimator: &dyn TokenEstimator,
    render_format: ContextRenderFormat,
) -> usize {
    let selected: Vec<&ContextItem> = candidates
        .iter()
        .filter(|candidate| candidate.selected)
        .map(|candidate| &candidate.item)
        .collect();
    estimator.estimate(&render::render_item_refs(&selected, render_format))
}

fn enforce_rendered_budget(
    candidates: &mut [Candidate],
    token_budget: usize,
    estimator: &dyn TokenEstimator,
    render_format: ContextRenderFormat,
) {
    loop {
        let selected: Vec<&ContextItem> = candidates
            .iter()
            .filter(|candidate| candidate.selected)
            .map(|candidate| &candidate.item)
            .collect();
        if estimator.estimate(&render::render_item_refs(&selected, render_format)) <= token_budget {
            return;
        }

        let Some(index) = candidates
            .iter()
            .enumerate()
            .filter(|(_, candidate)| candidate.selected)
            .min_by(|(_, left), (_, right)| {
                left.item
                    .value_per_token
                    .total_cmp(&right.item.value_per_token)
                    .then_with(|| left.item.utility_score.total_cmp(&right.item.utility_score))
                    .then_with(|| right.item.retrieval_rank.cmp(&left.item.retrieval_rank))
            })
            .map(|(index, _)| index)
        else {
            return;
        };
        candidates[index].selected = false;
    }
}

fn utility_order(left: &Candidate, right: &Candidate) -> std::cmp::Ordering {
    right
        .item
        .utility_score
        .total_cmp(&left.item.utility_score)
        .then_with(|| left.item.retrieval_rank.cmp(&right.item.retrieval_rank))
}

fn value_order(left: &Candidate, right: &Candidate) -> std::cmp::Ordering {
    right
        .item
        .value_per_token
        .total_cmp(&left.item.value_per_token)
        .then_with(|| utility_order(left, right))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::Validity;

    fn item(id: &str, text: &str, layer: MemoryLayer, score: f32, tokens: usize, rank: usize) -> ContextItem {
        ContextItem {
            text: text.to_string(),
            source_memory_ids: vec![id.to_string()],
            layer,
            trust: TrustLevel::User,
            role: ContextRole::derive(layer, TrustLevel::User),
            validity: Validity::since(0),
            temporal_status: super::super::ContextTemporalStatus::Current,
            retrieval_score: score,
            retrieval_rank: rank,
            retrieval_contributions: vec![super::super::RetrievalContribution {
                memory_id: id.to_string(),
                retrieval_rank: rank,
                retrieval_score: score,
            }],
            estimated_tokens: tokens,
            utility_score: 0.0,
            value_per_token: 0.0,
            freshness_score: 0.0,
            inclusion_reason: InclusionReason::ValuePerToken,
        }
    }

    fn select(
        items: Vec<ContextItem>,
        token_budget: usize,
        estimator: &dyn TokenEstimator,
        compiled_at: i64,
    ) -> SelectionOutcome {
        select_under_budget(
            items,
            token_budget,
            estimator,
            compiled_at,
            ContextProfile::Balanced,
            ContextRenderFormat::Markdown,
        )
    }

    struct ItemCountEstimator;

    impl TokenEstimator for ItemCountEstimator {
        fn estimate(&self, text: &str) -> usize {
            text.matches("[memory:").count()
        }
    }

    struct MarkerCostEstimator;

    impl TokenEstimator for MarkerCostEstimator {
        fn estimate(&self, text: &str) -> usize {
            text.matches("TOP").count() * 5
                + text.matches("LONG").count() * 4
                + text.matches("EXPENSIVE").count() * 10
                + text.matches("DENSE").count() * 2
        }
    }

    struct NonAdditiveEstimator;

    impl TokenEstimator for NonAdditiveEstimator {
        fn estimate(&self, text: &str) -> usize {
            match text.matches("[memory:").count() {
                0 => 0,
                1 => 1,
                _ => 100,
            }
        }
    }

    fn selected_ids(outcome: &SelectionOutcome) -> Vec<&str> {
        outcome
            .selected
            .iter()
            .flat_map(|item| item.source_memory_ids.iter().map(String::as_str))
            .collect()
    }

    #[test]
    fn section_reservation_preserves_context_diversity_under_budget() {
        let outcome = select(
            vec![
                item("fact-1", "primary fact", MemoryLayer::Semantic, 1.0, 1, 0),
                item("fact-2", "secondary fact", MemoryLayer::Semantic, 0.9, 1, 1),
                item("event", "relevant event", MemoryLayer::Episodic, 0.2, 1, 2),
            ],
            2,
            &ItemCountEstimator,
            0,
        );

        let ids = selected_ids(&outcome);
        assert!(ids.contains(&"fact-1"));
        assert!(ids.contains(&"event"));
        assert!(!ids.contains(&"fact-2"));
    }

    #[test]
    fn value_per_token_fill_prefers_dense_candidates() {
        let outcome = select(
            vec![
                item("top", "TOP", MemoryLayer::Semantic, 1.0, 5, 0),
                item("expensive", "EXPENSIVE", MemoryLayer::Semantic, 0.8, 10, 1),
                item("dense", "DENSE", MemoryLayer::Semantic, 0.6, 2, 2),
            ],
            7,
            &MarkerCostEstimator,
            0,
        );

        let ids = selected_ids(&outcome);
        assert!(ids.contains(&"top"));
        assert!(ids.contains(&"dense"));
        assert!(!ids.contains(&"expensive"));
    }

    #[test]
    fn local_replacement_increases_total_utility() {
        let outcome = select(
            vec![
                item("top", "TOP", MemoryLayer::Semantic, 1.0, 5, 0),
                item("long", "LONG", MemoryLayer::Semantic, 0.8, 4, 1),
                item("dense", "DENSE", MemoryLayer::Semantic, 0.5, 2, 2),
            ],
            9,
            &MarkerCostEstimator,
            0,
        );

        let ids = selected_ids(&outcome);
        assert!(ids.contains(&"top"));
        assert!(ids.contains(&"long"));
        assert!(!ids.contains(&"dense"));
        let total_utility: f64 = outcome.selected.iter().map(|item| item.utility_score).sum();
        assert!((total_utility - 1.8).abs() < 1e-6);
    }

    #[test]
    fn freshness_softly_prefers_the_newer_equally_relevant_item() {
        let compiled_at = 365 * 24 * 60 * 60;
        let mut recent = item("recent", "recent", MemoryLayer::Semantic, 1.0, 1, 1);
        recent.validity = Validity::since(compiled_at);
        let old = item("old", "old", MemoryLayer::Semantic, 1.0, 1, 0);

        let outcome = select(vec![old, recent], 1, &ItemCountEstimator, compiled_at);
        let ids = selected_ids(&outcome);
        assert!(ids.contains(&"recent"));
        assert!(!ids.contains(&"old"));
    }

    #[test]
    fn rendered_citation_cost_contributes_to_value_per_token() {
        let compact = item("a", "same", MemoryLayer::Semantic, 1.0, 0, 0);
        let verbose = item(
            "an-id-with-much-more-rendered-overhead",
            "same other",
            MemoryLayer::Semantic,
            1.0,
            0,
            1,
        );

        let outcome = select(
            vec![compact, verbose],
            usize::MAX,
            &super::super::ApproximateTokenEstimator,
            0,
        );
        let compact = outcome
            .selected
            .iter()
            .find(|item| item.source_memory_ids[0] == "a")
            .expect("compact item");
        let verbose = outcome
            .selected
            .iter()
            .find(|item| item.source_memory_ids[0].starts_with("an-id"))
            .expect("verbose item");
        assert!(compact.estimated_tokens < verbose.estimated_tokens);
        assert!(compact.value_per_token > verbose.value_per_token);
    }

    #[test]
    fn exact_final_check_handles_non_additive_estimators() {
        let outcome = select(
            vec![
                item("first", "first", MemoryLayer::Semantic, 1.0, 0, 0),
                item("second", "second", MemoryLayer::Semantic, 0.9, 0, 1),
            ],
            2,
            &NonAdditiveEstimator,
            0,
        );

        assert_eq!(outcome.selected.len(), 1);
        let rendered = render::render_item_refs(
            &outcome.selected.iter().collect::<Vec<_>>(),
            ContextRenderFormat::Markdown,
        );
        assert!(NonAdditiveEstimator.estimate(&rendered) <= 2);
    }

    #[test]
    fn profiles_change_role_priority_deterministically() {
        let procedure = item("procedure", "procedure", MemoryLayer::Procedural, 1.0, 1, 0);
        let event = item("event", "event", MemoryLayer::Episodic, 1.0, 1, 1);

        let conversation = select_under_budget(
            vec![procedure.clone(), event.clone()],
            1,
            &ItemCountEstimator,
            0,
            ContextProfile::Conversation,
            ContextRenderFormat::Markdown,
        );
        let coding = select_under_budget(
            vec![procedure, event],
            1,
            &ItemCountEstimator,
            0,
            ContextProfile::Coding,
            ContextRenderFormat::Markdown,
        );

        assert_eq!(selected_ids(&conversation), ["event"]);
        assert_eq!(selected_ids(&coding), ["procedure"]);
    }

    #[test]
    fn safety_profile_quota_never_becomes_a_permission_filter() {
        let mut uncertain = (0..3)
            .map(|index| {
                let mut item = item(
                    &format!("uncertain-{index}"),
                    "uncertain",
                    MemoryLayer::Semantic,
                    1.0,
                    1,
                    index,
                );
                item.trust = TrustLevel::Unknown;
                item.role = ContextRole::UncertainData;
                item
            })
            .collect::<Vec<_>>();
        uncertain[1].retrieval_score = 0.9;
        uncertain[2].retrieval_score = 0.8;

        let outcome = select_under_budget(
            uncertain,
            usize::MAX,
            &ItemCountEstimator,
            0,
            ContextProfile::SafetyCritical,
            ContextRenderFormat::Markdown,
        );

        assert_eq!(outcome.selected.len(), 1);
        assert_eq!(outcome.rejected.len(), 2);
        assert!(
            outcome
                .rejected
                .iter()
                .all(|rejected| rejected.reason == ExclusionReason::ProfileQuota)
        );
    }

    #[test]
    fn every_renderer_is_checked_against_its_complete_output() {
        let estimator = super::super::ApproximateTokenEstimator;
        for format in [
            ContextRenderFormat::Text,
            ContextRenderFormat::Markdown,
            ContextRenderFormat::Json,
        ] {
            let outcome = select_under_budget(
                vec![
                    item("first", "compact", MemoryLayer::Semantic, 1.0, 0, 0),
                    item(
                        "second",
                        "a deliberately longer candidate with rendering overhead",
                        MemoryLayer::Semantic,
                        0.9,
                        0,
                        1,
                    ),
                ],
                24,
                &estimator,
                0,
                ContextProfile::Balanced,
                format,
            );
            let rendered = render::render_item_refs(&outcome.selected.iter().collect::<Vec<_>>(), format);
            assert!(estimator.estimate(&rendered) <= 24, "{format:?}: {rendered}");
        }
    }
}
