//! Façade `Memory` exposée à Node. Les méthodes `async` deviennent des `Promise`
//! JS, exécutées sur le runtime tokio interne de NAPI-RS. Moteur 100 % local.

#[cfg(any(feature = "embed", feature = "test-util"))]
use std::path::PathBuf;
use std::sync::Arc;

use napi::Result;
#[cfg(not(feature = "embed"))]
use napi::{Error, Status};
use napi_derive::napi;

use basemyai::MemoryLayer;
#[cfg(any(feature = "embed", feature = "test-util"))]
use basemyai_core::Store;
#[cfg(feature = "embed")]
use basemyai_core::{CandleEmbedder, Device, EncryptionKey};

use crate::errors::to_napi;
use crate::types::{AgentStats, Entity, MemoryOpenOptions, Record};

/// Mémoire d'un agent (tenant). Ouverte par une fabrique asynchrone, puis
/// interrogée par `remember`/`recall`/... (toutes des `Promise`).
#[napi]
pub struct Memory {
    inner: Arc<basemyai::Memory>,
}

#[napi]
impl Memory {
    /// Ouvre une mémoire persistée de production, chiffrée, avec embedder Candle.
    #[napi(factory)]
    pub async fn open(options: MemoryOpenOptions) -> Result<Memory> {
        open_production(options).await
    }

    /// `agent_id` propriétaire de cette mémoire.
    #[napi]
    pub fn agent(&self) -> String {
        self.inner.agent().as_str().to_string()
    }

    /// Mémorise `text` dans une couche (défaut `semantic`). Résout vers l'UUID.
    #[napi]
    pub async fn remember(&self, text: String, layer: Option<String>) -> Result<String> {
        let inner = Arc::clone(&self.inner);
        let layer = MemoryLayer::from_table(layer.as_deref().unwrap_or("semantic")).map_err(to_napi)?;
        inner.remember(&text, layer).await.map_err(to_napi)
    }

    /// Recall temporel sémantique : résout vers un tableau de `Record`.
    #[napi]
    pub async fn recall(&self, query: String, k: Option<u32>) -> Result<Vec<Record>> {
        let inner = Arc::clone(&self.inner);
        let k = k.unwrap_or(5) as usize;
        let records = inner.recall(&query, k).await.map_err(to_napi)?;
        Ok(records.into_iter().map(Record::from).collect())
    }

    /// Recall limité à une couche mémoire (`short_term`, `episodic`, `procedural`, `semantic`).
    #[napi(js_name = "recallByLayer")]
    pub async fn recall_by_layer(&self, query: String, layer: String, k: Option<u32>) -> Result<Vec<Record>> {
        let inner = Arc::clone(&self.inner);
        let layer = MemoryLayer::from_table(&layer).map_err(to_napi)?;
        let k = k.unwrap_or(5) as usize;
        let records = inner.recall_by_layer(&query, layer, k).await.map_err(to_napi)?;
        Ok(records.into_iter().map(Record::from).collect())
    }

    /// Recall hybride : vecteur + BM25 (full-text) fusionnés par RRF. Résout vers
    /// un tableau de `Record` (le `score` porte le score RRF fusionné).
    #[napi(js_name = "recallHybrid")]
    pub async fn recall_hybrid(&self, query: String, k: Option<u32>) -> Result<Vec<Record>> {
        let inner = Arc::clone(&self.inner);
        let k = k.unwrap_or(5) as usize;
        let records = inner.recall_hybrid(&query, k).await.map_err(to_napi)?;
        // `score` = score RRF fusionné, conservé tel quel (pas de similarité).
        Ok(records
            .into_iter()
            .map(|r| Record {
                id: r.id,
                text: r.text,
                layer: r.layer.table().to_string(),
                score: f64::from(r.score),
            })
            .collect())
    }

    /// Invalide (soft-delete) un souvenir par son id.
    #[napi]
    pub async fn invalidate(&self, id: String) -> Result<()> {
        let inner = Arc::clone(&self.inner);
        inner.invalidate(&id).await.map_err(to_napi)
    }

    /// Supprime physiquement un souvenir (droit à l'effacement).
    #[napi]
    pub async fn forget(&self, id: String) -> Result<()> {
        let inner = Arc::clone(&self.inner);
        inner.forget(&id).await.map_err(to_napi)
    }

    /// Compte des souvenirs valides par couche : résout vers `AgentStats`.
    #[napi]
    pub async fn stats(&self) -> Result<AgentStats> {
        let inner = Arc::clone(&self.inner);
        let stats = inner.stats().await.map_err(to_napi)?;
        Ok(AgentStats::from(stats))
    }

    /// Insère ou met à jour une entité du graphe pour cet agent.
    #[napi(js_name = "addGraphEntity")]
    pub async fn add_graph_entity(&self, id: String, kind: String, label: String) -> Result<()> {
        let inner = Arc::clone(&self.inner);
        inner.graph().add_entity(&id, &kind, &label).await.map_err(to_napi)
    }

    /// Crée ou met à jour une relation orientée du graphe pour cet agent.
    #[napi(js_name = "addGraphEdge")]
    pub async fn add_graph_edge(&self, src: String, relation: String, dst: String, weight: Option<f64>) -> Result<()> {
        let inner = Arc::clone(&self.inner);
        inner
            .graph()
            .add_edge(&src, &relation, &dst, weight.unwrap_or(1.0))
            .await
            .map_err(to_napi)
    }

    /// Traverse le graphe depuis `start` : résout vers un tableau d'`Entity`.
    #[napi(js_name = "recallGraph")]
    pub async fn recall_graph(&self, start: String, max_depth: Option<u32>) -> Result<Vec<Entity>> {
        let inner = Arc::clone(&self.inner);
        let reached = inner
            .graph()
            .traverse(&start, max_depth.unwrap_or(2))
            .await
            .map_err(to_napi)?;
        Ok(reached.into_iter().map(Entity::from).collect())
    }
}

#[cfg(feature = "embed")]
async fn open_production(options: MemoryOpenOptions) -> Result<Memory> {
    let MemoryOpenOptions {
        path,
        agent_id,
        encryption_key,
        model_path,
        allow_model_download,
    } = options;

    let agent = basemyai::AgentId::new(agent_id).ok_or_else(|| to_napi(basemyai::MemoryError::MissingAgent))?;
    let (model_dir, device) = if let Some(model_path) = model_path {
        (PathBuf::from(model_path), Device::Cpu)
    } else {
        let provision = basemyai::provision(allow_model_download.unwrap_or(false))
            .await
            .map_err(to_napi)?;
        (provision.model_path, provision.device)
    };
    let embedder = CandleEmbedder::load(&model_dir, device)
        .map_err(basemyai::MemoryError::from)
        .map_err(to_napi)?;
    let db_path = PathBuf::from(path);
    let store = Store::open(&db_path, Some(EncryptionKey::new(encryption_key)))
        .await
        .map_err(basemyai::MemoryError::from)
        .map_err(to_napi)?;
    let mem = basemyai::Memory::open(store, Box::new(embedder), agent)
        .await
        .map_err(to_napi)?;
    Ok(Memory { inner: Arc::new(mem) })
}

#[cfg(not(feature = "embed"))]
async fn open_production(_options: MemoryOpenOptions) -> Result<Memory> {
    Err(Error::new(
        Status::GenericFailure,
        "Memory.open requires the basemyai-node `embed` feature",
    ))
}

/// Fabrique **test-only** isolée dans son propre bloc `impl` : le `#[cfg]` est
/// posé avant `#[napi]`, donc en build par défaut tout le code généré par la
/// macro (y compris la registration `*_c_callback`) disparaît proprement —
/// sinon napi référence un callback inexistant (E0425).
#[cfg(feature = "test-util")]
#[napi]
impl Memory {
    /// Ouvre une mémoire **éphémère, non chiffrée** (`:memory:`) avec un embedder
    /// déterministe sans modèle. Réservé aux tests/spikes (pas de CMake/Candle).
    #[napi(factory, js_name = "openInMemory")]
    pub async fn open_in_memory(agent_id: String) -> Result<Memory> {
        let mem = basemyai::Memory::open_in_memory(&agent_id).await.map_err(to_napi)?;
        Ok(Memory { inner: Arc::new(mem) })
    }

    /// Ouvre un fichier libSQL partagé avec l'embedder déterministe de test.
    /// Test-only : permet de vérifier l'isolation SQL réelle entre deux agents
    /// sans compiler Candle ni embarquer de modèle.
    #[napi(factory, js_name = "openTestFile")]
    pub async fn open_test_file(path: String, agent_id: String) -> Result<Memory> {
        let agent = basemyai::AgentId::new(agent_id).ok_or_else(|| to_napi(basemyai::MemoryError::MissingAgent))?;
        let store = Store::open(&PathBuf::from(path), None)
            .await
            .map_err(basemyai::MemoryError::from)
            .map_err(to_napi)?;
        store
            .migrate(&basemyai::schema())
            .await
            .map_err(basemyai::MemoryError::from)
            .map_err(to_napi)?;
        let mem = basemyai::Memory::new(store, Box::new(basemyai::HashEmbedder::new()), agent);
        Ok(Memory { inner: Arc::new(mem) })
    }
}
