//! Façade `Memory` exposée à Python. Chaque méthode rend un **awaitable**
//! (coroutine asyncio) : le futur tokio Rust est piloté par l'event loop Python
//! via `pyo3_async_runtimes`. Le moteur reste 100 % local, en process.

#[cfg(all(feature = "crypto", feature = "embed"))]
use std::path::PathBuf;
use std::sync::Arc;

use pyo3::prelude::*;
use pyo3_async_runtimes::tokio::future_into_py;

#[cfg(all(feature = "crypto", feature = "embed"))]
use basemyai::AgentId;
use basemyai::MemoryLayer;

#[cfg(all(feature = "crypto", feature = "embed"))]
use basemyai_core::{CandleEmbedder, Device, Embedder, EncryptionKey, Store};

#[cfg(all(feature = "crypto", feature = "embed"))]
use crate::errors::ValidationError;
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
    /// Ouvre une mémoire persistante chiffrée avec l'embedder Candle local.
    ///
    /// Si `model_dir` est absent, le provisioning ne télécharge le modèle que si
    /// `consent_to_fetch=True`. Par défaut, aucun accès réseau n'est déclenché.
    #[cfg(all(feature = "crypto", feature = "embed"))]
    #[staticmethod]
    #[pyo3(signature = (path, agent_id, encryption_key, *, model_dir = None, device = "auto".to_string(), consent_to_fetch = false))]
    fn open(
        py: Python<'_>,
        path: String,
        agent_id: String,
        encryption_key: String,
        model_dir: Option<String>,
        device: String,
        consent_to_fetch: bool,
    ) -> PyResult<Bound<'_, PyAny>> {
        future_into_py(py, async move {
            let agent =
                AgentId::new(agent_id).ok_or_else(|| ValidationError::new_err("a valid agent_id is required"))?;
            let parsed_device = parse_device(&device)?;

            let (model_path, resolved_device) = if let Some(model_dir) = model_dir {
                let resolved = parsed_device.unwrap_or_else(|| basemyai::detect_hardware().device);
                (PathBuf::from(model_dir), resolved)
            } else {
                let provision = basemyai::provision(consent_to_fetch).await.map_err(to_pyerr)?;
                (provision.model_path, parsed_device.unwrap_or(provision.device))
            };

            let embedder: Box<dyn Embedder> = Box::new(
                CandleEmbedder::load(&model_path, resolved_device)
                    .map_err(basemyai::MemoryError::from)
                    .map_err(to_pyerr)?,
            );
            let store = Store::open(&PathBuf::from(path), Some(EncryptionKey::new(encryption_key)))
                .await
                .map_err(basemyai::MemoryError::from)
                .map_err(to_pyerr)?;
            let mem = basemyai::Memory::open(store, embedder, agent).await.map_err(to_pyerr)?;
            Ok(Self { inner: Arc::new(mem) })
        })
    }

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

    /// Recall temporel sémantique filtré sur une couche unique.
    #[pyo3(signature = (query, layer, k = 5))]
    fn recall_by_layer<'p>(
        &self,
        py: Python<'p>,
        query: String,
        layer: String,
        k: usize,
    ) -> PyResult<Bound<'p, PyAny>> {
        let inner = Arc::clone(&self.inner);
        future_into_py(py, async move {
            let layer = MemoryLayer::from_table(&layer).map_err(to_pyerr)?;
            let records = inner.recall_by_layer(&query, layer, k).await.map_err(to_pyerr)?;
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

    /// Ajoute ou met à jour une entité dans le graphe de cette mémoire.
    fn add_graph_entity<'p>(
        &self,
        py: Python<'p>,
        id: String,
        kind: String,
        label: String,
    ) -> PyResult<Bound<'p, PyAny>> {
        let inner = Arc::clone(&self.inner);
        future_into_py(py, async move {
            inner.graph().add_entity(&id, &kind, &label).await.map_err(to_pyerr)?;
            Ok(())
        })
    }

    /// Ajoute ou met à jour une relation orientée `src -> dst`.
    #[pyo3(signature = (src, relation, dst, weight = 1.0))]
    fn add_graph_edge<'p>(
        &self,
        py: Python<'p>,
        src: String,
        relation: String,
        dst: String,
        weight: f64,
    ) -> PyResult<Bound<'p, PyAny>> {
        let inner = Arc::clone(&self.inner);
        future_into_py(py, async move {
            inner
                .graph()
                .add_edge(&src, &relation, &dst, weight)
                .await
                .map_err(to_pyerr)?;
            Ok(())
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

#[cfg(all(feature = "crypto", feature = "embed"))]
fn parse_device(value: &str) -> PyResult<Option<Device>> {
    match value {
        "auto" => Ok(None),
        "cpu" => Ok(Some(Device::Cpu)),
        "metal" => Ok(Some(Device::Metal)),
        "cuda" => Ok(Some(Device::Cuda(0))),
        s if s.starts_with("cuda:") => {
            let index = s
                .strip_prefix("cuda:")
                .and_then(|raw| raw.parse::<usize>().ok())
                .ok_or_else(|| {
                    ValidationError::new_err("device must be one of: auto, cpu, metal, cuda, cuda:<index>")
                })?;
            Ok(Some(Device::Cuda(index)))
        }
        _ => Err(ValidationError::new_err(
            "device must be one of: auto, cpu, metal, cuda, cuda:<index>",
        )),
    }
}
