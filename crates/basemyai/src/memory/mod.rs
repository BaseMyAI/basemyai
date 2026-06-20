//! Façade mémoire. Injecte les primitives du core (`Store`, `VectorIndex`,
//! `Embedder`) — testable en isolation via des doubles. Applique l'isolation
//! par agent et le RAG temporel par-dessus.

mod isolation;
mod layer;
mod porting;
pub(crate) mod schema;
#[cfg(feature = "test-util")]
mod testutil;

pub use isolation::AgentId;
pub use layer::{AgentStats, MemoryLayer, Record};
pub use porting::ImportReport;
#[cfg(feature = "test-util")]
pub use testutil::HashEmbedder;

use basemyai_core::libsql;
use basemyai_core::{Embedder, Filter, Metric, Store, Value};
use uuid::Uuid;

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

/// Mémoire d'un agent : store (vecteur natif) + embedder, scellés par un
/// [`AgentId`]. Le chiffrement est obligatoire (ADR-007).
pub struct Memory {
    store: Store,
    embedder: Box<dyn Embedder>,
    agent: AgentId,
}

impl Memory {
    /// Assemble une mémoire à partir des primitives du core déjà construites,
    /// **sans** migrer le schéma (à utiliser quand le schéma est déjà en place).
    #[must_use]
    pub fn new(store: Store, embedder: Box<dyn Embedder>, agent: AgentId) -> Self {
        Self { store, embedder, agent }
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
        Ok(Self { store, embedder, agent })
    }

    /// L'agent propriétaire de cette mémoire.
    #[must_use]
    pub fn agent(&self) -> &AgentId {
        &self.agent
    }

    /// Store sous-jacent. `pub(crate)` : la consolidation (même crate) lit les
    /// épisodes et construit un `Graph` dessus, sans exposer le store au public.
    pub(crate) fn store(&self) -> &Store {
        &self.store
    }

    /// Façade graphe sur le **même** store, scellée par le **même** agent.
    ///
    /// Permet aux consommateurs externes (MCP, REST, bindings) de traverser le
    /// graphe entités/relations (`recall_graph`) sans accéder au store, tout en
    /// conservant l'isolation par agent au niveau SQL (ADR-006).
    #[must_use]
    pub fn graph(&self) -> crate::Graph {
        crate::Graph::new(&self.store, self.agent.clone())
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
        let txn = self.store.begin_write().await?;
        insert_memory_row(&txn, &id, self.agent.as_str(), layer, text, validity, &vector, source).await?;
        txn.commit().await?;
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
        self.remember_batch_with_source(texts, layer, validity, SOURCE_USER).await
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
        let txn = self.store.begin_write().await?;
        for ((text, vector), id) in texts.iter().zip(&vectors).zip(&ids) {
            insert_memory_row(&txn, id, self.agent.as_str(), layer, text, validity, vector, source).await?;
        }
        txn.commit().await?;
        Ok(ids)
    }

    /// Recall temporel : pertinent ET valide, borné à cet agent.
    ///
    /// Le filtre combine isolation (`agent_id = ?`) ET temporel
    /// (`valid_from <= ? AND (valid_until IS NULL OR valid_until > ?)`) en un
    /// seul [`Filter`] paramétré — le core ne connaît le sens d'aucun.
    ///
    /// # Errors
    /// Propage les erreurs d'embedding/recherche.
    pub async fn recall(&self, query: &str, k: usize) -> Result<Vec<Record>> {
        let qvec = self.embedder.embed(query)?;
        let now = now_unix();

        let filter = Filter::new(
            "agent_id = ? AND valid_from <= ? AND (valid_until IS NULL OR valid_until > ?)",
            vec![
                Value::Text(self.agent.as_str().to_string()),
                Value::Integer(now),
                Value::Integer(now),
            ],
        );

        let neighbors = self.store.vector_knn("memory", &qvec, k, Some(&filter)).await?;

        let conn = self.store.connect();
        let mut out = Vec::with_capacity(neighbors.len());
        for n in neighbors {
            let mut rows = conn
                .query(
                    "SELECT content, layer FROM memory WHERE id = ?1",
                    libsql::params![n.id.clone()],
                )
                .await
                .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
            if let Some(row) = rows
                .next()
                .await
                .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?
            {
                let content: String = row
                    .get(0)
                    .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
                let layer: String = row
                    .get(1)
                    .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
                out.push(Record {
                    id: n.id,
                    text: content,
                    layer: MemoryLayer::from_table(&layer)?,
                    score: n.distance,
                });
            }
        }
        if !out.is_empty() {
            let now_access = now_unix();
            for record in &out {
                conn.execute(
                    "UPDATE memory SET last_access = ?1 WHERE id = ?2",
                    libsql::params![now_access, record.id.clone()],
                )
                .await
                .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
            }
        }
        Ok(out)
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

        let filter = Filter::new(
            "agent_id = ? AND valid_from <= ? AND (valid_until IS NULL OR valid_until > ?)",
            vec![
                Value::Text(self.agent.as_str().to_string()),
                Value::Integer(now),
                Value::Integer(now),
            ],
        );

        let neighbors = self
            .store
            .vector_knn_metric("memory", &qvec, k, Some(&filter), metric)
            .await?;

        let conn = self.store.connect();
        let mut out = Vec::with_capacity(neighbors.len());
        for n in neighbors {
            let mut rows = conn
                .query(
                    "SELECT content, layer FROM memory WHERE id = ?1",
                    libsql::params![n.id.clone()],
                )
                .await
                .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
            if let Some(row) = rows
                .next()
                .await
                .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?
            {
                let content: String = row
                    .get(0)
                    .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
                let layer: String = row
                    .get(1)
                    .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
                out.push(Record {
                    id: n.id,
                    text: content,
                    layer: MemoryLayer::from_table(&layer)?,
                    score: n.distance,
                });
            }
        }
        if !out.is_empty() {
            let now_access = now_unix();
            for record in &out {
                conn.execute(
                    "UPDATE memory SET last_access = ?1 WHERE id = ?2",
                    libsql::params![now_access, record.id.clone()],
                )
                .await
                .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
            }
        }
        Ok(out)
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
        let vector_ids = self.vector_ranking_ids(query, inner).await?;
        let keyword_ids = self.keyword_ranking_ids(query, inner).await?;

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

        let conn = self.store.connect();
        let mut out = Vec::with_capacity(k.min(fused.len()));
        for f in fused.into_iter().take(k) {
            let mut rows = conn
                .query(
                    "SELECT content, layer FROM memory WHERE id = ?1",
                    libsql::params![f.id.clone()],
                )
                .await
                .map_err(storage)?;
            if let Some(row) = rows.next().await.map_err(storage)? {
                let content: String = row.get(0).map_err(storage)?;
                let layer: String = row.get(1).map_err(storage)?;
                out.push(Record {
                    id: f.id,
                    text: content,
                    layer: MemoryLayer::from_table(&layer)?,
                    #[allow(clippy::cast_possible_truncation)]
                    score: f.score as f32,
                });
            }
        }
        if !out.is_empty() {
            let now_access = now_unix();
            for record in &out {
                conn.execute(
                    "UPDATE memory SET last_access = ?1 WHERE id = ?2",
                    libsql::params![now_access, record.id.clone()],
                )
                .await
                .map_err(storage)?;
            }
        }
        Ok(out)
    }

    /// Classement vectoriel (ids seuls, agent + validité), sans hydratation ni
    /// `last_access` — brique de [`recall_hybrid`].
    async fn vector_ranking_ids(&self, query: &str, k: usize) -> Result<Vec<String>> {
        let qvec = self.embedder.embed(query)?;
        let now = now_unix();
        let filter = Filter::new(
            "agent_id = ? AND valid_from <= ? AND (valid_until IS NULL OR valid_until > ?)",
            vec![
                Value::Text(self.agent.as_str().to_string()),
                Value::Integer(now),
                Value::Integer(now),
            ],
        );
        let neighbors = self.store.vector_knn("memory", &qvec, k, Some(&filter)).await?;
        Ok(neighbors.into_iter().map(|n| n.id).collect())
    }

    /// Classement BM25 (ids seuls, agent + validité) via FTS5 — brique de
    /// [`recall_hybrid`]. La requête est tokenisée et chaque terme cité (literal)
    /// pour éviter les erreurs de syntaxe MATCH ; fusion OR pour le rappel.
    async fn keyword_ranking_ids(&self, query: &str, k: usize) -> Result<Vec<String>> {
        let Some(match_expr) = fts_match_expr(query) else {
            return Ok(Vec::new());
        };
        let now = now_unix();
        let conn = self.store.connect();
        let mut rows = conn
            .query(
                // FTS5 exige le nom réel de la table dans MATCH/bm25 (pas un alias).
                "SELECT memory_fts.id FROM memory_fts JOIN memory m ON m.id = memory_fts.id \
                 WHERE memory_fts MATCH ?1 AND memory_fts.agent_id = ?2 \
                   AND m.valid_from <= ?3 AND (m.valid_until IS NULL OR m.valid_until > ?3) \
                 ORDER BY bm25(memory_fts) LIMIT ?4",
                libsql::params![
                    match_expr,
                    self.agent.as_str(),
                    now,
                    i64::try_from(k).unwrap_or(i64::MAX)
                ],
            )
            .await
            .map_err(storage)?;
        let mut ids = Vec::new();
        while let Some(row) = rows.next().await.map_err(storage)? {
            ids.push(row.get::<String>(0).map_err(storage)?);
        }
        Ok(ids)
    }

    /// Recall filtré sur une couche unique. Met à jour `last_access` sur chaque
    /// souvenir retourné (l'oubli adaptatif en dépend).
    ///
    /// # Errors
    /// Propage les erreurs d'embedding/recherche.
    pub async fn recall_by_layer(&self, query: &str, layer: MemoryLayer, k: usize) -> Result<Vec<Record>> {
        let qvec = self.embedder.embed(query)?;
        let now = now_unix();

        let filter = Filter::new(
            "agent_id = ? AND valid_from <= ? AND (valid_until IS NULL OR valid_until > ?) AND layer = ?",
            vec![
                Value::Text(self.agent.as_str().to_string()),
                Value::Integer(now),
                Value::Integer(now),
                Value::Text(layer.table().to_string()),
            ],
        );

        let neighbors = self.store.vector_knn("memory", &qvec, k, Some(&filter)).await?;

        let conn = self.store.connect();
        let mut out = Vec::with_capacity(neighbors.len());
        for n in &neighbors {
            let mut rows = conn
                .query(
                    "SELECT content, layer FROM memory WHERE id = ?1",
                    libsql::params![n.id.clone()],
                )
                .await
                .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
            if let Some(row) = rows
                .next()
                .await
                .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?
            {
                let content: String = row
                    .get(0)
                    .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
                let layer_str: String = row
                    .get(1)
                    .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
                out.push(Record {
                    id: n.id.clone(),
                    text: content,
                    layer: MemoryLayer::from_table(&layer_str)?,
                    score: n.distance,
                });
            }
        }
        if !out.is_empty() {
            let now_access = now_unix();
            for record in &out {
                conn.execute(
                    "UPDATE memory SET last_access = ?1 WHERE id = ?2",
                    libsql::params![now_access, record.id.clone()],
                )
                .await
                .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
            }
        }
        Ok(out)
    }

    /// Invalide un souvenir en fixant `valid_until = now()`. Il n'apparaît plus
    /// dans les recalls futurs mais reste physiquement en base.
    ///
    /// # Errors
    /// Propage les erreurs de stockage.
    pub async fn invalidate(&self, id: &str) -> Result<()> {
        let now = now_unix();
        let conn = self.store.connect();
        conn.execute(
            "UPDATE memory SET valid_until = ?1 WHERE id = ?2 AND agent_id = ?3",
            libsql::params![now, id, self.agent.as_str()],
        )
        .await
        .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
        Ok(())
    }

    /// Suppression physique d'un souvenir (RGPD, droit à l'effacement).
    /// Atomique : la ligne `memory` et son miroir FTS disparaissent ensemble.
    ///
    /// # Errors
    /// Propage les erreurs de stockage.
    pub async fn forget(&self, id: &str) -> Result<()> {
        let txn = self.store.begin_write().await?;
        txn.execute(
            "DELETE FROM memory WHERE id = ?1 AND agent_id = ?2",
            libsql::params![id, self.agent.as_str()],
        )
        .await
        .map_err(storage)?;
        txn.execute(
            "DELETE FROM memory_fts WHERE id = ?1 AND agent_id = ?2",
            libsql::params![id, self.agent.as_str()],
        )
        .await
        .map_err(storage)?;
        txn.commit().await?;
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
        let txn = self.store.begin_write().await?;
        // Noms de tables en dur (jamais d'input) ; l'agent passe en paramètre lié.
        // `memory_fts` (miroir BM25) est purgé avec le reste (ADR-014).
        for table in ["memory", "entity", "edge", "memory_fts"] {
            txn.execute(
                &format!("DELETE FROM {table} WHERE agent_id = ?1"),
                libsql::params![self.agent.as_str()],
            )
            .await
            .map_err(storage)?;
        }
        txn.commit().await?;
        Ok(())
    }

    /// Statistiques des souvenirs valides de cet agent, par couche.
    ///
    /// # Errors
    /// Propage les erreurs de stockage.
    pub async fn stats(&self) -> Result<AgentStats> {
        let now = now_unix();
        let conn = self.store.connect();
        let mut rows = conn
            .query(
                "SELECT layer, COUNT(*) FROM memory \
                 WHERE agent_id = ?1 AND valid_from <= ?2 \
                   AND (valid_until IS NULL OR valid_until > ?2) \
                 GROUP BY layer",
                libsql::params![self.agent.as_str(), now],
            )
            .await
            .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;

        let mut stats = AgentStats::default();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?
        {
            let layer_str: String = row
                .get(0)
                .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
            let count: i64 = row
                .get(1)
                .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
            let n = usize::try_from(count).unwrap_or(0);
            match layer_str.as_str() {
                "short_term" => stats.short_term = n,
                "episodic" => stats.episodic = n,
                "procedural" => stats.procedural = n,
                "semantic" => stats.semantic = n,
                _ => {}
            }
        }
        Ok(stats)
    }

    /// Recall vectoriel limité aux souvenirs dont le contenu mentionne une entité
    /// du graphe (P2). Met à jour `last_access` sur les résultats.
    ///
    /// # Errors
    /// Propage les erreurs d'embedding/recherche.
    pub async fn search_graph(&self, query: &str, k: usize) -> Result<Vec<Record>> {
        let qvec = self.embedder.embed(query)?;
        let now = now_unix();

        let filter = Filter::new(
            "agent_id = ? AND valid_from <= ? AND (valid_until IS NULL OR valid_until > ?) \
             AND EXISTS (\
               SELECT 1 FROM entity \
               WHERE entity.agent_id = ? \
                 AND (entity.valid_until IS NULL OR entity.valid_until > ?) \
                 AND instr(content, entity.label) > 0\
             )",
            vec![
                Value::Text(self.agent.as_str().to_string()),
                Value::Integer(now),
                Value::Integer(now),
                Value::Text(self.agent.as_str().to_string()),
                Value::Integer(now),
            ],
        );

        let neighbors = self.store.vector_knn("memory", &qvec, k, Some(&filter)).await?;

        let conn = self.store.connect();
        let mut out = Vec::with_capacity(neighbors.len());
        for n in &neighbors {
            let mut rows = conn
                .query(
                    "SELECT content, layer FROM memory WHERE id = ?1",
                    libsql::params![n.id.clone()],
                )
                .await
                .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
            if let Some(row) = rows
                .next()
                .await
                .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?
            {
                let content: String = row
                    .get(0)
                    .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
                let layer_str: String = row
                    .get(1)
                    .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
                out.push(Record {
                    id: n.id.clone(),
                    text: content,
                    layer: MemoryLayer::from_table(&layer_str)?,
                    score: n.distance,
                });
            }
        }
        if !out.is_empty() {
            let now_access = now_unix();
            for record in &out {
                conn.execute(
                    "UPDATE memory SET last_access = ?1 WHERE id = ?2",
                    libsql::params![now_access, record.id.clone()],
                )
                .await
                .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))?;
            }
        }
        Ok(out)
    }
}

/// Mappe une erreur libSQL en [`MemoryError`] (via `CoreError::Storage`).
fn storage(e: libsql::Error) -> crate::MemoryError {
    basemyai_core::CoreError::Storage(e.to_string()).into()
}

/// Rejette un texte dépassant [`MAX_TEXT_LEN`] (DoS de contexte, audit sécurité).
fn check_text_len(text: &str) -> Result<()> {
    let len = text.len();
    if len > MAX_TEXT_LEN {
        return Err(MemoryError::TextTooLong { len, max: MAX_TEXT_LEN });
    }
    Ok(())
}

/// Insère un souvenir (`memory` + miroir FTS, ADR-014) sur la connexion
/// fournie — une [`basemyai_core::WriteTxn`] en pratique, pour que les deux
/// écritures soient atomiques. `source` trace la provenance (`'user'` direct,
/// `'consolidation'` promu par le pipeline LLM, ADR-018 / audit sécurité).
#[allow(clippy::too_many_arguments)]
async fn insert_memory_row(
    conn: &libsql::Connection,
    id: &str,
    agent: &str,
    layer: MemoryLayer,
    text: &str,
    validity: Validity,
    vector: &[f32],
    source: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO memory (id, agent_id, layer, content, valid_from, valid_until, emb, source) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, vector(?7), ?8)",
        libsql::params![
            id,
            agent,
            layer.table(),
            text,
            validity.valid_from,
            validity.valid_until,
            to_vec_literal(vector),
            source,
        ],
    )
    .await
    .map_err(storage)?;
    conn.execute(
        "INSERT INTO memory_fts (id, agent_id, content) VALUES (?1, ?2, ?3)",
        libsql::params![id, agent, text],
    )
    .await
    .map_err(storage)?;
    Ok(())
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

/// Formate un vecteur en littéral SQL `[a,b,c]` consommé par `vector(?)`.
fn to_vec_literal(v: &[f32]) -> String {
    let mut s = String::with_capacity(v.len() * 8 + 2);
    s.push('[');
    for (i, x) in v.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&x.to_string());
    }
    s.push(']');
    s
}
