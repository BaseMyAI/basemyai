//! Façade `Memory` exposée à Python. Chaque méthode rend un **awaitable**
//! (coroutine asyncio) : le futur tokio Rust est piloté par l'event loop Python
//! via `pyo3_async_runtimes`. Le moteur reste 100 % local, en process.

use std::sync::Arc;

use pyo3::prelude::*;
use pyo3_async_runtimes::tokio::future_into_py;

use basemyai::MemoryLayer;

use crate::errors::to_pyerr;
use crate::types::{AgentStats, Entity, Record};

/// Mémoire d'un agent (tenant). Construite via une fabrique asynchrone, puis
/// interrogée par `remember`/`recall`/... (toutes asynchrones).
#[pyclass]
pub struct Memory {
    inner: Arc<basemyai::Memory>,
}

impl Memory {
    /// Test-only : seul `open_in_memory` (lui aussi `#[cfg(test-util)]`) l'appelle.
    /// La garde évite un warning `dead_code` en build par défaut.
    #[cfg(feature = "test-util")]
    fn wrap(inner: basemyai::Memory) -> Self {
        Self { inner: Arc::new(inner) }
    }
}

#[pymethods]
impl Memory {
    /// Ouvre une mémoire **éphémère, non chiffrée** (`:memory:`) avec un embedder
    /// déterministe sans modèle. Réservé aux tests/spikes (pas de CMake/Candle).
    #[cfg(feature = "test-util")]
    #[staticmethod]
    fn open_in_memory(py: Python<'_>, agent_id: String) -> PyResult<Bound<'_, PyAny>> {
        future_into_py(py, async move {
            let mem = basemyai::Memory::open_in_memory(&agent_id).await.map_err(to_pyerr)?;
            Ok(Memory::wrap(mem))
        })
    }

    /// `agent_id` propriétaire de cette mémoire.
    fn agent(&self) -> String {
        self.inner.agent().as_str().to_string()
    }

    /// Mémorise `text` dans une couche. Rend l'UUID (str) du souvenir créé.
    #[pyo3(signature = (text, layer = "semantic".to_string()))]
    fn remember<'p>(&self, py: Python<'p>, text: String, layer: String) -> PyResult<Bound<'p, PyAny>> {
        let inner = Arc::clone(&self.inner);
        future_into_py(py, async move {
            let layer = MemoryLayer::from_table(&layer).map_err(to_pyerr)?;
            inner.remember(&text, layer).await.map_err(to_pyerr)
        })
    }

    /// Recall temporel sémantique : rend une `list[Record]`.
    #[pyo3(signature = (query, k = 5))]
    fn recall<'p>(&self, py: Python<'p>, query: String, k: usize) -> PyResult<Bound<'p, PyAny>> {
        let inner = Arc::clone(&self.inner);
        future_into_py(py, async move {
            let records = inner.recall(&query, k).await.map_err(to_pyerr)?;
            Ok(records.into_iter().map(Record::from).collect::<Vec<_>>())
        })
    }

    /// Recall hybride : vecteur + BM25 (full-text) fusionnés par RRF. Rend une
    /// `list[Record]` (le `score` porte le score RRF fusionné).
    #[pyo3(signature = (query, k = 5))]
    fn recall_hybrid<'p>(&self, py: Python<'p>, query: String, k: usize) -> PyResult<Bound<'p, PyAny>> {
        let inner = Arc::clone(&self.inner);
        future_into_py(py, async move {
            let records = inner.recall_hybrid(&query, k).await.map_err(to_pyerr)?;
            // Ici `score` est le score RRF fusionné : on le garde tel quel (pas de
            // conversion en similarité comme pour `recall`).
            Ok(records
                .into_iter()
                .map(|r| Record {
                    id: r.id,
                    text: r.text,
                    layer: r.layer.table().to_string(),
                    score: r.score,
                })
                .collect::<Vec<_>>())
        })
    }

    /// Invalide (soft-delete) un souvenir par son id.
    fn invalidate<'p>(&self, py: Python<'p>, id: String) -> PyResult<Bound<'p, PyAny>> {
        let inner = Arc::clone(&self.inner);
        future_into_py(py, async move {
            inner.invalidate(&id).await.map_err(to_pyerr)?;
            Ok(())
        })
    }

    /// Supprime physiquement un souvenir (droit à l'effacement).
    fn forget<'p>(&self, py: Python<'p>, id: String) -> PyResult<Bound<'p, PyAny>> {
        let inner = Arc::clone(&self.inner);
        future_into_py(py, async move {
            inner.forget(&id).await.map_err(to_pyerr)?;
            Ok(())
        })
    }

    /// Compte des souvenirs valides par couche : rend un `AgentStats`.
    fn stats<'p>(&self, py: Python<'p>) -> PyResult<Bound<'p, PyAny>> {
        let inner = Arc::clone(&self.inner);
        future_into_py(py, async move {
            let stats = inner.stats().await.map_err(to_pyerr)?;
            Ok(AgentStats::from(stats))
        })
    }

    /// Traverse le graphe entités/relations depuis `start` : rend `list[Entity]`.
    #[pyo3(signature = (start, max_depth = 2))]
    fn recall_graph<'p>(&self, py: Python<'p>, start: String, max_depth: u32) -> PyResult<Bound<'p, PyAny>> {
        let inner = Arc::clone(&self.inner);
        future_into_py(py, async move {
            let reached = inner.graph().traverse(&start, max_depth).await.map_err(to_pyerr)?;
            Ok(reached.into_iter().map(Entity::from).collect::<Vec<_>>())
        })
    }
}
