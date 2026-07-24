// SPDX-License-Identifier: BUSL-1.1
//! Recall Quality Lab (`basemyai-eval`) : adapte le runner/comparateur
//! existant à la sortie text/JSON et au contrat d'exit code de la CLI, sans
//! réimplémenter la politique du runner (`docs/recall-quality-lab.md`
//! §"Required integrations"). Feature `eval-lab` uniquement : le crate tire
//! `basemyai/test-util` (HashEmbedder, store éphémère de test), qu'on ne veut
//! pas dans un binaire distribué par défaut.

use std::path::{Path, PathBuf};

use basemyai_eval::{
    RunOptions, compare_reports, run_dataset, write_comparison_human, write_comparison_json, write_human_report,
    write_json_report,
};

use crate::error::CliError;
use crate::output::Format;

pub(crate) async fn run(
    dataset: &Path,
    output: &Path,
    human: Option<PathBuf>,
    timings: bool,
    format: Format,
) -> Result<(), CliError> {
    let report = run_dataset(
        dataset,
        RunOptions {
            record_timings: timings,
        },
    )
    .await?;
    write_json_report(output, &report)?;
    if let Some(path) = &human {
        write_human_report(path, &report)?;
    }
    let passed = report.passed();
    format.print(
        || {
            crate::ui::render::section("Recall Quality Lab — run");
            crate::ui::table::print_table(
                &["Metric", "Value"],
                vec![
                    vec!["total_cases".to_string(), report.aggregate.total_cases.to_string()],
                    vec!["passed_cases".to_string(), report.aggregate.passed_cases.to_string()],
                    vec!["failed_cases".to_string(), report.aggregate.failed_cases.to_string()],
                    vec!["report".to_string(), output.display().to_string()],
                ],
            );
        },
        || {
            serde_json::json!({
                "total_cases": report.aggregate.total_cases,
                "passed_cases": report.aggregate.passed_cases,
                "failed_cases": report.aggregate.failed_cases,
                "report": output.display().to_string(),
                "human_report": human.as_ref().map(|p| p.display().to_string()),
            })
        },
    );
    if passed { Ok(()) } else { Err(CliError::EvalCasesFailed) }
}

pub(crate) async fn compare(
    baseline: &Path,
    current: &Path,
    output: Option<PathBuf>,
    human: Option<PathBuf>,
    fail_on_regression: bool,
    format: Format,
) -> Result<(), CliError> {
    let comparison = compare_reports(baseline, current)?;
    if let Some(path) = &output {
        write_comparison_json(path, &comparison)?;
    }
    if let Some(path) = &human {
        write_comparison_human(path, &comparison)?;
    }
    let passed = comparison.passed();
    format.print(
        || {
            crate::ui::render::section("Recall Quality Lab — compare");
            crate::ui::table::print_table(
                &["Metric", "Value"],
                vec![
                    vec!["regressions".to_string(), comparison.regressions.to_string()],
                    vec![
                        "baseline_failed_cases".to_string(),
                        comparison.baseline_failed_cases.to_string(),
                    ],
                    vec![
                        "current_failed_cases".to_string(),
                        comparison.current_failed_cases.to_string(),
                    ],
                ],
            );
        },
        || {
            serde_json::json!({
                "regressions": comparison.regressions,
                "baseline_failed_cases": comparison.baseline_failed_cases,
                "current_failed_cases": comparison.current_failed_cases,
            })
        },
    );
    if fail_on_regression && !passed {
        Err(CliError::EvalRegressionDetected)
    } else {
        Ok(())
    }
}
