//! Deterministic, offline evaluation for BaseMyAI recall and context bundles.

mod adapter;
mod compare;
mod error;
mod metrics;
mod report;
mod runner;
mod schema;

pub use compare::{ComparisonReport, compare_reports, write_comparison_human, write_comparison_json};
pub use error::{EvalError, Result};
pub use report::{EvalReport, write_human_report, write_json_report};
pub use runner::{RunOptions, run_dataset};
pub use schema::{EvalCase, load_dataset};
