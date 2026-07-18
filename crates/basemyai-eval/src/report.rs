use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::schema::RetrievalMode;
use crate::{EvalError, Result};

pub const REPORT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EvalReport {
    pub report_version: u32,
    pub dataset: String,
    pub dataset_fingerprint: String,
    pub deterministic_timings: bool,
    pub cases: Vec<CaseReport>,
    pub aggregate: AggregateReport,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CaseReport {
    pub id: String,
    pub suite: String,
    pub description: String,
    pub seed: u64,
    pub query: String,
    pub retrieval: BTreeMap<RetrievalMode, RetrievalReport>,
    pub bundle: BundleReport,
    pub assertions: Vec<AssertionReport>,
    pub passed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RetrievalReport {
    pub ids: Vec<String>,
    pub metrics: RetrievalMetrics,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_micros: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RetrievalMetrics {
    pub hit_at_k: f64,
    pub recall_at_k: f64,
    pub precision_at_k: f64,
    pub mean_reciprocal_rank: f64,
    pub ndcg_at_k: Option<f64>,
    pub exact_id_hit_rate: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BundleReport {
    pub items: Vec<BundleItemReport>,
    pub included_ids: Vec<String>,
    pub excluded: Vec<ExcludedReport>,
    pub merged: Vec<MergedReport>,
    pub estimated_tokens: usize,
    pub token_budget: usize,
    pub metrics: BundleMetrics,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_micros: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BundleItemReport {
    pub section: String,
    pub text: String,
    pub source_ids: Vec<String>,
    pub layer: String,
    pub provenance: String,
    pub estimated_tokens: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExcludedReport {
    pub id: String,
    pub reason: String,
    pub temporal_status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MergedReport {
    pub id: String,
    pub representative_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BundleMetrics {
    pub must_include_coverage: f64,
    pub forbidden_inclusion_rate: f64,
    pub budget_compliant: bool,
    pub duplicate_token_ratio: f64,
    pub provenance_coverage: f64,
    pub stale_fact_rate: f64,
    pub source_filtered_leakage_rate: f64,
    pub procedure_coverage: f64,
    pub unreported_conflicts: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssertionReport {
    pub name: String,
    pub passed: bool,
    pub details: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AggregateReport {
    pub total_cases: usize,
    pub passed_cases: usize,
    pub failed_cases: usize,
    pub retrieval: BTreeMap<RetrievalMode, AggregateRetrievalMetrics>,
    pub bundle: AggregateBundleMetrics,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AggregateRetrievalMetrics {
    pub cases: usize,
    pub hit_at_k: f64,
    pub recall_at_k: f64,
    pub precision_at_k: f64,
    pub mean_reciprocal_rank: f64,
    pub ndcg_at_k: Option<f64>,
    pub exact_id_hit_rate: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AggregateBundleMetrics {
    pub must_include_coverage: f64,
    pub forbidden_inclusion_rate: f64,
    pub budget_compliance_rate: f64,
    pub duplicate_token_ratio: f64,
    pub provenance_coverage: f64,
    pub stale_fact_rate: f64,
    pub source_filtered_leakage_rate: f64,
    pub procedure_coverage: f64,
    pub unreported_conflicts: usize,
}

impl EvalReport {
    #[must_use]
    pub fn passed(&self) -> bool {
        self.aggregate.failed_cases == 0
    }

    #[must_use]
    pub fn to_human(&self) -> String {
        let mut output = String::new();
        let _ = writeln!(output, "# Recall Quality Lab");
        let _ = writeln!(output);
        let _ = writeln!(output, "- Dataset: `{}`", self.dataset);
        let _ = writeln!(output, "- Dataset fingerprint: `{}`", self.dataset_fingerprint);
        let _ = writeln!(
            output,
            "- Result: {}/{} cases passed",
            self.aggregate.passed_cases, self.aggregate.total_cases
        );
        let _ = writeln!(
            output,
            "- Bundle budget compliance: {:.3}",
            self.aggregate.bundle.budget_compliance_rate
        );
        let _ = writeln!(output);
        let _ = writeln!(
            output,
            "| Case | Suite | Result | Must include | Forbidden | Budget | Provenance |"
        );
        let _ = writeln!(output, "|---|---|---:|---:|---:|---:|---:|");
        for case in &self.cases {
            let metrics = &case.bundle.metrics;
            let _ = writeln!(
                output,
                "| `{}` | {} | {} | {:.3} | {:.3} | {} | {:.3} |",
                case.id,
                case.suite,
                if case.passed { "PASS" } else { "FAIL" },
                metrics.must_include_coverage,
                metrics.forbidden_inclusion_rate,
                if metrics.budget_compliant { "yes" } else { "no" },
                metrics.provenance_coverage,
            );
        }

        let failures: Vec<_> = self
            .cases
            .iter()
            .flat_map(|case| {
                case.assertions
                    .iter()
                    .filter(|assertion| !assertion.passed)
                    .map(move |assertion| (case.id.as_str(), assertion))
            })
            .collect();
        if !failures.is_empty() {
            let _ = writeln!(output);
            let _ = writeln!(output, "## Failures");
            for (case_id, assertion) in failures {
                let _ = writeln!(output, "- `{case_id}` / `{}`: {}", assertion.name, assertion.details);
            }
        }

        let _ = writeln!(output);
        let _ = writeln!(output, "## Retrieval");
        let _ = writeln!(output);
        let _ = writeln!(
            output,
            "| Mode | Cases | Hit@K | Recall@K | Precision@K | MRR | nDCG@K | Exact ID |"
        );
        let _ = writeln!(output, "|---|---:|---:|---:|---:|---:|---:|---:|");
        for (mode, metrics) in &self.aggregate.retrieval {
            let _ = writeln!(
                output,
                "| {} | {} | {:.3} | {:.3} | {:.3} | {:.3} | {} | {:.3} |",
                mode_name(*mode),
                metrics.cases,
                metrics.hit_at_k,
                metrics.recall_at_k,
                metrics.precision_at_k,
                metrics.mean_reciprocal_rank,
                metrics
                    .ndcg_at_k
                    .map_or_else(|| "n/a".to_string(), |value| format!("{value:.3}")),
                metrics.exact_id_hit_rate,
            );
        }
        output
    }
}

pub fn write_json_report(path: &Path, report: &EvalReport) -> Result<()> {
    let mut json = serde_json::to_string_pretty(report).map_err(|source| EvalError::ReportJson {
        path: path.to_path_buf(),
        source,
    })?;
    json.push('\n');
    std::fs::write(path, json).map_err(|source| EvalError::Write {
        path: path.to_path_buf(),
        source,
    })
}

pub fn write_human_report(path: &Path, report: &EvalReport) -> Result<()> {
    std::fs::write(path, report.to_human()).map_err(|source| EvalError::Write {
        path: path.to_path_buf(),
        source,
    })
}

pub(crate) const fn mode_name(mode: RetrievalMode) -> &'static str {
    match mode {
        RetrievalMode::Vector => "vector",
        RetrievalMode::Hybrid => "hybrid",
        RetrievalMode::Graph => "graph",
    }
}
