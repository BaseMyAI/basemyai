// SPDX-License-Identifier: BUSL-1.1
//! Frontière moteur de stockage. [`basemyai_core::StorageEngine`] reste le
//! contrat d'**identité/capacités** bas niveau (inchangé par ce module).
//! [`MemoryStore`] est un *second* contrat, à un niveau sémantique différent :
//! il connaît `agent_id`, les couches mémoire et le graphe — exactement ce que
//! `basemyai-core` n'a pas le droit de connaître (ADR-001). Il vit donc ici,
//! dans `basemyai`, jamais dans le core agnostique.
//!
//! [`NativeMemoryStore`] est l'**unique** implémentation depuis ADR-032
//! (libSQL retiré du workspace, ADR-011 clos) — le trait reste un seam
//! délibéré (testabilité, ADR-020), pas une abstraction sans second cas
//! d'usage.

pub mod integrity;
mod native_store;

pub use native_store::{BMAI_FORMAT_VERSION, NativeExportRows, NativeMemoryStore};
pub(crate) use native_store::{NativeImportEdge, NativeImportEntity, NativeImportMemory};

use basemyai_core::Metric;

use crate::cognition::Reached;
use crate::temporal::Validity;
use crate::{AgentId, AgentStats, MemoryLayer, Record, Result};

/// Importance par défaut d'un souvenir inséré — parité avec le défaut
/// historique V1 (`1.0`) pour tout appelant qui ne fixe pas explicitement
/// (`Memory::remember_with_importance`, ADR-041 §7.1).
pub const DEFAULT_IMPORTANCE: f64 = 1.0;

/// Un souvenir prêt à insérer, pour [`MemoryStore::put_memory_batch`].
#[derive(Debug, Clone)]
pub struct NewMemory<'a> {
    pub id: String,
    pub layer: MemoryLayer,
    pub text: &'a str,
    pub validity: Validity,
    pub vector: &'a [f32],
    pub source: &'a str,
    /// Composante `importance` du score d'oubli adaptatif (ADR-012 §4,
    /// ADR-041 §7.1). `1.0` par défaut pour tout appelant qui ne fixe pas
    /// explicitement — parité avec le défaut historique.
    pub importance: f64,
}

/// Un souvenir listé (`MemoryStore::list_memories`) — toutes les colonnes que
/// le CLI `list` affiche, sans score de classement (ce n'est pas un recall).
#[derive(Debug, Clone)]
pub struct ListedRecord {
    pub id: String,
    pub layer: MemoryLayer,
    pub content: String,
    pub valid_from: i64,
    pub valid_until: Option<i64>,
}

/// Un souvenir hydraté (contenu + couche), sans score de classement — brique
/// de [`MemoryStore::hydrate`], partagée par les chemins de recall qui
/// attachent leur propre score (distance cosinus, RRF…) après hydratation.
#[derive(Debug, Clone)]
pub struct HydratedRecord {
    pub id: String,
    pub text: String,
    pub layer: MemoryLayer,
    pub source: String,
}

/// Un candidat à l'oubli adaptatif (VISION §5.2, ADR-012, portée sur le
/// moteur natif par ADR-037, périmètre affiné pour n'inclure que les
/// souvenirs **actifs** — voir la note sur [`MemoryStore::scan_for_forgetting`]) :
/// seulement les colonnes nécessaires au score de rétention, jamais le
/// contenu (le scan n'a pas besoin de le charger).
#[derive(Debug, Clone)]
pub struct ForgetCandidate {
    pub id: String,
    pub importance: f64,
    pub last_access: i64,
}

/// Bornes d'un lot de suppression physique ([`MemoryStore::forget_many`],
/// ADR-041 §7.4) : jamais plus de `max_items` souvenirs ni (approximativement)
/// plus de `max_wal_bytes` d'opérations agrégées dans une seule transaction
/// moteur. Cibles de dimensionnement, pas des bornes exactes au fil : un
/// souvenir dont l'empreinte propre dépasse `max_wal_bytes` part quand même,
/// seul dans son lot (l'atomicité par souvenir est le plancher).
#[derive(Debug, Clone, Copy)]
pub struct ForgetBatchOptions {
    /// Nombre maximum de souvenirs supprimés par lot atomique (`0` est
    /// ramené à `1` — un lot vide ne progresserait jamais).
    pub max_items: usize,
    /// Budget approximatif en octets d'un lot atomique.
    pub max_wal_bytes: usize,
}

impl Default for ForgetBatchOptions {
    /// Mêmes défauts (ordre de grandeur, pas un optimum mesuré) que le
    /// moteur natif : 256 souvenirs ou ~4 Mio par lot, premier atteint.
    fn default() -> Self {
        Self {
            max_items: 256,
            max_wal_bytes: 4 * 1024 * 1024,
        }
    }
}

/// Un candidat au GC temporel (ADR-038) : uniquement ce qu'il faut pour
/// journaliser/paginer, jamais le contenu. `valid_until` est toujours
/// `Some` ici (c'est le prédicat même de l'expiration) mais reste porté en
/// clair plutôt que déballé, pour un rapport diagnostiquable sans second aller-retour.
#[derive(Debug, Clone)]
pub struct ExpiredCandidate {
    pub id: String,
    pub valid_until: i64,
}

/// Contrat d'opérations mémoire : tout ce que `basemyai` a besoin de demander
/// à un moteur de stockage, en langage métier (agent, couche, graphe) — jamais
/// en SQL. Object-safe (`#[async_trait]`), même convention que
/// [`LlmInference`](crate::LlmInference).
///
/// Un seul trait, pas de split `MemoryStore`/`GraphStore` : il n'y a qu'une
/// implémentation ([`NativeMemoryStore`]), scinder maintenant serait une
/// abstraction sans second cas d'usage réel.
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
        importance: f64,
    ) -> Result<()>;

    /// Insère un lot de souvenirs en une seule transaction. No-op sur lot vide.
    async fn put_memory_batch(&self, agent: &AgentId, items: &[NewMemory<'_>]) -> Result<()>;

    /// Réécrit la composante `importance` d'un souvenir existant, borné à
    /// `agent` (ADR-041 §7.1). No-op silencieux si absent/autre agent — même
    /// parité UPDATE que [`MemoryStore::invalidate`].
    async fn set_importance(&self, agent: &AgentId, id: &str, importance: f64) -> Result<()>;

    /// KNN vectoriel, borné à `agent` + validité temporelle, filtré sur une
    /// couche optionnelle. Hydrate et marque `last_access` sur les résultats.
    #[allow(clippy::too_many_arguments)]
    async fn recall_vector(
        &self,
        agent: &AgentId,
        query: &[f32],
        k: usize,
        layer: Option<MemoryLayer>,
        metric: Metric,
        now: i64,
        include_procedural: bool,
    ) -> Result<Vec<Record>>;

    /// KNN vectoriel filtré aux souvenirs mentionnant une entité du graphe.
    async fn recall_graph_filtered(
        &self,
        agent: &AgentId,
        query: &[f32],
        k: usize,
        now: i64,
        include_procedural: bool,
    ) -> Result<Vec<Record>>;

    /// Classement vectoriel (ids seuls), sans hydratation ni `last_access` —
    /// brique du recall hybride (RRF).
    async fn vector_ranking_ids(
        &self,
        agent: &AgentId,
        query: &[f32],
        k: usize,
        now: i64,
        include_procedural: bool,
    ) -> Result<Vec<String>>;

    /// Classement BM25 (ids seuls) via FTS5 — brique du recall hybride (RRF).
    /// `match_expr` est déjà construit (tokenisé, cité) par l'appelant.
    async fn keyword_ranking_ids(
        &self,
        agent: &AgentId,
        match_expr: &str,
        k: usize,
        now: i64,
        include_procedural: bool,
    ) -> Result<Vec<String>>;

    /// Hydrate des ids en `(contenu, couche)` pour `agent` et marque
    /// `last_access`. Ordre préservé ; un id absent (ou appartenant à un autre
    /// agent) est silencieusement omis plutôt que de faire échouer tout l'appel.
    async fn hydrate(&self, agent: &AgentId, ids: &[String], now: i64) -> Result<Vec<HydratedRecord>>;

    /// Invalide un souvenir (`valid_until = now`), borné à `agent`.
    async fn invalidate(&self, agent: &AgentId, id: &str, now: i64) -> Result<()>;

    /// Suppression physique atomique (souvenir + miroir FTS), borné à `agent`.
    async fn forget(&self, agent: &AgentId, id: &str) -> Result<()>;

    /// Suppression physique de **plusieurs** souvenirs de `agent`, par lots
    /// atomiques bornés (ADR-041 §7.4) : au sein d'un lot, souvenirs, miroirs
    /// FTS, tombstones vectorielles et entrées d'index temporel partent en
    /// **une** transaction moteur — jamais une transaction par souvenir,
    /// jamais un lot géant non plus ([`ForgetBatchOptions`]). Idempotent et
    /// reprennable **entre** les lots : les ids absents (ou d'un autre agent,
    /// ou dupliqués) sont silencieusement ignorés — même parité DELETE
    /// qu'[`Self::forget`] — donc une interruption se répare en relançant.
    /// Renvoie le nombre de souvenirs effectivement supprimés.
    async fn forget_many(&self, agent: &AgentId, ids: &[String], options: ForgetBatchOptions) -> Result<u64>;

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
    async fn exact_fact_exists(&self, agent: &AgentId, content: &str, at: i64) -> Result<bool>;

    /// Couche d'un souvenir par id, borné à `agent` — `None` si absent (ou
    /// appartenant à un autre agent). Brique de l'étiquetage des événements
    /// `Invalidated`/`Forgotten` de la façade [`Memory`](crate::Memory) :
    /// n'émettre que si un souvenir existe réellement pour cet agent, jamais
    /// sur un no-op cross-agent.
    async fn layer_of(&self, agent: &AgentId, id: &str) -> Result<Option<MemoryLayer>>;

    /// Liste les souvenirs de `agent`, du plus récent au plus ancien
    /// (`valid_from` décroissant), filtrés sur une couche optionnelle et
    /// bornés à `limit` — brique du CLI `list` (diagnostic, pas un recall :
    /// aucun embedding). `include_invalid` inclut les souvenirs dont
    /// `valid_until` est déjà passé (défaut : exclus).
    async fn list_memories(
        &self,
        agent: &AgentId,
        layer: Option<MemoryLayer>,
        limit: usize,
        include_invalid: bool,
        now: i64,
    ) -> Result<Vec<ListedRecord>>;

    /// Une page de candidats à l'oubli adaptatif de `agent` :
    /// `importance`/`last_access` par souvenir, triée par id croissant,
    /// curseur `after_id` exclusif (le dernier id **candidat** vu à la page
    /// précédente — `None` pour la première page), bornée à `limit`
    /// candidats (ADR-041 §7.3 — le scan complet d'ADR-037 matérialisait
    /// tout l'agent, exactement ce qu'une passe à mémoire bornée doit
    /// éviter). Volontairement pas de tri par score ici : c'est la brique
    /// brute, la politique (capacité, demi-vie) vit côté
    /// [`crate::maintenance`].
    ///
    /// **Une page plus courte que `limit` signifie que l'agent est épuisé**
    /// — l'implémentation ne s'arrête court qu'en fin de population, jamais
    /// parce qu'un filtrage interne a réduit une page pleine. Une page de
    /// `limit` candidats signifie « rappeler avec `after_id` = le dernier id
    /// renvoyé ».
    ///
    /// **Périmètre : uniquement les souvenirs valides à `now`** (ni
    /// invalidés, ni déjà expirés). Décision affinée par rapport à la V1 :
    /// l'oubli adaptatif borne la population *active*, jamais les reliquats
    /// déjà invalides — ceux-là sont la responsabilité exclusive du GC
    /// temporel ([`Self::scan_expired`], ADR-038). Un souvenir invalidé de
    /// longue date mais très "important" ne doit jamais pouvoir protéger sa
    /// place au détriment d'un souvenir actif moins bien noté : compter les
    /// lignes déjà mortes dans la compétition de capacité fausserait la
    /// sélection (c'est exactement le cas limite qu'ADR-038 §"Non-chevauchement"
    /// couvre).
    async fn scan_for_forgetting(
        &self,
        agent: &AgentId,
        now: i64,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ForgetCandidate>>;

    /// Page de souvenirs **expirés** de `agent` (`valid_until <= now`),
    /// triée par id croissant, curseur `after_id` exclusif (le dernier id
    /// vu à la page précédente — `None` pour la première page), bornée à
    /// `limit` entrées (ADR-038). Le curseur est porté par l'id plutôt que
    /// par la position : une page reste correcte même si des lignes
    /// disparaissent entre deux appels (le cas normal — le GC efface au fur
    /// et à mesure).
    ///
    /// Ne renvoie **jamais** un souvenir dont `valid_until` est `None`
    /// (validité indéfinie) — seule l'expiration temporelle explicite est en
    /// jeu ici, jamais `valid_from` dans le futur (pas encore actif ≠ expiré).
    async fn scan_expired(
        &self,
        agent: &AgentId,
        now: i64,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ExpiredCandidate>>;
}
