use std::path::PathBuf;

/// Failures that prevent an evaluation result from being produced.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EvalError {
    #[error("cannot read {path}: {source}")]
    Read { path: PathBuf, source: std::io::Error },
    #[error("cannot write {path}: {source}")]
    Write { path: PathBuf, source: std::io::Error },
    #[error("invalid JSON on line {line}: {source}")]
    JsonLine { line: usize, source: serde_json::Error },
    #[error("invalid dataset case {case_id:?}: {message}")]
    Schema { case_id: String, message: String },
    #[error("BaseMyAI failed for case {case_id:?}: {source}")]
    BaseMyAi {
        case_id: String,
        source: basemyai::MemoryError,
    },
    #[error("cannot decode report {path}: {source}")]
    ReportJson { path: PathBuf, source: serde_json::Error },
}

pub type Result<T> = std::result::Result<T, EvalError>;
