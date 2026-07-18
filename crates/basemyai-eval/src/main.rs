use std::path::PathBuf;
use std::process::ExitCode;

use basemyai_eval::{
    RunOptions, compare_reports, run_dataset, write_comparison_human, write_comparison_json, write_human_report,
    write_json_report,
};
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "basemyai-eval", about = "Deterministic offline Recall Quality Lab")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run a versioned JSONL dataset against recall and Context Engine.
    Run {
        dataset: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        #[arg(long)]
        human: Option<PathBuf>,
        /// Record wall-clock latency. Omit for byte-stable reports.
        #[arg(long)]
        timings: bool,
    },
    /// Compare aggregate quality metrics from two JSON reports.
    Compare {
        baseline: PathBuf,
        current: PathBuf,
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[arg(long)]
        human: Option<PathBuf>,
        /// Exit with status 1 when a metric or failed-case count regresses.
        #[arg(long)]
        fail_on_regression: bool,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    match execute(Cli::parse()).await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("basemyai-eval: {error}");
            ExitCode::from(2)
        }
    }
}

async fn execute(cli: Cli) -> basemyai_eval::Result<ExitCode> {
    match cli.command {
        Command::Run {
            dataset,
            output,
            human,
            timings,
        } => {
            let report = run_dataset(
                &dataset,
                RunOptions {
                    record_timings: timings,
                },
            )
            .await?;
            write_json_report(&output, &report)?;
            if let Some(path) = human {
                write_human_report(&path, &report)?;
            }
            println!(
                "{}/{} cases passed; report: {}",
                report.aggregate.passed_cases,
                report.aggregate.total_cases,
                output.display()
            );
            Ok(if report.passed() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            })
        }
        Command::Compare {
            baseline,
            current,
            output,
            human,
            fail_on_regression,
        } => {
            let comparison = compare_reports(&baseline, &current)?;
            if let Some(path) = output {
                write_comparison_json(&path, &comparison)?;
            }
            if let Some(path) = human {
                write_comparison_human(&path, &comparison)?;
            }
            println!(
                "{} regressions; failed cases {} -> {}",
                comparison.regressions, comparison.baseline_failed_cases, comparison.current_failed_cases
            );
            Ok(if fail_on_regression && !comparison.passed() {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            })
        }
    }
}
