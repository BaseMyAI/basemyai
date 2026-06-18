//! Façade `Memory` exposée à Node. Les méthodes `async` deviennent des `Promise`
//! JS, exécutées sur le runtime tokio interne de NAPI-RS. Moteur 100 % local.

use std::sync::Arc;

use napi::Result;
use napi_derive::napi;

use basemyai::MemoryLayer;

use crate::errors::to_napi;
use crate::types::{AgentStats, Entity, Record};

/// Mémoire d'un agent (tenant). Ouverte par une fabrique asynchrone, puis
/// interrogée par `remember`/`recall`/... (toutes des `Promise`).
#[napi]
pub struct Memory {
    inner: Arc<basemyai::Memory>,
}

#[napi]
impl Memory {
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
