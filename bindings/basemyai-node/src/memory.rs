// SPDX-License-Identifier: BUSL-1.1
//! Façade `Memory` exposée à Node. Les méthodes `async` deviennent des `Promise`
//! JS, exécutées sur le runtime tokio interne de NAPI-RS. Moteur 100 % local.

#[cfg(feature = "embed")]
use std::path::PathBuf;
use std::sync::Arc;

use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi::{Error, Result, Status};
use napi_derive::napi;
use tokio::sync::oneshot;

use basemyai::MemoryLayer;
#[cfg(feature = "embed")]
use basemyai_core::EncryptionKey;
#[cfg(feature = "embed")]
use basemyai_core::{CandleEmbedder, Device};

use crate::errors::to_napi;
use crate::types::{AgentStats, Entity, MemoryEventPayload, MemoryOpenOptions, Record};

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
    pub async fn recall(
        &self,
        query: String,
        k: Option<u32>,
        include_procedural: Option<bool>,
        exclude_imported: Option<bool>,
    ) -> Result<Vec<Record>> {
        let inner = Arc::clone(&self.inner);
        let k = k.unwrap_or(5) as usize;
        let options = basemyai::RecallOptions {
            include_procedural: include_procedural.unwrap_or(false),
            exclude_imported: exclude_imported.unwrap_or(false),
        };
        let records = inner.recall_with_options(&query, k, options).await.map_err(to_napi)?;
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
    pub async fn recall_hybrid(
        &self,
        query: String,
        k: Option<u32>,
        include_procedural: Option<bool>,
        exclude_imported: Option<bool>,
    ) -> Result<Vec<Record>> {
        let inner = Arc::clone(&self.inner);
        let k = k.unwrap_or(5) as usize;
        let options = basemyai::RecallOptions {
            include_procedural: include_procedural.unwrap_or(false),
            exclude_imported: exclude_imported.unwrap_or(false),
        };
        let records = inner
            .recall_hybrid_with_options(&query, k, options)
            .await
            .map_err(to_napi)?;
        Ok(records.into_iter().map(Record::from_hybrid).collect())
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

    /// S'abonne en direct aux événements mémoire de `agent_id` (et, si fourni,
    /// à une seule couche) — équivalent binding natif du `watch` MCP/REST
    /// (ADR-022). `callback` est invoqué avec un [`MemoryEventPayload`] pour
    /// chaque événement qui passe le filtre d'isolation.
    ///
    /// L'isolation est appliquée **côté** `basemyai::MemorySubscription::recv`,
    /// jamais déléguée à l'appelant : un `agent_id` qui ne correspond pas à
    /// l'agent réellement propriétaire de l'événement ne délivre jamais rien,
    /// quel que soit le filtre demandé ici (défense en profondeur — voir
    /// `watch_isolates_events_from_other_agents` côté REST/MCP).
    ///
    /// Résout immédiatement vers un [`WatchHandle`] : appeler `close()` dessus
    /// (ou le laisser être garbage-collecté côté JS) arrête le relais et
    /// libère la tâche tokio de fond — aucune tâche ne survit indéfiniment
    /// sans abonné vivant.
    #[napi]
    pub async fn watch(
        &self,
        agent_id: String,
        layer: Option<String>,
        callback: ThreadsafeFunction<MemoryEventPayload, (), MemoryEventPayload, Status, false>,
    ) -> Result<WatchHandle> {
        let layer = layer
            .as_deref()
            .map(MemoryLayer::from_table)
            .transpose()
            .map_err(to_napi)?;
        let mem = Arc::clone(&self.inner);
        let mut subscription = mem.watch(&agent_id, layer);
        let (stop_tx, mut stop_rx) = oneshot::channel::<()>();
        tokio::spawn(async move {
            // `mem` reste vivant pour la durée de l'abonnement : le canal
            // `broadcast` d'événements de `Memory` ne disparaît pas tant qu'un
            // abonné actif (ici, cette tâche) le garde en vie.
            let _mem = mem;
            loop {
                tokio::select! {
                    _ = &mut stop_rx => break,
                    event = subscription.recv() => {
                        match event {
                            Some(ev) => {
                                let payload = MemoryEventPayload::from(&ev);
                                callback.call(payload, ThreadsafeFunctionCallMode::NonBlocking);
                            }
                            // Canal fermé (plus aucun `Sender` : `Memory` source détruite).
                            None => break,
                        }
                    }
                }
            }
        });
        Ok(WatchHandle { stop: Some(stop_tx) })
    }
}

/// Poignée d'abonnement renvoyée par [`Memory::watch`]. Tant qu'elle est
/// vivante (ou jusqu'à `close()`), la tâche de relais tourne en tâche de fond
/// et invoque le callback JS à chaque événement. `close()` est idempotent ;
/// elle est aussi appelée implicitement quand l'objet JS est garbage-collecté
/// (via `Drop`), pour ne jamais fuir de tâche tokio.
#[napi]
pub struct WatchHandle {
    stop: Option<oneshot::Sender<()>>,
}

#[napi]
impl WatchHandle {
    /// Arrête l'abonnement : le callback ne sera plus jamais invoqué après cet
    /// appel. Idempotent — un second appel est un no-op.
    #[napi]
    pub fn close(&mut self) {
        if let Some(stop) = self.stop.take() {
            // Le récepteur peut déjà avoir disparu (tâche déjà terminée,
            // p. ex. canal source fermé) : `send` échoue silencieusement,
            // c'est attendu.
            let _ = stop.send(());
        }
    }
}

impl Drop for WatchHandle {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
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
    let key = EncryptionKey::resolve(encryption_key.as_deref())
        .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?;
    let mem = basemyai::Memory::open_native(db_path, &key, Box::new(embedder), agent)
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
}
