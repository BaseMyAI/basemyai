// SPDX-License-Identifier: BUSL-1.1
//! # basemyai (bindings Python)
//!
//! Liaison PyO3 du moteur de mémoire [`basemyai`]. API asynchrone (asyncio) :
//! chaque opération rend un awaitable piloté par un runtime tokio interne.
//!
//! Le module natif est `basemyai._internal` ; le package pur-Python `basemyai`
//! le ré-exporte (voir `python/basemyai/__init__.py`).

mod errors;
mod memory;
mod types;

use pyo3::prelude::*;

use errors::{BasemyaiError, EncryptionError, InferenceError, StorageError, ValidationError};
use memory::{Memory, MemoryWatch};
use types::{AgentStats, Entity, Record, WatchEvent};

/// Point d'entrée du module natif (`PyInit__internal`).
#[pymodule]
fn _internal(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Memory>()?;
    m.add_class::<Record>()?;
    m.add_class::<AgentStats>()?;
    m.add_class::<Entity>()?;
    m.add_class::<WatchEvent>()?;
    m.add_class::<MemoryWatch>()?;

    m.add("BasemyaiError", m.py().get_type::<BasemyaiError>())?;
    m.add("ValidationError", m.py().get_type::<ValidationError>())?;
    m.add("StorageError", m.py().get_type::<StorageError>())?;
    m.add("EncryptionError", m.py().get_type::<EncryptionError>())?;
    m.add("InferenceError", m.py().get_type::<InferenceError>())?;

    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
