// SPDX-License-Identifier: BUSL-1.1
//! Frontière moteur de stockage (suivi ADR-019 : *« Gradually move SQL/libSQL-
//! specific code behind an engine module… Add backend contract tests before
//! any second backend exists »*).
//!
//! [`basemyai_core::StorageEngine`] reste le contrat d'**identité/capacités**
//! bas niveau (inchangé par ce module). [`MemoryStore`] est un *second*
//! contrat, à un niveau sémantique différent : il connaît `agent_id`, les
//! couches mémoire et le graphe — exactement ce que `basemyai-core` n'a pas le
//! droit de connaître (ADR-001). Il vit donc ici, dans `basemyai`, jamais dans
//! le core agnostique.
//!
//! [`Filter`](basemyai_core::Filter)/[`Value`](basemyai_core::Value) et le SQL
//! brut n'apparaissent dans **aucune** signature de [`MemoryStore`] : ils
//! restent un détail d'implémentation de [`LibsqlMemoryStore`], la seule
//! implémentation prévue en V1.

mod libsql_store;
#[cfg(feature = "engine-native")]
mod native_store;

pub use libsql_store::LibsqlMemoryStore;
#[cfg(feature = "engine-native")]
pub use native_store::NativeMemoryStore;

use basemyai_core::Metric;

use crate::cognition::Reached;
use crate::temporal::Validity;
use crate::{AgentId, AgentStats, MemoryLayer, Record, Result};

/// Un souvenir prêt à insérer, pour [`MemoryStore::put_memory_batch`].
#[derive(Debug, Clone)]
pub struct NewMemory<'a> {
    pub id: String,
    pub layer: MemoryLayer,
    pub text: &'a str,
    pub validity: Validity,
    pub vector: &'a [f32],
    pub source: &'a str,
}

/// Un souvenir hydraté (contenu + couche), sans score de classement — brique
/// de [`MemoryStore::hydrate`], partagée par les chemins de recall qui
/// attachent leur propre score (distance cosinus, RRF…) après hydratation.
#[derive(Debug, Clone)]
pub struct HydratedRecord {
    pub id: String,
    pub text: String,
    pub layer: MemoryLayer,
}

/// Contrat d'opérations mémoire : tout ce que `basemyai` a besoin de demander
/// à un moteur de stockage, en langage métier (agent, couche, graphe) — jamais
/// en SQL. Object-safe (`#[async_trait]`), même convention que
/// [`LlmInference`](crate::LlmInference).
///
/// Un seul trait, pas de split `MemoryStore`/`GraphStore` : il n'y a qu'une
/// implémentation prévue en V1 ([`LibsqlMemoryStore`]), scinder maintenant
/// serait une abstraction sans second cas d'usage réel.
#[async_trait::async_trait]
pub trait MemoryStore: Send + Sync {
    /// Insère un souvenir (et son miroir FTS) de façon atomique.
    #[allow(clippy::too_many_arguments)]
    async fn put_memory(
        &self,
        id: &str,
        agent: &AgentId,
        layer: MemoryLayer,
        text: &str,
        validity: Validity,
        vector: &[f32],
        source: &str,
    ) -> Result<()>;

    /// Insère un lot de souvenirs en une seule transaction. No-op sur lot vide.
    async fn put_memory_batch(&self, agent: &AgentId, items: &[NewMemory<'_>]) -> Result<()>;

    /// KNN vectoriel, borné à `agent` + validité temporelle, filtré sur une
    /// couche optionnelle. Hydrate et marque `last_access` sur les résultats.
    async fn recall_vector(
        &self,
        agent: &AgentId,
        query: &[f32],
        k: usize,
        layer: Option<MemoryLayer>,
        metric: Metric,
        now: i64,
    ) -> Result<Vec<Record>>;

    /// KNN vectoriel filtré aux souvenirs mentionnant une entité du graphe.
    async fn recall_graph_filtered(&self, agent: &AgentId, query: &[f32], k: usize, now: i64) -> Result<Vec<Record>>;

    /// Classement vectoriel (ids seuls), sans hydratation ni `last_access` —
    /// brique du recall hybride (RRF).
    async fn vector_ranking_ids(&self, agent: &AgentId, query: &[f32], k: usize, now: i64) -> Result<Vec<String>>;

    /// Classement BM25 (ids seuls) via FTS5 — brique du recall hybride (RRF).
    /// `match_expr` est déjà construit (tokenisé, cité) par l'appelant.
    async fn keyword_ranking_ids(&self, agent: &AgentId, match_expr: &str, k: usize, now: i64) -> Result<Vec<String>>;

    /// Hydrate des ids en `(contenu, couche)` pour `agent` et marque
    /// `last_access`. Ordre préservé ; un id absent (ou appartenant à un autre
    /// agent) est silencieusement omis plutôt que de faire échouer tout l'appel.
    async fn hydrate(&self, agent: &AgentId, ids: &[String], now: i64) -> Result<Vec<HydratedRecord>>;

    /// Invalide un souvenir (`valid_until = now`), borné à `agent`.
    async fn invalidate(&self, agent: &AgentId, id: &str, now: i64) -> Result<()>;

    /// Suppression physique atomique (souvenir + miroir FTS), borné à `agent`.
    async fn forget(&self, agent: &AgentId, id: &str) -> Result<()>;

    /// Purge atomique de toutes les données (`memory`/`entity`/`edge`) de `agent`.
    async fn purge_agent(&self, agent: &AgentId) -> Result<()>;

    /// Statistiques par couche des souvenirs valides de `agent`.
    async fn agent_stats(&self, agent: &AgentId, now: i64) -> Result<AgentStats>;

    /// Upsert idempotent d'une entité du graphe.
    async fn graph_upsert_entity(
        &self,
        agent: &AgentId,
        id: &str,
        kind: &str,
        label: &str,
        validity: Validity,
    ) -> Result<()>;

    /// Upsert idempotent d'une relation orientée du graphe.
    async fn graph_upsert_edge(
        &self,
        agent: &AgentId,
        src: &str,
        relation: &str,
        dst: &str,
        weight: f64,
        now: i64,
    ) -> Result<()>;

    /// Traversée multi-sauts du graphe depuis `start`, bornée à `max_depth`.
    async fn graph_traverse(&self, agent: &AgentId, start: &str, max_depth: u32, now: i64) -> Result<Vec<Reached>>;

    /// Contenus des épisodes valides de `agent`, du plus récent au plus
    /// ancien, bornés à `limit` — brique de la consolidation.
    async fn recent_episodes(&self, agent: &AgentId, limit: usize, now: i64) -> Result<Vec<String>>;

    /// `true` si un fait sémantique au contenu **exactement** identique existe
    /// déjà pour `agent` — brique de la déduplication de consolidation (le
    /// volet similarité sémantique reste côté `Memory::recall_by_layer`).
    async fn exact_fact_exists(&self, agent: &AgentId, content: &str) -> Result<bool>;
}
