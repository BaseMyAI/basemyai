use std::collections::{BTreeMap, BTreeSet};

use crate::report::{BundleMetrics, RetrievalMetrics};
use crate::schema::{EvalCase, Provenance, SourcePolicy};

pub(crate) fn retrieval_metrics(case: &EvalCase, ids: &[String]) -> RetrievalMetrics {
    let relevance: BTreeMap<&str, u8> = case
        .memories
        .iter()
        .map(|memory| (memory.id.as_str(), memory.relevance))
        .collect();
    let relevant_total = relevance.values().filter(|grade| **grade > 0).count();
    let relevant_hits = ids
        .iter()
        .take(case.k)
        .filter(|id| relevance.get(id.as_str()).is_some_and(|grade| *grade > 0))
        .count();
    let first_relevant = ids
        .iter()
        .take(case.k)
        .position(|id| relevance.get(id.as_str()).is_some_and(|grade| *grade > 0));

    let gains: Vec<f64> = ids
        .iter()
        .take(case.k)
        .map(|id| f64::from(*relevance.get(id.as_str()).unwrap_or(&0)))
        .collect();
    let has_graded_relevance = relevance.values().any(|grade| *grade > 1);
    let ndcg_at_k = has_graded_relevance.then(|| {
        let mut ideal: Vec<f64> = relevance.values().map(|grade| f64::from(*grade)).collect();
        ideal.sort_by(|left, right| right.total_cmp(left));
        ideal.truncate(case.k);
        let ideal_dcg = dcg(&ideal);
        if ideal_dcg == 0.0 { 0.0 } else { dcg(&gains) / ideal_dcg }
    });

    let exact_hits = case
        .must_include
        .iter()
        .filter(|expected| ids.contains(expected))
        .count();
    RetrievalMetrics {
        hit_at_k: bool_score(relevant_hits > 0),
        recall_at_k: coverage_ratio(relevant_hits, relevant_total),
        precision_at_k: rate_ratio(relevant_hits, case.k),
        mean_reciprocal_rank: first_relevant.map_or(0.0, |rank| 1.0 / (rank + 1) as f64),
        ndcg_at_k,
        exact_id_hit_rate: coverage_ratio(exact_hits, case.must_include.len()),
    }
}

pub(crate) fn bundle_metrics(
    case: &EvalCase,
    included_ids: &[String],
    item_texts: &[String],
    observed_provenance: &BTreeMap<String, Provenance>,
    estimated_tokens: usize,
) -> BundleMetrics {
    let included: BTreeSet<&str> = included_ids.iter().map(String::as_str).collect();
    let required_hits = case
        .must_include
        .iter()
        .filter(|id| included.contains(id.as_str()))
        .count();
    let forbidden_hits = case
        .must_exclude
        .iter()
        .filter(|id| included.contains(id.as_str()))
        .count();
    let provenance_hits = case
        .expected_provenance
        .iter()
        .filter(|(id, expected)| observed_provenance.get(id.as_str()) == Some(expected))
        .count();
    let stale_ids: BTreeSet<&str> = case
        .memories
        .iter()
        .filter(|memory| memory.stale)
        .map(|memory| memory.id.as_str())
        .collect();
    let stale_hits = included.iter().filter(|id| stale_ids.contains(**id)).count();
    let filtered_ids: BTreeSet<&str> = case
        .memories
        .iter()
        .filter(|memory| source_is_filtered(memory.source, case.options.source_policy))
        .map(|memory| memory.id.as_str())
        .collect();
    let filtered_hits = included.iter().filter(|id| filtered_ids.contains(**id)).count();
    let procedures: BTreeSet<&str> = case
        .memories
        .iter()
        .filter(|memory| memory.procedure_required)
        .map(|memory| memory.id.as_str())
        .collect();
    let procedure_hits = included.iter().filter(|id| procedures.contains(**id)).count();

    let mut conflict_counts = BTreeMap::<&str, usize>::new();
    for memory in &case.memories {
        if included.contains(memory.id.as_str())
            && let Some(group) = memory.conflict_group.as_deref()
        {
            *conflict_counts.entry(group).or_default() += 1;
        }
    }

    BundleMetrics {
        must_include_coverage: coverage_ratio(required_hits, case.must_include.len()),
        forbidden_inclusion_rate: rate_ratio(forbidden_hits, case.must_exclude.len()),
        budget_compliant: estimated_tokens <= case.token_budget,
        duplicate_token_ratio: duplicate_token_ratio(item_texts),
        provenance_coverage: coverage_ratio(provenance_hits, case.expected_provenance.len()),
        stale_fact_rate: rate_ratio(stale_hits, included.len()),
        source_filtered_leakage_rate: rate_ratio(filtered_hits, filtered_ids.len()),
        procedure_coverage: coverage_ratio(procedure_hits, procedures.len()),
        unreported_conflicts: conflict_counts.values().filter(|count| **count > 1).count(),
    }
}

fn source_is_filtered(source: Provenance, policy: SourcePolicy) -> bool {
    match policy {
        SourcePolicy::AllowAll => false,
        SourcePolicy::ExcludeImported => source == Provenance::Import,
        SourcePolicy::UserAndConsolidationOnly => !matches!(source, Provenance::User | Provenance::Consolidation),
    }
}

fn dcg(gains: &[f64]) -> f64 {
    gains
        .iter()
        .enumerate()
        .map(|(index, grade)| (2.0_f64.powf(*grade) - 1.0) / ((index + 2) as f64).log2())
        .sum()
}

fn duplicate_token_ratio(texts: &[String]) -> f64 {
    let mut seen = BTreeSet::new();
    let mut total = 0usize;
    let mut duplicates = 0usize;
    for token in texts
        .iter()
        .flat_map(|text| text.split(|character: char| !character.is_alphanumeric()))
        .filter(|token| !token.is_empty())
        .map(str::to_lowercase)
    {
        total += 1;
        if !seen.insert(token) {
            duplicates += 1;
        }
    }
    rate_ratio(duplicates, total)
}

fn coverage_ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        1.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn rate_ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

const fn bool_score(value: bool) -> f64 {
    if value { 1.0 } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_ratio_counts_repeated_normalized_tokens() {
        assert_eq!(
            duplicate_token_ratio(&["Alpha beta".to_string(), "alpha gamma".to_string()]),
            0.25
        );
    }

    #[test]
    fn empty_coverage_is_complete_but_empty_incident_rate_is_zero() {
        assert_eq!(coverage_ratio(0, 0), 1.0);
        assert_eq!(rate_ratio(0, 0), 0.0);
    }
}
