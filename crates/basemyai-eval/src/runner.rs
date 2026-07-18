use std::collections::BTreeMap;
use std::path::Path;

use crate::Result;
use crate::adapter::execute_case;
use crate::report::{
    AggregateBundleMetrics, AggregateReport, AggregateRetrievalMetrics, AssertionReport, EvalReport, REPORT_VERSION,
};
use crate::schema::{EvalCase, RetrievalMode, load_dataset};

#[derive(Debug, Clone, Copy, Default)]
pub struct RunOptions {
    pub record_timings: bool,
}

pub async fn run_dataset(path: &Path, options: RunOptions) -> Result<EvalReport> {
    let cases = load_dataset(path)?;
    let bytes = std::fs::read(path).map_err(|source| crate::EvalError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    run_cases(
        path.file_name().and_then(|name| name.to_str()).unwrap_or("<dataset>"),
        &fingerprint(&bytes),
        &cases,
        options,
    )
    .await
}

async fn run_cases(
    dataset: &str,
    dataset_fingerprint: &str,
    cases: &[EvalCase],
    options: RunOptions,
) -> Result<EvalReport> {
    let mut reports = Vec::with_capacity(cases.len());
    for case in cases {
        let mut report = execute_case(case, options.record_timings).await?;
        if case.assert_deterministic {
            let second = execute_case(case, false).await?;
            let deterministic = deterministic_snapshot(&report) == deterministic_snapshot(&second);
            report.assertions.push(AssertionReport {
                name: "determinism".to_string(),
                passed: deterministic,
                details: "normalized outputs and quality metrics must match a second isolated run".to_string(),
            });
            report.assertions.sort_by(|left, right| left.name.cmp(&right.name));
            report.passed = report.assertions.iter().all(|assertion| assertion.passed);
        }
        reports.push(report);
    }

    let aggregate = aggregate(&reports);
    Ok(EvalReport {
        report_version: REPORT_VERSION,
        dataset: dataset.to_string(),
        dataset_fingerprint: dataset_fingerprint.to_string(),
        deterministic_timings: !options.record_timings,
        cases: reports,
        aggregate,
    })
}

fn fingerprint(bytes: &[u8]) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("fnv1a64:{hash:016x}")
}

fn deterministic_snapshot(report: &crate::report::CaseReport) -> crate::report::CaseReport {
    let mut snapshot = report.clone();
    for retrieval in snapshot.retrieval.values_mut() {
        retrieval.latency_micros = None;
    }
    snapshot.bundle.latency_micros = None;
    snapshot
}

fn aggregate(cases: &[crate::report::CaseReport]) -> AggregateReport {
    let passed_cases = cases.iter().filter(|case| case.passed).count();
    let mut retrieval = BTreeMap::new();
    for mode in [RetrievalMode::Vector, RetrievalMode::Hybrid, RetrievalMode::Graph] {
        let metrics: Vec<_> = cases
            .iter()
            .filter_map(|case| case.retrieval.get(&mode).map(|report| &report.metrics))
            .collect();
        if !metrics.is_empty() {
            let ndcg_values: Vec<f64> = metrics.iter().filter_map(|metric| metric.ndcg_at_k).collect();
            retrieval.insert(
                mode,
                AggregateRetrievalMetrics {
                    cases: metrics.len(),
                    hit_at_k: average(metrics.iter().map(|metric| metric.hit_at_k)),
                    recall_at_k: average(metrics.iter().map(|metric| metric.recall_at_k)),
                    precision_at_k: average(metrics.iter().map(|metric| metric.precision_at_k)),
                    mean_reciprocal_rank: average(metrics.iter().map(|metric| metric.mean_reciprocal_rank)),
                    ndcg_at_k: (!ndcg_values.is_empty()).then(|| average(ndcg_values)),
                    exact_id_hit_rate: average(metrics.iter().map(|metric| metric.exact_id_hit_rate)),
                },
            );
        }
    }

    AggregateReport {
        total_cases: cases.len(),
        passed_cases,
        failed_cases: cases.len().saturating_sub(passed_cases),
        retrieval,
        bundle: AggregateBundleMetrics {
            must_include_coverage: average(cases.iter().map(|case| case.bundle.metrics.must_include_coverage)),
            forbidden_inclusion_rate: average(cases.iter().map(|case| case.bundle.metrics.forbidden_inclusion_rate)),
            budget_compliance_rate: average(cases.iter().map(|case| f64::from(case.bundle.metrics.budget_compliant))),
            duplicate_token_ratio: average(cases.iter().map(|case| case.bundle.metrics.duplicate_token_ratio)),
            provenance_coverage: average(cases.iter().map(|case| case.bundle.metrics.provenance_coverage)),
            stale_fact_rate: average(cases.iter().map(|case| case.bundle.metrics.stale_fact_rate)),
            source_filtered_leakage_rate: average(
                cases
                    .iter()
                    .map(|case| case.bundle.metrics.source_filtered_leakage_rate),
            ),
            procedure_coverage: average(cases.iter().map(|case| case.bundle.metrics.procedure_coverage)),
            unreported_conflicts: cases.iter().map(|case| case.bundle.metrics.unreported_conflicts).sum(),
        },
    }
}

fn average(values: impl IntoIterator<Item = f64>) -> f64 {
    let values: Vec<f64> = values.into_iter().collect();
    if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::parse_dataset;

    #[tokio::test]
    async fn minimal_case_runs_offline_and_is_deterministic() {
        let input = r#"{"schema_version":1,"id":"smoke","suite":"direct","description":"smoke","seed":7,"query":"SMOKE-42","k":2,"token_budget":128,"options":{"source_policy":"allow_all"},"memories":[{"id":"target","text":"SMOKE-42 release checklist","layer":"semantic","relevance":3}],"must_include":["target"],"expected_provenance":{"target":"user"},"retrieval":{"hybrid":{"must_include":["target"]}},"assert_deterministic":true}"#;
        let cases = parse_dataset(input).expect("valid inline dataset");
        let report = run_cases(
            "inline.jsonl",
            &fingerprint(input.as_bytes()),
            &cases,
            RunOptions::default(),
        )
        .await
        .expect("runner succeeds");
        assert!(report.passed());
        assert_eq!(report.aggregate.total_cases, 1);
        assert_eq!(report.cases[0].bundle.included_ids, ["target"]);
    }
}
