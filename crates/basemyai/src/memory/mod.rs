//! Façade mémoire. Injecte les primitives du core (`Store`, `VectorIndex`,
//! `Embedder`) — testable en isolation via des doubles. Applique l'isolation
//! par agent et le RAG temporel par-dessus.

mod event;
mod isolation;
mod layer;
mod porting;
pub(crate) mod schema;
#[cfg(feature = "test-util")]
mod testutil;

pub use event::{MemoryEvent, MemoryEventKind, MemorySubscription};
pub use isolation::AgentId;
pub use layer::{AgentStats, MemoryLayer, Record};
pub use porting::ImportReport;
#[cfg(feature = "test-util")]
pub use testutil::HashEmbedder;

use basemyai_core::{Embedder, Metric, Store, libsql};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::broadcast;
use uuid::Uuid;

use event::DEFAULT_EVENT_CAPACITY;

use crate::storage::{LibsqlMemoryStore, MemoryStore, NewMemory};
use crate::temporal::Validity;
use crate::{MemoryError, RRF_K, Ranking, Result, now_unix, rrf_fuse};

/// Borne la taille d'un texte mémorisé (octets). Au-delà, un item démesuré
/// saturerait le prompt de consolidation (`MAX_EPISODES` ne borne que le
/// *nombre* d'épisodes, pas leur taille individuelle) — DoS de contexte.
/// Cohérent avec la limite documentée côté REST (`openapi-sidecar.yaml`).
pub const MAX_TEXT_LEN: usize = 65_536;

/// Provenance par défaut d'un souvenir mémorisé directement par l'agent (par
/// opposition à `"consolidation"`, faits promus par le pipeline LLM).
const SOURCE_USER: &str = "user";
const META_EMBEDDING_MODEL_ID: &str = "embedding_model_id";
const META_EMBEDDING_DIM: &str = "embedding_dim";

/// Provenance des faits promus par consolidation (vs [`SOURCE_USER`]). Référence
/// unique partagée avec `cognition::consolidation` : c'est elle qui distingue un
/// événement [`MemoryEventKind::Consolidated`] d'un [`MemoryEventKind::Remembered`].
pub(crate) const SOURCE_CONSOLIDATION: &str = "consolidation";

/// Mémoire d'un agent : moteur de stockage (vecteur natif) + embedder,
/// scellés par un [`AgentId`]. Le chiffrement est obligatoire (ADR-007).
pub struct Memory {
    engine: Arc<LibsqlMemoryStore>,
    embedder: Box<dyn Embedder>,
    agent: AgentId,
    /// Diffuseur d'événements mémoire (abonnements temps réel). Émis **après**
    /// commit d'une écriture. Bon marché à conserver/cloner. `send` sans abonné
    /// renvoie `Err` — ignoré (best-effort, cf. [`event`]).
    events: broadcast::Sender<MemoryEvent>,
}

impl Memory {
    /// Assemble une mémoire à partir des primitives du core déjà construites,
    /// **sans** migrer le schéma (à utiliser quand le schéma est déjà en place).
    #[must_use]
    fn from_migrated_store(store: Store, embedder: Box<dyn Embedder>, agent: AgentId) -> Self {
        let (events, _) = broadcast::channel(DEFAULT_EVENT_CAPACITY);
        Self {
            engine: Arc::new(LibsqlMemoryStore::new(store)),
            embedder,
            agent,
            events,
        }
    }

    /// Ouvre une mémoire : vérifie le chiffrement, applique le schéma
    /// (`memory` + index vecteur natif), puis renvoie la façade scellée par `agent`.
    ///
    /// Le chiffrement est **obligatoire** pour les stores sur fichier (ADR-007) :
    /// un store `:memory:` est éphémère, la règle ne s'y applique pas.
    ///
    /// # Errors
    /// [`crate::MemoryError::EncryptionRequired`] si le store est sur fichier et non chiffré.
    /// [`crate::MemoryError::Core`] si la migration échoue.
    pub async fn open(store: Store, embedder: Box<dyn Embedder>, agent: AgentId) -> Result<Self> {
        if store.path().is_some() && !store.is_encrypted() {
            return Err(crate::MemoryError::EncryptionRequired);
        }
        store.migrate(&schema::schema()).await?;
        ensure_embedding_contract(&store, embedder.as_ref()).await?;
        Ok(Self::from_migrated_store(store, embedder, agent))
    }

    /// L'agent propriétaire de cette mémoire.
    #[must_use]
    pub fn agent(&self) -> &AgentId {
        &self.agent
    }

    /// S'abonne au flux d'événements mémoire de **cet** agent (et d'une couche
    /// donnée, si `layer` est fourni). L'abonnement renvoyé n'expose jamais le
    /// canal brut : l'isolation par agent/couche est appliquée côté serveur dans
    /// [`MemorySubscription::recv`], jamais déléguée à l'appelant.
    ///
    /// `agent_id` est capturé tel quel : un appelant qui passe l'identifiant
    /// d'un autre agent ne reçoit que les événements de cet autre agent — il ne
    /// peut pas remonter au-delà de ce que [`Memory`] émet, et chaque `Memory`
    /// n'émet que pour son propre agent. La sécurité multi-tenant repose sur
    /// l'isolation SQL en amont (ADR-006) ; ce filtre la prolonge au flux.
    #[must_use]
    pub fn watch(&self, agent_id: &str, layer: Option<MemoryLayer>) -> MemorySubscription {
        MemorySubscription::new(self.events.subscribe(), agent_id.to_string(), layer)
    }

    /// Émet un événement mémoire vers les abonnés. **Best-effort** : un `send`
    /// sans récepteur vivant renvoie `Err` — attendu (personne n'écoute), jamais
    /// propagé. À n'appeler **qu'après** commit de l'écriture concernée.
    fn emit(&self, kind: MemoryEventKind, layer: MemoryLayer, id: &str) {
        let _ = self.events.send(MemoryEvent {
            agent_id: self.agent.as_str().to_string(),
            kind,
            layer,
            id: id.to_string(),
        });
    }

    /// Moteur de stockage sous-jacent, vu à travers le contrat [`MemoryStore`].
    /// `pub(crate)` : la consolidation (même crate) lit les épisodes et
    /// construit un `Graph` dessus, sans exposer le moteur au public.
    pub(crate) fn engine(&self) -> Arc<dyn MemoryStore> {
        Arc::clone(&self.engine) as Arc<dyn MemoryStore>
    }

    /// Moteur de stockage **concret**. `pub(crate)` : réservé à
    /// `memory::porting` (export/import JSONL), qui a besoin de colonnes hors
    /// du contrat sémantique [`MemoryStore`] (cf. [`LibsqlMemoryStore::store`]).
    pub(crate) fn libsql_engine(&self) -> &LibsqlMemoryStore {
        &self.engine
    }

    /// Façade graphe sur le **même** moteur, scellée par le **même** agent.
    ///
    /// Permet aux consommateurs externes (MCP, REST, bindings) de traverser le
    /// graphe entités/relations (`recall_graph`) sans accéder au moteur, tout
    /// en conservant l'isolation par agent au niveau SQL (ADR-006).
    #[must_use]
    pub fn graph(&self) -> crate::Graph {
        crate::Graph::new(self.engine(), self.agent.clone())
    }

    /// Ouvre une mémoire **éphémère, non chiffrée** (`:memory:`) dotée d'un
    /// embedder déterministe sans modèle ([`HashEmbedder`]).
    ///
    /// Réservé aux **tests et aux spikes des bindings** : ni Candle, ni fichiers
    /// modèle, ni CMake — un roundtrip remember/recall fonctionne hors-ligne.
    /// **Jamais en production** (vecteurs non sémantiques). Le store `:memory:`
    /// est éphémère : la règle de chiffrement obligatoire ne s'y applique pas.
    ///
    /// # Errors
    /// [`crate::MemoryError::MissingAgent`] si `agent_id` est vide ;
    /// [`crate::MemoryError::Core`] si l'ouverture/migration échoue.
    #[cfg(feature = "test-util")]
    pub async fn open_in_memory(agent_id: &str) -> Result<Self> {
        let agent = AgentId::new(agent_id).ok_or(crate::MemoryError::MissingAgent)?;
        let store = Store::open_in_memory().await?;
        Self::open(store, Box::new(HashEmbedder::new()), agent).await
    }

    /// Ouvre une mémoire sur un fichier libSQL **non chiffré** avec l'embedder
    /// déterministe de test, en contournant volontairement la règle de
    /// chiffrement obligatoire de [`Memory::open`].
    ///
    /// Réservé aux **tests et spikes des bindings** (`test-util`) : vérifie
    /// l'isolation SQL réelle entre agents sur un vrai fichier partagé, sans
    /// Candle ni CMake. **Jamais en production** — le seul bypass de chiffrement
    /// du crate vit ici, strictement confiné à cette feature.
    ///
    /// # Errors
    /// [`crate::MemoryError::MissingAgent`] si `agent_id` est vide ;
    /// [`crate::MemoryError::Core`] si l'ouverture/migration échoue.
    #[cfg(feature = "test-util")]
    pub async fn open_test_file(path: &std::path::Path, agent_id: &str) -> Result<Self> {
        let agent = AgentId::new(agent_id).ok_or(crate::MemoryError::MissingAgent)?;
        let store = Store::open(path, None).await?;
        store.migrate(&schema::schema()).await?;
        Ok(Self::from_migrated_store(store, Box::new(HashEmbedder::new()), agent))
    }

    /// Mémorise un texte dans une couche, valide dès maintenant et sans
    /// expiration. Renvoie l'identifiant (UUID v4) du souvenir créé.
    ///
    /// # Errors
    /// Propage les erreurs d'embedding/stockage.
    pub async fn remember(&self, text: &str, layer: MemoryLayer) -> Result<String> {
        let now = now_unix();
        self.remember_with(text, layer, Validity::since(now)).await
    }

    /// Mémorise un texte avec une fenêtre de validité explicite. Renvoie
    /// l'identifiant (UUID v4) du souvenir créé — les consommateurs (MCP, REST,
    /// bindings) le retournent à l'appelant pour invalidation/effacement ultérieurs.
    ///
    /// L'insertion `memory` + miroir FTS est **atomique** ([`Store::begin_write`]) :
    /// jamais de souvenir visible par vecteur mais invisible en BM25, ou l'inverse.
    ///
    /// # Errors
    /// [`MemoryError::TextTooLong`] si `text` dépasse [`MAX_TEXT_LEN`].
    /// Propage aussi les erreurs d'embedding/stockage.
    pub async fn remember_with(&self, text: &str, layer: MemoryLayer, validity: Validity) -> Result<String> {
        self.remember_with_source(text, layer, validity, SOURCE_USER).await
    }

    /// Comme [`Memory::remember_with`], mais trace explicitement la `source`
    /// du souvenir (`"user"` pour un appel direct de l'agent, `"consolidation"`
    /// pour un fait promu par le pipeline LLM, ADR-018 / audit sécurité —
    /// memory poisoning). `pub(crate)` : la provenance n'est pas (encore)
    /// exposée à l'API publique de recall, seulement tracée en base.
    ///
    /// # Errors
    /// [`MemoryError::TextTooLong`] si `text` dépasse [`MAX_TEXT_LEN`].
    /// Propage aussi les erreurs d'embedding/stockage.
    pub(crate) async fn remember_with_source(
        &self,
        text: &str,
        layer: MemoryLayer,
        validity: Validity,
        source: &str,
    ) -> Result<String> {
        check_text_len(text)?;
        let vector = self.embedder.embed(text)?;
        let id = Uuid::new_v4().to_string();
        self.engine
            .put_memory(&id, &self.agent, layer, text, validity, &vector, source)
            .await?;
        // Émis après commit : un souvenir visible est annoncé, jamais l'inverse.
        // Un fait promu par consolidation (`source = "consolidation"`) porte le
        // genre `Consolidated` ; un souvenir direct, `Remembered`.
        let kind = if source == SOURCE_CONSOLIDATION {
            MemoryEventKind::Consolidated
        } else {
            MemoryEventKind::Remembered
        };
        self.emit(kind, layer, &id);
        Ok(id)
    }

    /// Mémorise un **lot** de textes dans une couche, valides dès maintenant et
    /// sans expiration. Renvoie les identifiants créés, dans l'ordre des textes.
    ///
    /// # Errors
    /// Propage les erreurs d'embedding/stockage.
    pub async fn remember_batch(&self, texts: &[String], layer: MemoryLayer) -> Result<Vec<String>> {
        let now = now_unix();
        self.remember_batch_with(texts, layer, Validity::since(now)).await
    }

    /// Mémorise un lot avec une fenêtre de validité explicite : **une** passe
    /// d'embedding ([`Embedder::embed_batch`]) et **une** transaction — le lot
    /// devient visible d'un coup, ou pas du tout. C'est le chemin de
    /// l'ingestion initiale (import d'historique, seed de connaissances).
    ///
    /// # Errors
    /// [`MemoryError::TextTooLong`] si un texte du lot dépasse [`MAX_TEXT_LEN`]
    /// (fail-fast : le premier texte trop long stoppe le lot, rien n'est inséré
    /// puisque la validation précède l'embedding/la transaction).
    /// Propage aussi les erreurs d'embedding/stockage.
    pub async fn remember_batch_with(
        &self,
        texts: &[String],
        layer: MemoryLayer,
        validity: Validity,
    ) -> Result<Vec<String>> {
        self.remember_batch_with_source(texts, layer, validity, SOURCE_USER)
            .await
    }

    /// Comme [`Memory::remember_batch_with`], mais trace explicitement la
    /// `source` du lot (cf. [`Memory::remember_with_source`]).
    ///
    /// # Errors
    /// [`MemoryError::TextTooLong`] si un texte du lot dépasse [`MAX_TEXT_LEN`].
    /// Propage aussi les erreurs d'embedding/stockage.
    pub(crate) async fn remember_batch_with_source(
        &self,
        texts: &[String],
        layer: MemoryLayer,
        validity: Validity,
        source: &str,
    ) -> Result<Vec<String>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        for text in texts {
            check_text_len(text)?;
        }
        let vectors = self.embedder.embed_batch(texts)?;
        let ids: Vec<String> = texts.iter().map(|_| Uuid::new_v4().to_string()).collect();
        let items: Vec<NewMemory<'_>> = texts
            .iter()
            .zip(&vectors)
            .zip(&ids)
            .map(|((text, vector), id)| NewMemory {
                id: id.clone(),
                layer,
                text,
                validity,
                vector,
                source,
            })
            .collect();
        self.engine.put_memory_batch(&self.agent, &items).await?;
        // Émis après commit du lot : un événement par souvenir inséré.
        let kind = if source == SOURCE_CONSOLIDATION {
            MemoryEventKind::Consolidated
        } else {
            MemoryEventKind::Remembered
        };
        for id in &ids {
            self.emit(kind, layer, id);
        }
        Ok(ids)
    }

    /// Recall temporel : pertinent ET valide, borné à cet agent.
    ///
    /// L'isolation (`agent_id`) et le filtre temporel sont appliqués par le
    /// moteur de stockage ([`MemoryStore::recall_vector`]) — `Memory` ne
    /// connaît plus le SQL, seulement le vecteur de requête et l'agent.
    ///
    /// # Errors
    /// Propage les erreurs d'embedding/recherche.
    pub async fn recall(&self, query: &str, k: usize) -> Result<Vec<Record>> {
        let qvec = self.embedder.embed(query)?;
        let now = now_unix();
        self.engine
            .recall_vector(&self.agent, &qvec, k, None, Metric::Cosine, now)
            .await
    }

    /// Recall sémantique temporel avec **métrique explicite** ([`Metric`]).
    ///
    /// [`Metric::Cosine`] emprunte le chemin natif ; [`Metric::Euclidean`] et
    /// [`Metric::Hamming`] sur-échantillonnent les candidats cosinus puis sont
    /// re-classées en Rust (ADR-012). Met à jour `last_access` sur les résultats.
    ///
    /// # Errors
    /// Propage les erreurs d'embedding/recherche.
    pub async fn recall_with_metric(&self, query: &str, k: usize, metric: Metric) -> Result<Vec<Record>> {
        let qvec = self.embedder.embed(query)?;
        let now = now_unix();
        self.engine
            .recall_vector(&self.agent, &qvec, k, None, metric, now)
            .await
    }

    /// Recall **hybride** (ADR-014) : fusionne le classement **vectoriel** et le
    /// classement **BM25** (full-text natif libSQL) par Reciprocal Rank Fusion
    /// ([`rrf_fuse`]). Un terme exact que l'embedding rate (sigle, identifiant
    /// rare, nom propre) remonte par BM25 ; une reformulation que les mots-clés
    /// ratent remonte par le vecteur. Borné à l'agent et à la validité.
    /// Met à jour `last_access` sur les résultats.
    ///
    /// Le `score` du [`Record`] retourné porte le **score RRF fusionné**, pas la
    /// similarité cosinus.
    ///
    /// # Errors
    /// Propage les erreurs d'embedding/recherche/stockage.
    pub async fn recall_hybrid(&self, query: &str, k: usize) -> Result<Vec<Record>> {
        // Sur-échantillonne chaque signal pour une fusion plus riche.
        let inner = k.saturating_mul(4).max(k);
        let now = now_unix();
        let qvec = self.embedder.embed(query)?;

        let vector_ids = self.engine.vector_ranking_ids(&self.agent, &qvec, inner, now).await?;
        let keyword_ids = match fts_match_expr(query) {
            Some(match_expr) => {
                self.engine
                    .keyword_ranking_ids(&self.agent, &match_expr, inner, now)
                    .await?
            }
            None => Vec::new(),
        };

        let fused = rrf_fuse(
            &[
                Ranking {
                    signal: "vector".to_string(),
                    ids: vector_ids,
                },
                Ranking {
                    signal: "keyword".to_string(),
                    ids: keyword_ids,
                },
            ],
            RRF_K,
        );

        let top_ids: Vec<String> = fused.iter().take(k).map(|f| f.id.clone()).collect();
        #[allow(clippy::cast_possible_truncation)]
        let scores: HashMap<&str, f32> = fused.iter().take(k).map(|f| (f.id.as_str(), f.score as f32)).collect();

        let hydrated = self.engine.hydrate(&self.agent, &top_ids, now).await?;
        Ok(hydrated
            .into_iter()
            .map(|h| {
                let score = scores.get(h.id.as_str()).copied().unwrap_or(0.0);
                Record {
                    id: h.id,
                    text: h.text,
                    layer: h.layer,
                    score,
                }
            })
            .collect())
    }

    /// Recall filtré sur une couche unique. Met à jour `last_access` sur chaque
    /// souvenir retourné (l'oubli adaptatif en dépend).
    ///
    /// # Errors
    /// Propage les erreurs d'embedding/recherche.
    pub async fn recall_by_layer(&self, query: &str, layer: MemoryLayer, k: usize) -> Result<Vec<Record>> {
        let qvec = self.embedder.embed(query)?;
        let now = now_unix();
        self.engine
            .recall_vector(&self.agent, &qvec, k, Some(layer), Metric::Cosine, now)
            .await
    }

    /// Invalide un souvenir en fixant `valid_until = now()`. Il n'apparaît plus
    /// dans les recalls futurs mais reste physiquement en base.
    ///
    /// # Errors
    /// Propage les erreurs de stockage.
    pub async fn invalidate(&self, id: &str) -> Result<()> {
        // Capturer la couche AVANT l'invalidation : le souvenir reste en base
        // (seul `valid_until` change), donc lisible. `None` ⇒ aucun souvenir de
        // cet agent ⇒ no-op silencieux, aucun événement (pas de fuite cross-agent).
        let layer = self.engine.layer_of(&self.agent, id).await?;
        self.engine.invalidate(&self.agent, id, now_unix()).await?;
        if let Some(layer) = layer {
            self.emit(MemoryEventKind::Invalidated, layer, id);
        }
        Ok(())
    }

    /// Suppression physique d'un souvenir (RGPD, droit à l'effacement).
    /// Atomique : la ligne `memory` et son miroir FTS disparaissent ensemble.
    ///
    /// # Errors
    /// Propage les erreurs de stockage.
    pub async fn forget(&self, id: &str) -> Result<()> {
        // Capturer la couche AVANT l'effacement physique (après, la ligne a
        // disparu). `None` ⇒ rien à effacer pour cet agent ⇒ aucun événement.
        let layer = self.engine.layer_of(&self.agent, id).await?;
        self.engine.forget(&self.agent, id).await?;
        if let Some(layer) = layer {
            self.emit(MemoryEventKind::Forgotten, layer, id);
        }
        Ok(())
    }

    /// Purge **toutes** les données de cet agent : souvenirs (`memory`), entités
    /// (`entity`) et relations (`edge`). Irréversible (RGPD, droit à l'oubli).
    /// Idempotent : ne renvoie pas d'erreur si l'agent n'a aucune donnée.
    /// Atomique : pas de purge partielle possible (tout ou rien).
    ///
    /// # Errors
    /// Propage les erreurs de stockage.
    pub async fn purge_agent(&self) -> Result<()> {
        self.engine.purge_agent(&self.agent).await
    }

    /// Statistiques des souvenirs valides de cet agent, par couche.
    ///
    /// # Errors
    /// Propage les erreurs de stockage.
    pub async fn stats(&self) -> Result<AgentStats> {
        self.engine.agent_stats(&self.agent, now_unix()).await
    }

    /// Recall vectoriel limité aux souvenirs dont le contenu mentionne une entité
    /// du graphe (P2). Met à jour `last_access` sur les résultats.
    ///
    /// # Errors
    /// Propage les erreurs d'embedding/recherche.
    pub async fn search_graph(&self, query: &str, k: usize) -> Result<Vec<Record>> {
        let qvec = self.embedder.embed(query)?;
        let now = now_unix();
        self.engine.recall_graph_filtered(&self.agent, &qvec, k, now).await
    }
}

/// Rejette un texte dépassant [`MAX_TEXT_LEN`] (DoS de contexte, audit sécurité).
fn check_text_len(text: &str) -> Result<()> {
    let len = text.len();
    if len > MAX_TEXT_LEN {
        return Err(MemoryError::TextTooLong { len, max: MAX_TEXT_LEN });
    }
    Ok(())
}

async fn ensure_embedding_contract(store: &Store, embedder: &dyn Embedder) -> Result<()> {
    let conn = store.connect();
    conn.execute(
        "INSERT OR IGNORE INTO bmai_meta (key, value) VALUES (?1, ?2)",
        libsql::params![META_EMBEDDING_MODEL_ID, embedder.model_id()],
    )
    .await
    .map_err(storage)?;
    conn.execute(
        "INSERT OR IGNORE INTO bmai_meta (key, value) VALUES (?1, ?2)",
        libsql::params![META_EMBEDDING_DIM, embedder.dim().to_string()],
    )
    .await
    .map_err(storage)?;

    let mut rows = conn
        .query(
            "SELECT key, value FROM bmai_meta WHERE key IN (?1, ?2)",
            libsql::params![META_EMBEDDING_MODEL_ID, META_EMBEDDING_DIM],
        )
        .await
        .map_err(storage)?;

    let mut meta = HashMap::new();
    while let Some(row) = rows.next().await.map_err(storage)? {
        meta.insert(
            row.get::<String>(0).map_err(storage)?,
            row.get::<String>(1).map_err(storage)?,
        );
    }

    let stored_model = meta
        .get(META_EMBEDDING_MODEL_ID)
        .ok_or_else(|| MemoryError::EmbeddingMetadata("missing embedding_model_id".into()))?;
    let stored_dim = meta
        .get(META_EMBEDDING_DIM)
        .ok_or_else(|| MemoryError::EmbeddingMetadata("missing embedding_dim".into()))?
        .parse::<usize>()
        .map_err(|e| MemoryError::EmbeddingMetadata(format!("invalid embedding_dim: {e}")))?;

    if stored_model != embedder.model_id() || stored_dim != embedder.dim() {
        return Err(MemoryError::EmbeddingModelMismatch {
            stored_model: stored_model.clone(),
            stored_dim,
            embedder_model: embedder.model_id().to_string(),
            embedder_dim: embedder.dim(),
        });
    }

    Ok(())
}

fn storage(e: libsql::Error) -> MemoryError {
    basemyai_core::CoreError::Storage(e.to_string()).into()
}

/// Construit une expression FTS5 MATCH sûre depuis une requête libre : tokens
/// alphanumériques, chacun cité (literal, donc insensible aux mots-clés FTS5
/// comme AND/OR/NEAR) et joints par OR (orienté rappel). `None` si aucun token.
fn fts_match_expr(query: &str) -> Option<String> {
    let tokens: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .take(32)
        .map(|t| format!("\"{}\"", t.to_lowercase()))
        .collect();
    if tokens.is_empty() {
        None
    } else {
        Some(tokens.join(" OR "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use basemyai_core::Result as CoreResult;

    struct TestEmbedder {
        model: &'static str,
        dim: usize,
    }

    impl Embedder for TestEmbedder {
        fn embed(&self, _text: &str) -> CoreResult<Vec<f32>> {
            Ok(vec![0.0; self.dim])
        }

        fn embed_batch(&self, texts: &[String]) -> CoreResult<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|_| vec![0.0; self.dim]).collect())
        }

        fn model_id(&self) -> &str {
            self.model
        }

        fn dim(&self) -> usize {
            self.dim
        }
    }

    #[tokio::test]
    async fn open_records_embedding_model_metadata() {
        let store = Store::open_in_memory().await.expect("store opens");
        let agent = AgentId::new("metadata-agent").expect("valid agent");
        let memory = Memory::open(
            store,
            Box::new(TestEmbedder {
                model: "test-model-a",
                dim: schema::EMBEDDING_DIM,
            }),
            agent,
        )
        .await
        .expect("memory opens");

        let conn = memory.libsql_engine().store().connect();
        let mut rows = conn
            .query(
                "SELECT value FROM bmai_meta WHERE key = ?1",
                libsql::params![META_EMBEDDING_MODEL_ID],
            )
            .await
            .expect("metadata query");
        let row = rows.next().await.expect("row read").expect("metadata row");
        let model = row.get::<String>(0).expect("model value");
        assert_eq!(model, "test-model-a");
    }

    #[tokio::test]
    async fn open_rejects_incompatible_embedding_model() {
        let store = Store::open_in_memory().await.expect("store opens");
        let agent = AgentId::new("metadata-agent").expect("valid agent");
        let memory = Memory::open(
            store,
            Box::new(TestEmbedder {
                model: "test-model-a",
                dim: schema::EMBEDDING_DIM,
            }),
            agent,
        )
        .await
        .expect("memory opens");

        let err = ensure_embedding_contract(
            memory.libsql_engine().store(),
            &TestEmbedder {
                model: "test-model-b",
                dim: schema::EMBEDDING_DIM,
            },
        )
        .await
        .expect_err("different model must be rejected");

        assert!(matches!(err, MemoryError::EmbeddingModelMismatch { .. }));
    }
}
