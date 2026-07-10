// SPDX-License-Identifier: BUSL-1.1
//! Façade mémoire. Injecte les primitives du core (moteur natif, `Embedder`)
//! — testable en isolation via des doubles. Applique l'isolation par agent et
//! le RAG temporel par-dessus.

mod event;
mod isolation;
mod layer;
mod porting;
#[cfg(feature = "test-util")]
mod testutil;
mod trust;

pub use event::{MemoryEvent, MemoryEventKind, MemorySubscription};
pub use isolation::AgentId;
pub use layer::{AgentStats, MemoryLayer, Record};
pub use porting::ImportReport;
#[cfg(feature = "test-util")]
pub use testutil::HashEmbedder;
pub use trust::{SOURCE_CONSOLIDATION, SOURCE_IMPORT, SOURCE_USER, TrustLevel};

use basemyai_core::{Embedder, Metric};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::broadcast;
use uuid::Uuid;

use event::DEFAULT_EVENT_CAPACITY;

use crate::storage::{MemoryStore, NativeMemoryStore, NewMemory};
use crate::temporal::Validity;
use crate::{MemoryError, RRF_K, Ranking, Result, now_unix, rrf_fuse};

/// Borne la taille d'un texte mémorisé (octets). Au-delà, un item démesuré
/// saturerait le prompt de consolidation (`MAX_EPISODES` ne borne que le
/// *nombre* d'épisodes, pas leur taille individuelle) — DoS de contexte.
/// Cohérent avec la limite documentée côté REST (`crates/basemyai-rest/openapi.yaml`).
/// Bornée à `u16::MAX` pour rester compatible avec le format des termes FTS.
pub const MAX_TEXT_LEN: usize = 65_535;

const META_EMBEDDING_MODEL_ID: &str = "embedding_model_id";
const META_EMBEDDING_DIM: &str = "embedding_dim";

/// Options de recall (audit sécurité — memory poisoning, ADR-035/036).
#[derive(Debug, Clone, Copy, Default)]
pub struct RecallOptions {
    /// Inclure la couche `procedural` dans un recall général. Défaut : `false`
    /// (les instructions procédurales ne remontent que via `recall_by_layer`).
    pub include_procedural: bool,
    /// Exclure les souvenirs importés ([`TrustLevel::Import`]) du résultat.
    pub exclude_imported: bool,
}

/// Filtre post-recall selon [`RecallOptions`] (provenance, ADR-036).
fn apply_recall_options(records: Vec<Record>, options: RecallOptions) -> Vec<Record> {
    if !options.exclude_imported {
        return records;
    }
    records
        .into_iter()
        .filter(|r| r.trust() != TrustLevel::Import)
        .collect()
}

/// Mémoire d'un agent : moteur de stockage natif + embedder, scellés par un
/// [`AgentId`]. Le chiffrement est obligatoire (ADR-007/ADR-030).
pub struct Memory {
    engine: Arc<NativeMemoryStore>,
    embedder: Box<dyn Embedder>,
    agent: AgentId,
    /// Diffuseur d'événements mémoire (abonnements temps réel). Émis **après**
    /// commit d'une écriture. Bon marché à conserver/cloner. `send` sans abonné
    /// renvoie `Err` — ignoré (best-effort, cf. [`event`]).
    events: broadcast::Sender<MemoryEvent>,
}

impl Memory {
    /// Assemble une mémoire sur un store natif déjà ouvert, **partagé**
    /// (`Arc`) : vérifie le contrat embedding puis scelle la façade par
    /// `agent`.
    ///
    /// Le moteur natif est **mono-écrivain exclusif** (ADR-025) : ouvrir deux
    /// fois le même répertoire-store corromprait le WAL. Un même
    /// `Arc<NativeMemoryStore>` doit donc être **partagé** entre toutes les
    /// `Memory` d'agents différents qui pointent vers le même store (le
    /// pattern des providers REST/MCP — isolation structurelle par préfixe de
    /// clé, ADR-027 §2, jamais une instance de store par agent). Réservé aux
    /// consommateurs qui construisent/possèdent déjà leur `NativeMemoryStore`
    /// (providers de surface, éphémère de test) — [`Memory::open_native`]
    /// gère le cas commun d'une seule `Memory`.
    ///
    /// # Errors
    /// [`crate::MemoryError::EmbeddingModelMismatch`] si le store a été créé
    /// avec un autre modèle/dimension d'embedding ; propage les erreurs de
    /// stockage.
    pub async fn from_native_store(
        store: Arc<NativeMemoryStore>,
        embedder: Box<dyn Embedder>,
        agent: AgentId,
    ) -> Result<Self> {
        ensure_embedding_contract(&store, embedder.as_ref()).await?;
        let (events, _) = broadcast::channel(DEFAULT_EVENT_CAPACITY);
        Ok(Self {
            engine: store,
            embedder,
            agent,
            events,
        })
    }

    /// Ouvre une mémoire sur un répertoire-store `.bmai` (au besoin le crée),
    /// **chiffré au repos** (ADR-030 — la clé est obligatoire, ADR-007 ; sans
    /// CMake), vérifie le contrat embedding, puis renvoie la façade scellée
    /// par `agent`.
    ///
    /// Ouvre un **nouveau** `NativeMemoryStore` à chaque appel (mono-écrivain
    /// exclusif, ADR-025) : n'appeler qu'**une fois** par répertoire-store
    /// par process — c'est le cas commun d'une seule `Memory` (CLI, spike).
    /// Un consommateur qui sert **plusieurs agents** sur le même store (un
    /// provider REST/MCP) doit ouvrir une fois via
    /// [`NativeMemoryStore::open_encrypted`], l'envelopper en `Arc`, puis
    /// appeler [`Memory::from_native_store`] pour chaque agent — jamais
    /// rouvrir le répertoire.
    ///
    /// # Errors
    /// Erreur de stockage typée si la clé est fausse ou si `path` contient un
    /// store en clair ; [`crate::MemoryError::EmbeddingModelMismatch`] si le
    /// store a été créé avec un autre modèle/dimension d'embedding.
    pub async fn open_native(
        path: impl AsRef<std::path::Path>,
        key: &basemyai_core::EncryptionKey,
        embedder: Box<dyn Embedder>,
        agent: AgentId,
    ) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let key = key.expose().to_string();
        // L'ouverture (recovery WAL, chargement des méta d'index) est
        // bloquante — jamais sur un thread du runtime.
        let store = tokio::task::spawn_blocking(move || NativeMemoryStore::open_encrypted(&path, &key))
            .await
            .map_err(|e| {
                crate::MemoryError::Core(basemyai_core::CoreError::Storage(format!(
                    "ouverture du store natif interrompue : {e}"
                )))
            })??;
        Self::from_native_store(Arc::new(store), embedder, agent).await
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
    /// l'isolation structurelle en amont (ADR-006) ; ce filtre la prolonge au flux.
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

    /// Moteur natif **concret**. `pub(crate)` : réservé à `memory::porting`
    /// (export/import JSONL) et à la rotation de clé — jamais aux chemins
    /// sémantiques, qui passent par le contrat [`MemoryStore`].
    pub(crate) fn native_engine(&self) -> &Arc<NativeMemoryStore> {
        &self.engine
    }

    /// Façade graphe sur le **même** moteur, scellée par le **même** agent.
    ///
    /// Permet aux consommateurs externes (MCP, REST, bindings) de traverser le
    /// graphe entités/relations (`recall_graph`) sans accéder au moteur, tout
    /// en conservant l'isolation par agent (ADR-006).
    #[must_use]
    pub fn graph(&self) -> crate::Graph {
        crate::Graph::new(self.engine(), self.agent.clone())
    }

    /// Ouvre une mémoire **éphémère, non chiffrée** dotée d'un embedder
    /// déterministe sans modèle ([`HashEmbedder`]) — le backend que les
    /// utilisateurs reçoivent réellement, la suite de tests façade l'exerce
    /// tel quel.
    ///
    /// Réservé aux **tests et aux spikes des bindings** : ni Candle, ni fichiers
    /// modèle, ni CMake — un roundtrip remember/recall fonctionne hors-ligne.
    /// **Jamais en production** (vecteurs non sémantiques). Le store est
    /// éphémère : la règle de chiffrement obligatoire ne s'y applique pas.
    ///
    /// # Errors
    /// [`crate::MemoryError::MissingAgent`] si `agent_id` est vide ;
    /// [`crate::MemoryError::Core`] si l'ouverture échoue.
    #[cfg(feature = "test-util")]
    pub async fn open_in_memory(agent_id: &str) -> Result<Self> {
        let agent = AgentId::new(agent_id).ok_or(crate::MemoryError::MissingAgent)?;
        let store = NativeMemoryStore::open_ephemeral()?;
        Self::from_native_store(Arc::new(store), Box::new(HashEmbedder::new()), agent).await
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
    /// L'insertion souvenir + miroir FTS est **atomique** (un seul batch WAL,
    /// N5.5) : jamais de souvenir visible par vecteur mais invisible en BM25,
    /// ou l'inverse.
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
    /// d'embedding ([`Embedder::embed_batch`]) et **un seul batch atomique**
    /// (N5.5) — le lot devient visible d'un coup, ou pas du tout. C'est le
    /// chemin de l'ingestion initiale (import d'historique, seed de connaissances).
    ///
    /// # Errors
    /// [`MemoryError::TextTooLong`] si un texte du lot dépasse [`MAX_TEXT_LEN`]
    /// (fail-fast : le premier texte trop long stoppe le lot, rien n'est inséré
    /// puisque la validation précède l'embedding/l'insertion).
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
    /// Par défaut, la couche `procedural` est **exclue** (audit memory poisoning) ;
    /// passer [`RecallOptions::include_procedural`] ou utiliser [`Self::recall_by_layer`].
    ///
    /// # Errors
    /// Propage les erreurs d'embedding/recherche.
    pub async fn recall(&self, query: &str, k: usize) -> Result<Vec<Record>> {
        self.recall_with_options(query, k, RecallOptions::default()).await
    }

    /// Recall avec options explicites (filtrage procedural, etc.).
    ///
    /// # Errors
    /// Propage les erreurs d'embedding/recherche.
    pub async fn recall_with_options(&self, query: &str, k: usize, options: RecallOptions) -> Result<Vec<Record>> {
        let qvec = self.embedder.embed(query)?;
        let now = now_unix();
        self.engine
            .recall_vector(
                &self.agent,
                &qvec,
                k,
                None,
                Metric::Cosine,
                now,
                options.include_procedural,
            )
            .await
            .map(|records| apply_recall_options(records, options))
    }

    /// Recall sémantique temporel avec **métrique explicite** ([`Metric`]).
    ///
    /// [`Metric::Cosine`] emprunte le chemin natif ; [`Metric::Euclidean`] et
    /// [`Metric::Hamming`] n'ont pas d'implémentation de re-classement sur le
    /// backend natif aujourd'hui (erreur franche, jamais un résultat
    /// silencieusement faux — ADR-032). Met à jour `last_access` sur les
    /// résultats.
    ///
    /// # Errors
    /// Propage les erreurs d'embedding/recherche.
    pub async fn recall_with_metric(&self, query: &str, k: usize, metric: Metric) -> Result<Vec<Record>> {
        self.recall_with_metric_options(query, k, metric, RecallOptions::default())
            .await
    }

    /// Recall sémantique temporel avec métrique et options explicites.
    ///
    /// # Errors
    /// Propage les erreurs d'embedding/recherche.
    pub async fn recall_with_metric_options(
        &self,
        query: &str,
        k: usize,
        metric: Metric,
        options: RecallOptions,
    ) -> Result<Vec<Record>> {
        let qvec = self.embedder.embed(query)?;
        let now = now_unix();
        self.engine
            .recall_vector(&self.agent, &qvec, k, None, metric, now, options.include_procedural)
            .await
            .map(|records| apply_recall_options(records, options))
    }

    /// Recall **hybride** (ADR-014) : fusionne le classement **vectoriel** et le
    /// classement **BM25** (full-text natif, ADR-028) par Reciprocal Rank Fusion
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
        self.recall_hybrid_with_options(query, k, RecallOptions::default())
            .await
    }

    /// Recall hybride avec options explicites.
    ///
    /// # Errors
    /// Propage les erreurs d'embedding/recherche/stockage.
    pub async fn recall_hybrid_with_options(
        &self,
        query: &str,
        k: usize,
        options: RecallOptions,
    ) -> Result<Vec<Record>> {
        // Sur-échantillonne chaque signal pour une fusion plus riche.
        let inner = k.saturating_mul(4).max(k);
        let now = now_unix();
        let qvec = self.embedder.embed(query)?;

        let vector_ids = self
            .engine
            .vector_ranking_ids(&self.agent, &qvec, inner, now, options.include_procedural)
            .await?;
        let keyword_ids = match fts_match_expr(query) {
            Some(match_expr) => {
                self.engine
                    .keyword_ranking_ids(&self.agent, &match_expr, inner, now, options.include_procedural)
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
        let records: Vec<Record> = hydrated
            .into_iter()
            .map(|h| {
                let score = scores.get(h.id.as_str()).copied().unwrap_or(0.0);
                Record {
                    id: h.id,
                    text: h.text,
                    layer: h.layer,
                    score,
                    source: h.source,
                }
            })
            .collect();
        Ok(apply_recall_options(records, options))
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
            .recall_vector(&self.agent, &qvec, k, Some(layer), Metric::Cosine, now, true)
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
    /// Atomique : le souvenir et son miroir FTS disparaissent ensemble.
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

    /// Passe d'oubli adaptatif (VISION §5.2, ADR-012 §4, portée sur le moteur
    /// natif par ADR-037) : scanne tous les souvenirs de cet agent, calcule
    /// le score de rétention (`importance + H/(H+age)`), et évince
    /// physiquement (via [`Self::forget`], donc atomique souvenir+FTS et
    /// événementiel) tout ce qui dépasse `policy.capacity`, du moins bien
    /// noté au mieux noté. No-op si l'agent a `capacity` souvenirs ou moins.
    ///
    /// # Errors
    /// Propage les erreurs de stockage (scan ou éviction).
    pub async fn adaptive_forget(
        &self,
        policy: crate::maintenance::AdaptiveForgettingPolicy,
    ) -> Result<crate::maintenance::ForgettingReport> {
        let candidates = self.engine.scan_for_forgetting(&self.agent).await?;
        let scanned = candidates.len();
        let victims = crate::maintenance::adaptive_forgetting::select_victims(&candidates, now_unix(), policy);
        let evicted = victims.len();
        for id in victims {
            self.forget(&id).await?;
        }
        Ok(crate::maintenance::ForgettingReport { scanned, evicted })
    }

    /// Purge **toutes** les données de cet agent : souvenirs, entités et
    /// relations. Irréversible (RGPD, droit à l'oubli). Idempotent : ne
    /// renvoie pas d'erreur si l'agent n'a aucune donnée.
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
        self.engine
            .recall_graph_filtered(&self.agent, &qvec, k, now, RecallOptions::default().include_procedural)
            .await
    }

    /// Change la clé de chiffrement du store sous-jacent **en place** —
    /// re-scellement O(1) de la DEK sous la nouvelle clé (ADR-030) : cette
    /// `Memory` **reste pleinement utilisable** après l'appel, aucune
    /// réouverture requise.
    ///
    /// # Errors
    /// [`basemyai_core::CoreError::Encryption`] si le store n'est pas
    /// chiffré ; [`basemyai_core::CoreError::Storage`] si la rotation échoue.
    /// Les deux remontent enveloppées dans [`MemoryError::Core`].
    pub async fn rotate_key(&self, new_key: basemyai_core::EncryptionKey) -> Result<()> {
        self.engine.rotate_key(new_key.expose()).await
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

/// Vérifie/scelle le contrat embedding (`embedding_model_id`/`embedding_dim`)
/// sur la méta consommateur du moteur ([`NativeMemoryStore::meta_ensure`]) :
/// sémantique `INSERT OR IGNORE` puis vérification stricte — un embedder
/// différent de celui qui a créé le store est rejeté plutôt que de produire
/// des vecteurs silencieusement incompatibles.
async fn ensure_embedding_contract(store: &NativeMemoryStore, embedder: &dyn Embedder) -> Result<()> {
    let stored_model = store.meta_ensure(META_EMBEDDING_MODEL_ID, embedder.model_id()).await?;
    let stored_dim = store
        .meta_ensure(META_EMBEDDING_DIM, &embedder.dim().to_string())
        .await?
        .parse::<usize>()
        .map_err(|e| MemoryError::EmbeddingMetadata(format!("invalid embedding_dim: {e}")))?;

    if stored_model != embedder.model_id() || stored_dim != embedder.dim() {
        return Err(MemoryError::EmbeddingModelMismatch {
            stored_model,
            stored_dim,
            embedder_model: embedder.model_id().to_string(),
            embedder_dim: embedder.dim(),
        });
    }
    Ok(())
}

/// Construit une expression FTS `MATCH` sûre depuis une requête libre : tokens
/// alphanumériques, chacun cité (literal) et joints par OR (orienté rappel).
/// `None` si aucun token.
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

    fn open_native_store_for_tests() -> Arc<NativeMemoryStore> {
        Arc::new(NativeMemoryStore::open_ephemeral().expect("open ephemeral store"))
    }

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
        let store = open_native_store_for_tests();
        let agent = AgentId::new("metadata-agent").expect("valid agent");
        let memory = Memory::from_native_store(
            Arc::clone(&store),
            Box::new(TestEmbedder {
                model: "test-model-a",
                dim: crate::EMBEDDING_DIM,
            }),
            agent,
        )
        .await
        .expect("memory opens");
        drop(memory);

        let stored = store
            .meta_ensure(META_EMBEDDING_MODEL_ID, "should-not-overwrite")
            .await
            .expect("meta read");
        assert_eq!(stored, "test-model-a");
    }

    #[tokio::test]
    async fn open_rejects_incompatible_embedding_model() {
        let store = open_native_store_for_tests();
        let agent = AgentId::new("metadata-agent").expect("valid agent");
        let _memory = Memory::from_native_store(
            Arc::clone(&store),
            Box::new(TestEmbedder {
                model: "test-model-a",
                dim: crate::EMBEDDING_DIM,
            }),
            agent,
        )
        .await
        .expect("memory opens");

        let err = ensure_embedding_contract(
            &store,
            &TestEmbedder {
                model: "test-model-b",
                dim: crate::EMBEDDING_DIM,
            },
        )
        .await
        .expect_err("different model must be rejected");

        assert!(matches!(err, MemoryError::EmbeddingModelMismatch { .. }));
    }
}
