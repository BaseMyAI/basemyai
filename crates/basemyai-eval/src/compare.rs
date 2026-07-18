use std::fmt::Write as _;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::report::{EvalReport, mode_name};
use crate::schema::RetrievalMode;
use crate::{EvalError, Result};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ComparisonReport {
    pub comparison_version: u32,
    pub dataset: String,
    pub dataset_fingerprint: String,
    pub baseline_failed_cases: usize,
    pub current_failed_cases: usize,
    pub metrics: Vec<MetricDelta>,
    pub regressions: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MetricDelta {
    pub name: String,
    pub baseline: f64,
    pub current: f64,
    pub delta: f64,
    pub better: Direction,
    pub regression: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Higher,
    Lower,
}

impl ComparisonReport {
    #[must_use]
    pub fn passed(&self) -> bool {
        self.regressions == 0 && self.current_failed_cases <= self.baseline_failed_cases
    }

    #[must_use]
    pub fn to_human(&self) -> String {
        let mut output = String::new();
        let _ = writeln!(output, "# Recall Quality Comparison");
        let _ = writeln!(output);
        let _ = writeln!(output, "- Dataset: `{}`", self.dataset);
        let _ = writeln!(output, "- Dataset fingerprint: `{}`", self.dataset_fingerprint);
        let _ = writeln!(
            output,
            "- Failed cases: {} -> {}",
            self.baseline_failed_cases, self.current_failed_cases
        );
        let _ = writeln!(output, "- Regressions: {}", self.regressions);
        let _ = writeln!(output);
        let _ = writeln!(output, "| Metric | Baseline | Current | Delta | Direction | Result |");
        let _ = writeln!(output, "|---|---:|---:|---:|---|---:|");
        for metric in &self.metrics {
            let _ = writeln!(
                output,
                "| `{}` | {:.6} | {:.6} | {:+.6} | {:?} | {} |",
                metric.name,
                metric.baseline,
                metric.current,
                metric.delta,
                metric.better,
                if metric.regression { "REGRESSION" } else { "ok" },
            );
        }
        output
    }
}

pub fn compare_reports(baseline_path: &Path, current_path: &Path) -> Result<ComparisonReport> {
    let baseline = read_report(baseline_path)?;
    let current = read_report(current_path)?;
    if baseline.report_version != current.report_version {
        return Err(EvalError::Schema {
            case_id: "<comparison>".to_string(),
            message: "report versions differ".to_string(),
        });
    }
    if baseline.dataset != current.dataset || baseline.dataset_fingerprint != current.dataset_fingerprint {
        return Err(EvalError::Schema {
            case_id: "<comparison>".to_string(),
            message: "reports come from different dataset names or contents".to_string(),
        });
    }

    let mut metrics = Vec::new();
    for mode in [RetrievalMode::Vector, RetrievalMode::Hybrid, RetrievalMode::Graph] {
        let Some(base) = baseline.aggregate.retrieval.get(&mode) else {
            continue;
        };
        let Some(now) = current.aggregate.retrieval.get(&mode) else {
            continue;
        };
        let prefix = format!("retrieval.{}", mode_name(mode));
        push(
            &mut metrics,
            format!("{prefix}.hit_at_k"),
            base.hit_at_k,
            now.hit_at_k,
            Direction::Higher,
        );
        push(
            &mut metrics,
            format!("{prefix}.recall_at_k"),
            base.recall_at_k,
            now.recall_at_k,
            Direction::Higher,
        );
        push(
            &mut metrics,
            format!("{prefix}.precision_at_k"),
            base.precision_at_k,
            now.precision_at_k,
            Direction::Higher,
        );
        push(
            &mut metrics,
            format!("{prefix}.mrr"),
            base.mean_reciprocal_rank,
            now.mean_reciprocal_rank,
            Direction::Higher,
        );
        if let (Some(base_ndcg), Some(now_ndcg)) = (base.ndcg_at_k, now.ndcg_at_k) {
            push(
                &mut metrics,
                format!("{prefix}.ndcg_at_k"),
                base_ndcg,
                now_ndcg,
                Direction::Higher,
            );
        }
        push(
            &mut metrics,
            format!("{prefix}.exact_id_hit_rate"),
            base.exact_id_hit_rate,
            now.exact_id_hit_rate,
            Direction::Higher,
        );
    }

    let base = &baseline.aggregate.bundle;
    let now = &current.aggregate.bundle;
    push(
        &mut metrics,
        "bundle.must_include_coverage".to_string(),
        base.must_include_coverage,
        now.must_include_coverage,
        Direction::Higher,
    );
    push(
        &mut metrics,
        "bundle.forbidden_inclusion_rate".to_string(),
        base.forbidden_inclusion_rate,
        now.forbidden_inclusion_rate,
        Direction::Lower,
    );
    push(
        &mut metrics,
        "bundle.budget_compliance_rate".to_string(),
        base.budget_compliance_rate,
        now.budget_compliance_rate,
        Direction::Higher,
    );
    push(
        &mut metrics,
        "bundle.duplicate_token_ratio".to_string(),
        base.duplicate_token_ratio,
        now.duplicate_token_ratio,
        Direction::Lower,
    );
    push(
        &mut metrics,
        "bundle.provenance_coverage".to_string(),
        base.provenance_coverage,
        now.provenance_coverage,
        Direction::Higher,
    );
    push(
        &mut metrics,
        "bundle.stale_fact_rate".to_string(),
        base.stale_fact_rate,
        now.stale_fact_rate,
        Direction::Lower,
    );
    push(
        &mut metrics,
        "bundle.source_filtered_leakage_rate".to_string(),
        base.source_filtered_leakage_rate,
        now.source_filtered_leakage_rate,
        Direction::Lower,
    );
    push(
        &mut metrics,
        "bundle.procedure_coverage".to_string(),
        base.procedure_coverage,
        now.procedure_coverage,
        Direction::Higher,
    );
    push(
        &mut metrics,
        "bundle.unreported_conflicts".to_string(),
        base.unreported_conflicts as f64,
        now.unreported_conflicts as f64,
        Direction::Lower,
    );

    let regressions = metrics.iter().filter(|metric| metric.regression).count();
    Ok(ComparisonReport {
        comparison_version: 1,
        dataset: baseline.dataset,
        dataset_fingerprint: baseline.dataset_fingerprint,
        baseline_failed_cases: baseline.aggregate.failed_cases,
        current_failed_cases: current.aggregate.failed_cases,
        metrics,
        regressions,
    })
}

pub fn write_comparison_json(path: &Path, comparison: &ComparisonReport) -> Result<()> {
    let mut json = serde_json::to_string_pretty(comparison).map_err(|source| EvalError::ReportJson {
        path: path.to_path_buf(),
        source,
    })?;
    json.push('\n');
    std::fs::write(path, json).map_err(|source| EvalError::Write {
        path: path.to_path_buf(),
        source,
    })
}

pub fn write_comparison_human(path: &Path, comparison: &ComparisonReport) -> Result<()> {
    std::fs::write(path, comparison.to_human()).map_err(|source| EvalError::Write {
        path: path.to_path_buf(),
        source,
    })
}

fn read_report(path: &Path) -> Result<EvalReport> {
    let content = std::fs::read_to_string(path).map_err(|source| EvalError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_str(&content).map_err(|source| EvalError::ReportJson {
        path: path.to_path_buf(),
        source,
    })
}

fn push(metrics: &mut Vec<MetricDelta>, name: String, baseline: f64, current: f64, better: Direction) {
    let delta = current - baseline;
    let regression = match better {
        Direction::Higher => delta < -f64::EPSILON,
        Direction::Lower => delta > f64::EPSILON,
    };
    metrics.push(MetricDelta {
        name,
        baseline,
        current,
        delta,
        better,
        regression,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direction_marks_only_adverse_changes() {
        let mut metrics = Vec::new();
        push(&mut metrics, "coverage".to_string(), 1.0, 0.5, Direction::Higher);
        push(&mut metrics, "leakage".to_string(), 0.0, 0.1, Direction::Lower);
        push(&mut metrics, "improved".to_string(), 0.5, 1.0, Direction::Higher);
        assert!(metrics[0].regression);
        assert!(metrics[1].regression);
        assert!(!metrics[2].regression);
    }
}
