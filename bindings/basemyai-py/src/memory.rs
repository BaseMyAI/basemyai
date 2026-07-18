// SPDX-License-Identifier: BUSL-1.1
//! Façade `Memory` exposée à Python. Chaque méthode rend un **awaitable**
//! (coroutine asyncio) : le futur tokio Rust est piloté par l'event loop Python
//! via `pyo3_async_runtimes`. Le moteur reste 100 % local, en process.

#[cfg(feature = "embed")]
use std::path::PathBuf;
use std::sync::Arc;

use pyo3::exceptions::PyStopAsyncIteration;
use pyo3::prelude::*;
use pyo3_async_runtimes::tokio::future_into_py;
use tokio::sync::Mutex as AsyncMutex;

#[cfg(feature = "embed")]
use basemyai::AgentId;
use basemyai::{MemoryLayer, MemorySubscription};

#[cfg(feature = "embed")]
use basemyai_core::EncryptionKey;
#[cfg(feature = "embed")]
use basemyai_core::{CandleEmbedder, Device, Embedder};

#[cfg(feature = "embed")]
use crate::errors::EncryptionError;
use crate::errors::ValidationError;
use crate::errors::to_pyerr;
use crate::types::{AgentStats, ContextBundle, Entity, Record, WatchEvent, parse_source_policy};

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
    /// Ouvre une mémoire persistante chiffrée (moteur natif, ADR-032) avec
    /// l'embedder Candle local.
    ///
    /// `path`/`agent_id` sont optionnels : omis, ils retombent sur
    /// `~/.basemyai/config.toml` / `BASEMYAI_DB_PATH` / `BASEMYAI_AGENT`
    /// (même résolution que la CLI, `basemyai config set db-path|agent`),
    /// puis sur un défaut intégré (`./basemyai.bmai`, agent `"default"`) —
    /// jamais d'erreur pour absence de configuration.
    /// Si `encryption_key` est omis, la résolution ADR-034 s'applique
    /// (`BASEMYAI_DB_KEY`, `BASEMYAI_DB_KEY_FILE`, `~/.basemyai/key`, etc.) ;
    /// si aucune source n'existe nulle part, une clé est générée et persistée
    /// automatiquement (avis imprimé sur stderr — sauvegardez ce fichier).
    /// Une credential explicite utilise `credential_mode="raw"` par défaut ;
    /// choisir `"passphrase"` active Argon2id pour cet appel.
    /// Si `model_dir` est absent, le provisioning ne télécharge le modèle que si
    /// `consent_to_fetch=True`. Par défaut, aucun accès réseau n'est déclenché.
    #[cfg(feature = "embed")]
    #[staticmethod]
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (path = None, agent_id = None, *, encryption_key = None, credential_mode = None, model_dir = None, device = "auto".to_string(), consent_to_fetch = false))]
    fn open(
        py: Python<'_>,
        path: Option<String>,
        agent_id: Option<String>,
        encryption_key: Option<String>,
        credential_mode: Option<String>,
        model_dir: Option<String>,
        device: String,
        consent_to_fetch: bool,
    ) -> PyResult<Bound<'_, PyAny>> {
        future_into_py(py, async move {
            let defaults = basemyai::ConfigDefaults::load();
            let path = defaults
                .resolve_open_path(path.map(PathBuf::from))
                .display()
                .to_string();
            let agent_id = defaults.resolve_open_agent(agent_id);
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
            let key = resolve_encryption_key(encryption_key, credential_mode.as_deref())?;
            let mem = basemyai::Memory::open_native(PathBuf::from(path), &key, embedder, agent)
                .await
                .map_err(to_pyerr)?;
            Ok(Self { inner: Arc::new(mem) })
        })
    }

    /// Ouvre une mémoire **éphémère, non chiffrée** avec un embedder
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

    /// Ingère une conversation brute : chaque tour `(role, content)` devient
    /// un souvenir épisodique (`"{role}: {content}"`), en un seul batch.
    /// Aucune extraction de faits ici — c'est la consolidation (tâche de
    /// fond) qui promeut plus tard des faits durables en couche `semantic`
    /// à partir de ces épisodes. Rend la `list[str]` des UUID créés, dans
    /// l'ordre des tours.
    fn observe<'p>(&self, py: Python<'p>, turns: Vec<(String, String)>) -> PyResult<Bound<'p, PyAny>> {
        let inner = Arc::clone(&self.inner);
        future_into_py(py, async move {
            let turns: Vec<basemyai::ConversationTurn> = turns
                .into_iter()
                .map(|(role, content)| basemyai::ConversationTurn::new(role, content))
                .collect();
            inner.observe(&turns).await.map_err(to_pyerr)
        })
    }

    /// Recall temporel sémantique : rend une `list[Record]`.
    #[pyo3(signature = (query, k = 5, *, include_procedural = false, exclude_imported = false))]
    fn recall<'p>(
        &self,
        py: Python<'p>,
        query: String,
        k: usize,
        include_procedural: bool,
        exclude_imported: bool,
    ) -> PyResult<Bound<'p, PyAny>> {
        let inner = Arc::clone(&self.inner);
        future_into_py(py, async move {
            let options = basemyai::RecallOptions {
                include_procedural,
                exclude_imported,
            };
            let records = inner.recall_with_options(&query, k, options).await.map_err(to_pyerr)?;
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
    #[pyo3(signature = (query, k = 5, *, include_procedural = false, exclude_imported = false))]
    fn recall_hybrid<'p>(
        &self,
        py: Python<'p>,
        query: String,
        k: usize,
        include_procedural: bool,
        exclude_imported: bool,
    ) -> PyResult<Bound<'p, PyAny>> {
        let inner = Arc::clone(&self.inner);
        future_into_py(py, async move {
            let options = basemyai::RecallOptions {
                include_procedural,
                exclude_imported,
            };
            let records = inner
                .recall_hybrid_with_options(&query, k, options)
                .await
                .map_err(to_pyerr)?;
            Ok(records.into_iter().map(Record::from_hybrid).collect::<Vec<_>>())
        })
    }

    /// Compile un recall hybride en contexte Markdown borné et traçable.
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (query, token_budget, *, candidate_limit = 64, include_procedural = false, source_policy = "exclude_imported".to_string(), explain = false))]
    fn compile_context<'p>(
        &self,
        py: Python<'p>,
        query: String,
        token_budget: usize,
        candidate_limit: usize,
        include_procedural: bool,
        source_policy: String,
        explain: bool,
    ) -> PyResult<Bound<'p, PyAny>> {
        let inner = Arc::clone(&self.inner);
        let source_policy = parse_source_policy(&source_policy).map_err(ValidationError::new_err)?;
        future_into_py(py, async move {
            let mut request = basemyai::ContextRequest::new(&query, token_budget)
                .candidate_limit(candidate_limit)
                .source_policy(source_policy);
            if include_procedural {
                request = request.include_procedural();
            }
            if explain {
                request = request.explain();
            }
            let bundle = inner.compile_context(request).await.map_err(to_pyerr)?;
            Ok(ContextBundle::from(bundle))
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

    /// Démarre un abonnement aux événements mémoire de **cet** agent (ADR-022,
    /// seconde vague — PLAN.md §P2.1). Rend un [`MemoryWatch`], un itérateur
    /// asynchrone Python : `async for event in memory.watch():` consomme les
    /// événements au fur et à mesure qu'ils arrivent (`remember`/`invalidate`/
    /// `forget`/consolidation), jusqu'à ce que la boucle soit interrompue ou
    /// que la `Memory` sous-jacente disparaisse. `layer` restreint optionnellement
    /// à une couche unique. L'isolation par agent/couche est déjà garantie par
    /// `MemorySubscription::recv` (ADR-022) — cette méthode passe `agent_id` tel
    /// quel, elle ne refait aucun filtrage.
    #[pyo3(signature = (layer = None))]
    fn watch(&self, layer: Option<String>) -> PyResult<MemoryWatch> {
        let layer = layer
            .map(|l| MemoryLayer::from_table(&l))
            .transpose()
            .map_err(to_pyerr)?;
        let agent_id = self.inner.agent().as_str().to_string();
        let subscription = self.inner.watch(&agent_id, layer);
        Ok(MemoryWatch {
            subscription: Arc::new(AsyncMutex::new(subscription)),
        })
    }
}

#[cfg(feature = "embed")]
/// Résout la clé de chiffrement. Sans credential explicite et sans source
/// configurée nulle part (env, fichier), **génère et persiste** une clé dans
/// `~/.basemyai/key` plutôt que d'échouer ([`EncryptionKey::resolve_or_generate`])
/// — générer une clé est une opération locale hors-ligne, contrairement au
/// téléchargement de modèle (ADR-010) qui lui reste soumis à consentement
/// explicite. Imprime un avis sur stderr quand une clé vient d'être créée :
/// c'est la seule copie, sa perte rend les données existantes irrécupérables.
fn resolve_encryption_key(encryption_key: Option<String>, credential_mode: Option<&str>) -> PyResult<EncryptionKey> {
    match encryption_key {
        Some(material) => match credential_mode.unwrap_or("raw") {
            "raw" | "raw-key" | "raw_key" => Ok(EncryptionKey::raw(material)),
            "passphrase" => Ok(EncryptionKey::passphrase(material)),
            other => Err(ValidationError::new_err(format!(
                "credential_mode must be 'raw' or 'passphrase', got {other:?}"
            ))),
        },
        None => {
            let (key, generated_at) =
                EncryptionKey::resolve_or_generate(None).map_err(|e| EncryptionError::new_err(e.to_string()))?;
            if let Some(path) = generated_at {
                eprintln!(
                    "basemyai: generated a new encryption key at {} — back this file up, it cannot be recovered if lost",
                    path.display()
                );
            }
            Ok(key)
        }
    }
}

#[cfg(all(test, feature = "embed"))]
mod credential_tests {
    use basemyai_core::EncryptionKeyMode;

    use super::resolve_encryption_key;

    #[test]
    fn explicit_credentials_use_the_per_call_mode() {
        assert_eq!(
            resolve_encryption_key(Some("secret".to_string()), Some("raw"))
                .expect("raw credential")
                .mode(),
            EncryptionKeyMode::RawKey
        );
        assert_eq!(
            resolve_encryption_key(Some("secret".to_string()), Some("passphrase"))
                .expect("passphrase credential")
                .mode(),
            EncryptionKeyMode::Passphrase
        );
    }

    #[test]
    fn explicit_credentials_reject_unknown_modes() {
        assert!(resolve_encryption_key(Some("secret".to_string()), Some("automatic")).is_err());
    }
}

/// Itérateur asynchrone Python rendu par [`Memory::watch`]. Porte un
/// [`MemorySubscription`] derrière un verrou tokio (interior mutability) :
/// `__anext__` prend `&self` — c'est le protocole Python — pas `&mut self`,
/// donc l'exclusivité vient du verrou, pas de l'emprunt Rust.
#[pyclass]
pub struct MemoryWatch {
    subscription: Arc<AsyncMutex<MemorySubscription>>,
}

#[pymethods]
impl MemoryWatch {
    fn __aiter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    /// Rend l'awaitable du prochain événement. Lève `StopAsyncIteration`
    /// quand le canal source est fermé (la `Memory` d'origine a disparu) —
    /// c'est le seul cas où le flux se termine, sinon il attend indéfiniment.
    fn __anext__<'p>(&self, py: Python<'p>) -> PyResult<Bound<'p, PyAny>> {
        let subscription = Arc::clone(&self.subscription);
        future_into_py(py, async move {
            let mut sub = subscription.lock().await;
            match sub.recv().await {
                Some(event) => Ok(WatchEvent::from(event)),
                None => Err(PyStopAsyncIteration::new_err(())),
            }
        })
    }
}

#[cfg(feature = "embed")]
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
