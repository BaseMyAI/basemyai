// SPDX-License-Identifier: BUSL-1.1
//! Exceptions Python et mapping depuis [`basemyai::MemoryError`].
//!
//! Hiérarchie : `BasemyaiError(Exception)` à la racine ; `ValidationError`
//! dérive de `ValueError` (erreurs d'entrée). Le reste dérive de `BasemyaiError`.

use pyo3::PyErr;
use pyo3::create_exception;
use pyo3::exceptions::{PyException, PyValueError};

create_exception!(
    basemyai,
    BasemyaiError,
    PyException,
    "Base class for all basemyai errors."
);
create_exception!(
    basemyai,
    ValidationError,
    PyValueError,
    "Invalid input (agent_id, layer, ...)."
);
create_exception!(basemyai, StorageError, BasemyaiError, "Storage / embedding failure.");
create_exception!(
    basemyai,
    EncryptionError,
    BasemyaiError,
    "Encryption is required or misconfigured."
);
create_exception!(
    basemyai,
    InferenceError,
    BasemyaiError,
    "LLM inference / extraction failure."
);

/// Convertit une [`basemyai::MemoryError`] en exception Python typée.
pub(crate) fn to_pyerr(e: basemyai::MemoryError) -> PyErr {
    use basemyai::MemoryError as E;
    use basemyai_core::CoreError;

    let msg = e.to_string();
    match e {
        E::MissingAgent | E::UnknownLayer(_) => ValidationError::new_err(msg),
        E::EncryptionRequired => EncryptionError::new_err(msg),
        E::Inference(_) | E::Extraction(_) => InferenceError::new_err(msg),
        E::Core(CoreError::Encryption) => EncryptionError::new_err(msg),
        E::Core(_) => StorageError::new_err(msg),
        // `MemoryError` est `#[non_exhaustive]`.
        _ => BasemyaiError::new_err(msg),
    }
}
