// SPDX-License-Identifier: BUSL-1.1
//! Seconde implémentation de [`MemoryStore`] : le moteur natif BaseMyAI
//! (ADR-024/025/026, câblage acté par ADR-027). Enveloppe
//! [`basemyai_engine::Engine`] + ses quatre index logiques (vecteur, graphe,
//! mémoire, full-text) et concentre toute la **politique** de requête du
//! backend natif — fenêtres de validité, filtre de couche, oversampling —
//! pendant que la **mécanique** crash-critique (composition de batchs
//! atomiques, allocation d'ids) vit côté moteur
//! (`idx::memory::PersistentMemoryIndex`, `idx::fts::PersistentFts`).
//!
//! ## Parité comportementale (ADR-027 §6, ADR-028)
//!
//! Chaque méthode reproduit la requête SQL de [`super::LibsqlMemoryStore`],
//! y compris ses non-filtres : `hydrate` et `exact_fact_exists` ne vérifient
//! **pas** la validité temporelle (l'original non plus), `graph_upsert_edge`
//! préserve le `valid_from` d'une arête existante et ne met à jour que
//! `weight` (le `ON CONFLICT ... DO UPDATE SET weight` original). Le KNN
//! oversample ×[`OVERSAMPLE`] puis post-filtre (ADR-012 — libSQL fait
//! exactement cela quand un filtre est présent, et ici un filtre
//! agent+validité est *toujours* présent). `keyword_ranking_ids` est BM25
//! natif (ADR-028) sur le sous-ensemble de `match_expr` que
//! `fts_match_expr()` produit réellement — pas de racinisation Porter (gap
//! assumé, ADR-028 §2).
//!
//! `put_memory_batch` est **tout-ou-rien** depuis N5.5
//! (`PersistentMemoryIndex::put_many`, résorbant l'écart initial d'ADR-027
//! §6). Écart restant, assumé et documenté : `purge_agent` est
//! idempotent/reprennable (pas globalement atomique — un crash au milieu se
//! répare en relançant, ADR-027 §6). Les métriques non-cosinus retournent
//! une **erreur franche** (N5.3) — jamais un faux résultat.
//!
//! ## Pont sync↔async et concurrence (ADR-027 §5, N5.5)
//!
//! Le moteur (`basemyai_engine::Engine`) est sync **mono-écrivain** — ça ne
//! change pas ici, `apply_batch`/`put`/`delete` exigent `&mut Engine`. Le
//! trait est async ; chaque méthode s'exécute dans `tokio::task::
//! spawn_blocking`, le verrou pris à l'intérieur de la closure bloquante —
//! jamais tenu à travers un `.await` (lint `await_holding_lock`). Depuis
//! N5.5, `inner` est un `RwLock` : les lectures pures (`vector_ranking_ids`,
//! `keyword_ranking_ids`, `agent_stats`, `graph_traverse`,
//! `recent_episodes`, `exact_fact_exists`) prennent un verrou de lecture et
//! s'exécutent concurremment entre elles (mesuré : ~3× plus rapide que
//! séquentiel sur 64 lectures mixtes, `tests/memory_tests.rs
//! native_concurrent_reads_are_correct_and_faster_than_sequential`). Les
//! chemins hybrides (`recall_vector`, `recall_graph_filtered`, `hydrate`)
//! font deux passes — recherche sous verrou de lecture, `touch` de
//! `last_access` sous un verrou d'écriture bref séparé — plutôt qu'une passe
//! unique sous verrou exclusif qui bloquerait tout lecteur concurrent
//! pendant toute la recherche. Les écritures restent sérialisées entre elles
//! (verrou d'écriture exclusif) : lever *ça* exigerait de faire du moteur
//! lui-même un multi-écrivain, hors périmètre N5.5 (voir
//! `docs/adr/ADR-027-native-memory-store.md` §5).

use std::path::Path;
use std::sync::{Arc, RwLock};

use basemyai_core::Metric;
use basemyai_engine::{
    Engine, NewMemoryRecord, PersistentFts, PersistentGraph, PersistentMemoryIndex, PersistentVectorIndex,
};

use super::{HydratedRecord, MemoryStore, NewMemory};
use crate::cognition::Reached;
use crate::temporal::Validity;
use crate::{AgentId, AgentStats, MemoryLayer, Record, Result};

/// Facteur d'oversampling du KNN filtré (ADR-012) : on demande `k × 8`
/// candidats à l'index, puis le post-filtre agent/validité/couche réduit à
/// `k` — même politique que le `vector_knn` libSQL en présence d'un filtre.
const OVERSAMPLE: usize = 8;

/// Importance par défaut d'un souvenir inséré — parité avec le `DEFAULT 1.0`
/// de la colonne `importance` du schéma libSQL.
const DEFAULT_IMPORTANCE: f64 = 1.0;

/// Moteur de stockage natif — ADR-024/ADR-027, feature `engine-native`.
///
/// Concurrence (N5.5, barre hardening M6) : `inner` est un `RwLock`, pas un
/// `Mutex` — les chemins de lecture pure (`vector_ranking_ids`,
/// `keyword_ranking_ids`, `agent_stats`, `graph_traverse`,
/// `recent_episodes`, `exact_fact_exists`) prennent un verrou de **lecture**
/// et s'exécutent concurremment entre eux. Les chemins hybrides
/// (`recall_vector`, `recall_graph_filtered`, `hydrate`) font deux passes
/// séparées : la recherche sous verrou de lecture, puis le `touch`
/// (`last_access`) sous un bref verrou d'écriture — jamais une passe unique
/// sous verrou d'écriture qui bloquerait les lecteurs pendant toute la
/// recherche. Les écritures (`put_memory*`, `invalidate`, `forget`,
/// `purge_agent`, `graph_upsert_*`, `rotate_key`) restent sous verrou
/// d'écriture exclusif — `Engine` lui-même reste mono-écrivain (ADR-025) ;
/// ce `RwLock` ne change rien à ça, il ne fait qu'arrêter de sérialiser les
/// lecteurs entre eux. Voir `docs/adr/ADR-027-native-memory-store.md` §5
/// pour le contexte : ce `RwLock` remplace le `Mutex` que ce paragraphe
/// décrivait comme la barre à lever en N5.5.
pub struct NativeMemoryStore {
    inner: Arc<RwLock<NativeInner>>,
    /// Garde de vie du répertoire temporaire d'[`Self::open_ephemeral`] —
    /// supprimé au drop du store, comme un `open_in_memory` libSQL.
    #[cfg(feature = "test-util")]
    _tempdir: Option<tempfile::TempDir>,
}

struct NativeInner {
    engine: Engine,
    vectors: PersistentVectorIndex,
    memory: PersistentMemoryIndex,
    graph: PersistentGraph,
    fts: PersistentFts,
}

/// Mappe une erreur du backend natif (ou du pont async) en
/// [`crate::MemoryError`] — même convention que le `storage()` de
/// `libsql_store.rs`.
fn storage(e: impl std::fmt::Display) -> crate::MemoryError {
    basemyai_core::CoreError::Storage(e.to_string()).into()
}

impl NativeMemoryStore {
    /// Ouvre (en le créant au besoin) un store natif dans le répertoire
    /// `path`, à la dimension d'embedding du schéma
    /// ([`crate::EMBEDDING_DIM`]).
    ///
    /// # Errors
    /// Erreur de stockage si le moteur ou l'un de ses index ne s'ouvre pas
    /// (I/O, corruption non réparable, dimension incompatible avec un index
    /// existant).
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_engine(Engine::open(path).map_err(storage)?)
    }

    /// Ouvre (en le créant au besoin) un store natif **chiffré au repos**
    /// (ADR-030) : WAL et SST scellés sous la DEK du store, `key` vérifiée
    /// contre `crypto.meta` à l'ouverture — une mauvaise clé échoue ici,
    /// typée, jamais en corruption inexplicable plus loin.
    ///
    /// # Errors
    /// Erreur de stockage si la clé est fausse, si `path` contient déjà un
    /// store en clair (pas de chiffrement a posteriori, ADR-030 §2), ou sur
    /// toute erreur I/O/corruption d'ouverture.
    pub fn open_encrypted(path: impl AsRef<Path>, key: &str) -> Result<Self> {
        Self::from_engine(Engine::open_encrypted(path, key.as_bytes()).map_err(storage)?)
    }

    fn from_engine(mut engine: Engine) -> Result<Self> {
        let params = basemyai_engine::VectorIndexParams::with_dim(crate::EMBEDDING_DIM);
        let vectors = PersistentVectorIndex::open(&mut engine, params).map_err(storage)?;
        let memory = PersistentMemoryIndex::open(&engine).map_err(storage)?;
        Ok(Self {
            inner: Arc::new(RwLock::new(NativeInner {
                engine,
                vectors,
                memory,
                graph: PersistentGraph::new(),
                fts: PersistentFts::new(),
            })),
            #[cfg(feature = "test-util")]
            _tempdir: None,
        })
    }

    /// Fait tourner la clé de chiffrement **en place** (ADR-030 §4) : la DEK
    /// du store est ré-enveloppée sous une KEK dérivée de `new_key`,
    /// `crypto.meta` remplacé atomiquement. O(1), et contrairement à
    /// [`Memory::rotate_key`](crate::Memory::rotate_key) côté libSQL,
    /// **cette instance reste pleinement utilisable après l'appel** — pas de
    /// réouverture requise.
    ///
    /// # Errors
    /// Erreur de stockage si le store n'a pas été ouvert chiffré (rien à
    /// rotater — parité de posture avec `CoreError::Encryption`, ADR-007) ou
    /// si le remplacement atomique échoue.
    pub async fn rotate_key(&self, new_key: &str) -> Result<()> {
        let new_key = new_key.to_string();
        self.with_inner(move |inner| inner.engine.rotate_key(new_key.as_bytes()).map_err(storage))
            .await
    }

    /// Store natif jetable dans un répertoire temporaire, supprimé au drop —
    /// l'équivalent natif de `Store::open_in_memory` (le moteur LSM n'a pas
    /// de mode in-memory). Réservé aux tests, comme son homologue libSQL.
    ///
    /// # Errors
    /// Erreur de stockage si le répertoire temporaire ou le store ne se
    /// crée pas.
    #[cfg(feature = "test-util")]
    pub fn open_ephemeral() -> Result<Self> {
        let dir = tempfile::tempdir().map_err(storage)?;
        let mut store = Self::open(dir.path())?;
        store._tempdir = Some(dir);
        Ok(store)
    }

    /// Variante chiffrée d'[`Self::open_ephemeral`] — même répertoire
    /// temporaire jetable, ouvert via [`Self::open_encrypted`]. Réservé aux
    /// tests (le diff multi-backend rejoue la suite complète des scénarios
    /// contre un store natif chiffré, N5.4).
    ///
    /// # Errors
    /// Erreur de stockage si le répertoire temporaire ou le store ne se
    /// crée pas.
    #[cfg(feature = "test-util")]
    pub fn open_ephemeral_encrypted(key: &str) -> Result<Self> {
        let dir = tempfile::tempdir().map_err(storage)?;
        let mut store = Self::open_encrypted(dir.path(), key)?;
        store._tempdir = Some(dir);
        Ok(store)
    }

    /// Exécute `f` sur l'état natif dans le pool bloquant de tokio sous
    /// verrou d'**écriture** (exclusif), pris à l'intérieur de la closure
    /// (jamais à travers un `.await`) — les mutations (`put_memory*`,
    /// `invalidate`, `forget`, `purge_agent`, `graph_upsert_*`,
    /// `rotate_key`, et le `touch` des chemins hybrides) passent par ici.
    async fn with_inner<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut NativeInner) -> Result<T> + Send + 'static,
    {
        let inner = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let mut guard = inner
                .write()
                .map_err(|_| storage("verrou d'écriture du store natif empoisonné"))?;
            f(&mut guard)
        })
        .await
        .map_err(|e| storage(format!("tâche bloquante du store natif interrompue : {e}")))?
    }

    /// [`Self::with_inner`], sous verrou de **lecture** partagé (N5.5) : `f`
    /// n'a droit qu'à `&NativeInner` — plusieurs lectures peuvent s'exécuter
    /// concurremment tant qu'aucune écriture n'est en cours. Réservé aux
    /// chemins qui ne mutent rien (ni les index, ni `last_access`).
    async fn with_inner_read<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&NativeInner) -> Result<T> + Send + 'static,
    {
        let inner = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let guard = inner
                .read()
                .map_err(|_| storage("verrou de lecture du store natif empoisonné"))?;
            f(&guard)
        })
        .await
        .map_err(|e| storage(format!("tâche bloquante du store natif interrompue : {e}")))?
    }
}

/// `true` si la fenêtre `[valid_from, valid_until)` couvre `now` — le filtre
/// `valid_from <= ? AND (valid_until IS NULL OR valid_until > ?)` commun à
/// tous les recalls (ADR-005).
fn record_valid_at(record: &basemyai_engine::MemoryRecord, now: i64) -> bool {
    record.valid_from <= now && record.valid_until.is_none_or(|until| until > now)
}

impl NativeInner {
    /// KNN oversamplé puis post-filtré : les (id, record, distance) des `k`
    /// plus proches souvenirs **de cet agent, valides à `now`**, couche
    /// optionnelle — la brique commune de tous les chemins vectoriels.
    fn search_filtered(
        &self,
        agent: &AgentId,
        query: &[f32],
        k: usize,
        layer: Option<MemoryLayer>,
        now: i64,
    ) -> Result<Vec<(String, basemyai_engine::MemoryRecord, f32)>> {
        let Self {
            engine,
            vectors,
            memory,
            ..
        } = self;
        let oversampled = k.saturating_mul(OVERSAMPLE);
        let hits = vectors.search_scored(engine, query, oversampled).map_err(storage)?;

        let mut out = Vec::with_capacity(k);
        for (vec_id, distance) in hits {
            let Some(mapping) = memory.resolve(engine, vec_id).map_err(storage)? else {
                // Id sans mapping : reliquat bénin d'un forget interrompu
                // (ADR-027 §3) — jamais un résultat.
                continue;
            };
            if mapping.agent != agent.as_str() {
                continue;
            }
            let Some(record) = memory.get(engine, &mapping.agent, &mapping.id).map_err(storage)? else {
                continue;
            };
            if !record_valid_at(&record, now) {
                continue;
            }
            if let Some(l) = layer
                && record.layer != l.table()
            {
                continue;
            }
            out.push((mapping.id, record, distance));
            if out.len() == k {
                break;
            }
        }
        Ok(out)
    }

    /// Marque `last_access = now` sur `ids` (un seul batch atomique côté
    /// moteur, ids absents ignorés — parité `UPDATE` no-op).
    fn touch(&mut self, agent: &AgentId, ids: &[String], now: i64) -> Result<()> {
        let Self { engine, memory, .. } = self;
        memory
            .touch_last_access(engine, agent.as_str(), ids.iter().map(String::as_str), now)
            .map_err(storage)
    }

    fn put_one(&mut self, agent: &AgentId, item: &NewMemory<'_>) -> Result<()> {
        self.put_many(agent, std::slice::from_ref(item))
    }

    /// Insère plusieurs souvenirs de `agent` en **un seul** batch atomique
    /// (N5.5, `PersistentMemoryIndex::put_many`) : plus l'écart « atomique
    /// par item » d'ADR-027 §6 — un `put_memory_batch` natif est désormais
    /// UN enregistrement WAL, tout-ou-rien, comme la transaction libSQL qu'il
    /// remplace.
    fn put_many(&mut self, agent: &AgentId, items: &[NewMemory<'_>]) -> Result<()> {
        let Self {
            engine,
            vectors,
            memory,
            fts,
            ..
        } = self;
        let entries: Vec<(&str, NewMemoryRecord<'_>, Vec<f32>)> = items
            .iter()
            .map(|item| {
                (
                    item.id.as_str(),
                    NewMemoryRecord {
                        layer: item.layer.table(),
                        content: item.text,
                        source: item.source,
                        valid_from: item.validity.valid_from,
                        valid_until: item.validity.valid_until,
                        importance: DEFAULT_IMPORTANCE,
                        last_access: item.validity.valid_from,
                    },
                    item.vector.to_vec(),
                )
            })
            .collect();
        memory
            .put_many(engine, vectors, fts, agent.as_str(), &entries)
            .map_err(storage)?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl MemoryStore for NativeMemoryStore {
    async fn put_memory(
        &self,
        id: &str,
        agent: &AgentId,
        layer: MemoryLayer,
        text: &str,
        validity: Validity,
        vector: &[f32],
        source: &str,
    ) -> Result<()> {
        let agent = agent.clone();
        let item = OwnedNewMemory {
            id: id.to_string(),
            layer,
            text: text.to_string(),
            validity,
            vector: vector.to_vec(),
            source: source.to_string(),
        };
        self.with_inner(move |inner| inner.put_one(&agent, &item.borrowed()))
            .await
    }

    async fn put_memory_batch(&self, agent: &AgentId, items: &[NewMemory<'_>]) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        // Tout-ou-rien (N5.5) : un seul batch atomique côté moteur — voir
        // `NativeInner::put_many`.
        let agent = agent.clone();
        let owned_items: Vec<OwnedNewMemory> = items.iter().map(owned).collect();
        self.with_inner(move |inner| {
            let borrowed: Vec<NewMemory<'_>> = owned_items.iter().map(OwnedNewMemory::borrowed).collect();
            inner.put_many(&agent, &borrowed)
        })
        .await
    }

    async fn recall_vector(
        &self,
        agent: &AgentId,
        query: &[f32],
        k: usize,
        layer: Option<MemoryLayer>,
        metric: Metric,
        now: i64,
    ) -> Result<Vec<Record>> {
        // L'index natif est cosinus (ADR-026) ; les métriques par
        // re-classement (ADR-015) arrivent avec la parité contrats N5.3 —
        // erreur franche plutôt qu'un score silencieusement faux.
        if metric != Metric::Cosine {
            return Err(storage(format!(
                "métrique {metric:?} non implémentée sur le backend natif (re-classement ADR-015, prévu N5.3)"
            )));
        }
        let (agent, query) = (agent.clone(), query.to_vec());
        // Deux passes (N5.5) : la recherche sous verrou de lecture (partagé
        // avec tout autre lecteur concurrent), le `touch` de `last_access`
        // seul sous verrou d'écriture bref — jamais toute la recherche sous
        // verrou exclusif.
        let (agent2, query2) = (agent.clone(), query.clone());
        let found = self
            .with_inner_read(move |inner| inner.search_filtered(&agent2, &query2, k, layer, now))
            .await?;
        let ids: Vec<String> = found.iter().map(|(id, _, _)| id.clone()).collect();
        self.with_inner(move |inner| inner.touch(&agent, &ids, now)).await?;
        found
            .into_iter()
            .map(|(id, record, distance)| {
                Ok(Record {
                    id,
                    text: record.content,
                    layer: MemoryLayer::from_table(&record.layer)?,
                    score: distance,
                })
            })
            .collect()
    }

    async fn recall_graph_filtered(&self, agent: &AgentId, query: &[f32], k: usize, now: i64) -> Result<Vec<Record>> {
        let (agent, query) = (agent.clone(), query.to_vec());
        // Même découpage lecture/écriture que `recall_vector` (N5.5).
        let (agent2, query2) = (agent.clone(), query.clone());
        let found = self
            .with_inner_read(move |inner| {
                // Les labels des entités valides de l'agent (comme l'EXISTS
                // original, seule `valid_until` gate la visibilité d'une entité).
                let labels: Vec<String> = inner
                    .graph
                    .entities(&inner.engine, agent2.as_str())
                    .map_err(storage)?
                    .into_iter()
                    .filter(|(_, e)| e.valid_until.is_none_or(|until| until > now))
                    .map(|(_, e)| e.label)
                    .collect();
                // Oversample large puis filtre « le contenu mentionne un
                // label » (l'`instr(content, entity.label) > 0` original).
                let candidates = inner.search_filtered(&agent2, &query2, k.saturating_mul(OVERSAMPLE), None, now)?;
                Ok(candidates
                    .into_iter()
                    .filter(|(_, record, _)| labels.iter().any(|label| record.content.contains(label.as_str())))
                    .take(k)
                    .collect::<Vec<(String, basemyai_engine::MemoryRecord, f32)>>())
            })
            .await?;
        let ids: Vec<String> = found.iter().map(|(id, _, _)| id.clone()).collect();
        self.with_inner(move |inner| inner.touch(&agent, &ids, now)).await?;
        found
            .into_iter()
            .map(|(id, record, distance)| {
                Ok(Record {
                    id,
                    text: record.content,
                    layer: MemoryLayer::from_table(&record.layer)?,
                    score: distance,
                })
            })
            .collect()
    }

    async fn vector_ranking_ids(&self, agent: &AgentId, query: &[f32], k: usize, now: i64) -> Result<Vec<String>> {
        let (agent, query) = (agent.clone(), query.to_vec());
        // Lecture pure — aucun `touch` (parité avec l'original, ADR-027
        // §6) — verrou de lecture partagé (N5.5).
        self.with_inner_read(move |inner| {
            Ok(inner
                .search_filtered(&agent, &query, k, None, now)?
                .into_iter()
                .map(|(id, _, _)| id)
                .collect())
        })
        .await
    }

    async fn keyword_ranking_ids(&self, agent: &AgentId, match_expr: &str, k: usize, now: i64) -> Result<Vec<String>> {
        let (agent, match_expr) = (agent.clone(), match_expr.to_string());
        // Lecture pure — verrou de lecture partagé (N5.5).
        self.with_inner_read(move |inner| {
            let NativeInner {
                engine, memory, fts, ..
            } = inner;
            // Le moteur (`search_bm25`) est agnostique de la validité
            // temporelle (mécanisme au moteur, sens au consommateur) ; on
            // sur-échantillonne donc comme le chemin vectoriel (ADR-012) pour
            // ne pas sous-compter après le filtre — même raison que libSQL
            // applique son filtre `valid_from`/`valid_until` *avant* son
            // `LIMIT` dans la requête SQL.
            let oversampled = k.saturating_mul(OVERSAMPLE);
            let hits = fts
                .search_bm25(engine, agent.as_str(), &match_expr, oversampled)
                .map_err(storage)?;
            let mut ids = Vec::with_capacity(k.min(hits.len()));
            for (vec_id, _score) in hits {
                let Some(mapping) = memory.resolve(engine, vec_id).map_err(storage)? else {
                    // Reliquat bénin d'un forget interrompu (ADR-027 §3) —
                    // jamais un résultat.
                    continue;
                };
                if mapping.agent != agent.as_str() {
                    continue;
                }
                let Some(record) = memory.get(engine, &mapping.agent, &mapping.id).map_err(storage)? else {
                    continue;
                };
                if !record_valid_at(&record, now) {
                    continue;
                }
                ids.push(mapping.id);
                if ids.len() == k {
                    break;
                }
            }
            Ok(ids)
        })
        .await
    }

    async fn hydrate(&self, agent: &AgentId, ids: &[String], now: i64) -> Result<Vec<HydratedRecord>> {
        let (agent, ids) = (agent.clone(), ids.to_vec());
        // Même découpage lecture/écriture que `recall_vector` (N5.5).
        let (agent2, ids2) = (agent.clone(), ids.clone());
        let out = self
            .with_inner_read(move |inner| {
                let mut out = Vec::with_capacity(ids2.len());
                for id in &ids2 {
                    // Parité : pas de filtre de validité ici (l'original n'en
                    // a pas) ; un id absent ou d'un autre agent est omis.
                    if let Some(record) = inner.memory.get(&inner.engine, agent2.as_str(), id).map_err(storage)? {
                        out.push(HydratedRecord {
                            id: id.clone(),
                            text: record.content,
                            layer: MemoryLayer::from_table(&record.layer)?,
                        });
                    }
                }
                Ok(out)
            })
            .await?;
        let touched: Vec<String> = out.iter().map(|r| r.id.clone()).collect();
        self.with_inner(move |inner| inner.touch(&agent, &touched, now)).await?;
        Ok(out)
    }

    async fn invalidate(&self, agent: &AgentId, id: &str, now: i64) -> Result<()> {
        let (agent, id) = (agent.clone(), id.to_string());
        self.with_inner(move |inner| {
            let NativeInner { engine, memory, .. } = &mut *inner;
            // Parité UPDATE : no-op silencieux si absent / autre agent.
            if let Some(mut record) = memory.get(engine, agent.as_str(), &id).map_err(storage)? {
                record.valid_until = Some(now);
                memory.update(engine, agent.as_str(), &id, &record).map_err(storage)?;
            }
            Ok(())
        })
        .await
    }

    async fn forget(&self, agent: &AgentId, id: &str) -> Result<()> {
        let (agent, id) = (agent.clone(), id.to_string());
        self.with_inner(move |inner| {
            let NativeInner {
                engine,
                vectors,
                memory,
                fts,
                ..
            } = &mut *inner;
            // Parité DELETE : no-op silencieux si absent (bool ignoré).
            memory
                .forget(engine, vectors, fts, agent.as_str(), &id)
                .map_err(storage)?;
            Ok(())
        })
        .await
    }

    async fn purge_agent(&self, agent: &AgentId) -> Result<()> {
        let agent = agent.clone();
        self.with_inner(move |inner| {
            let NativeInner {
                engine,
                vectors,
                memory,
                graph,
                fts,
            } = &mut *inner;
            memory
                .purge_agent(engine, vectors, fts, agent.as_str())
                .map_err(storage)?;
            graph.purge_agent(engine, agent.as_str()).map_err(storage)?;
            Ok(())
        })
        .await
    }

    async fn agent_stats(&self, agent: &AgentId, now: i64) -> Result<AgentStats> {
        let agent = agent.clone();
        // Lecture pure — verrou de lecture partagé (N5.5).
        self.with_inner_read(move |inner| {
            let NativeInner { engine, memory, .. } = inner;
            let mut stats = AgentStats::default();
            for (_, record) in memory.scan_agent(engine, agent.as_str()).map_err(storage)? {
                if !record_valid_at(&record, now) {
                    continue;
                }
                // Parité GROUP BY : une couche inconnue est ignorée, jamais
                // une erreur.
                match record.layer.as_str() {
                    "short_term" => stats.short_term += 1,
                    "episodic" => stats.episodic += 1,
                    "procedural" => stats.procedural += 1,
                    "semantic" => stats.semantic += 1,
                    _ => {}
                }
            }
            Ok(stats)
        })
        .await
    }

    async fn graph_upsert_entity(
        &self,
        agent: &AgentId,
        id: &str,
        kind: &str,
        label: &str,
        validity: Validity,
    ) -> Result<()> {
        let (agent, id) = (agent.clone(), id.to_string());
        let entity = basemyai_engine::GraphEntity {
            kind: kind.to_string(),
            label: label.to_string(),
            valid_from: validity.valid_from,
            valid_until: validity.valid_until,
        };
        self.with_inner(move |inner| {
            let NativeInner { engine, graph, .. } = &mut *inner;
            // Parité ON CONFLICT DO UPDATE SET kind/label/valid_* :
            // écrasement complet.
            graph
                .upsert_entity(engine, agent.as_str(), &id, entity)
                .map_err(storage)
        })
        .await
    }

    async fn graph_upsert_edge(
        &self,
        agent: &AgentId,
        src: &str,
        relation: &str,
        dst: &str,
        weight: f64,
        now: i64,
    ) -> Result<()> {
        let (agent, src, relation, dst) = (agent.clone(), src.to_string(), relation.to_string(), dst.to_string());
        self.with_inner(move |inner| {
            let NativeInner { engine, graph, .. } = &mut *inner;
            // Parité ON CONFLICT DO UPDATE SET weight : une arête existante
            // garde sa fenêtre de validité, seule `weight` bouge.
            let meta = match graph
                .edge_meta(engine, agent.as_str(), &src, &relation, &dst)
                .map_err(storage)?
            {
                Some(existing) => basemyai_engine::GraphEdgeMeta { weight, ..existing },
                None => basemyai_engine::GraphEdgeMeta {
                    weight,
                    valid_from: now,
                    valid_until: None,
                },
            };
            graph
                .upsert_edge(engine, agent.as_str(), &src, &relation, &dst, meta)
                .map_err(storage)
        })
        .await
    }

    async fn graph_traverse(&self, agent: &AgentId, start: &str, max_depth: u32, now: i64) -> Result<Vec<Reached>> {
        let (agent, start) = (agent.clone(), start.to_string());
        // Lecture pure — verrou de lecture partagé (N5.5).
        self.with_inner_read(move |inner| {
            let NativeInner { engine, graph, .. } = inner;
            Ok(graph
                .traverse(engine, agent.as_str(), &start, max_depth, now)
                .map_err(storage)?
                .into_iter()
                .map(|r| Reached {
                    id: r.id,
                    kind: r.kind,
                    label: r.label,
                    depth: r.depth,
                })
                .collect())
        })
        .await
    }

    async fn recent_episodes(&self, agent: &AgentId, limit: usize, now: i64) -> Result<Vec<String>> {
        let agent = agent.clone();
        // Lecture pure — verrou de lecture partagé (N5.5).
        self.with_inner_read(move |inner| {
            let NativeInner { engine, memory, .. } = inner;
            let mut episodes: Vec<basemyai_engine::MemoryRecord> = memory
                .scan_agent(engine, agent.as_str())
                .map_err(storage)?
                .into_iter()
                .map(|(_, record)| record)
                .filter(|record| record.layer == MemoryLayer::Episodic.table() && record_valid_at(record, now))
                .collect();
            // Parité ORDER BY valid_from DESC LIMIT (tri stable : les
            // ex-æquo restent en ordre d'id, l'ordre du scan structurel).
            episodes.sort_by_key(|record| std::cmp::Reverse(record.valid_from));
            episodes.truncate(limit);
            Ok(episodes.into_iter().map(|record| record.content).collect())
        })
        .await
    }

    async fn exact_fact_exists(&self, agent: &AgentId, content: &str) -> Result<bool> {
        let (agent, content) = (agent.clone(), content.to_string());
        // Lecture pure — verrou de lecture partagé (N5.5).
        self.with_inner_read(move |inner| {
            let NativeInner { engine, memory, .. } = inner;
            // Parité : pas de filtre de validité (l'original n'en a pas).
            Ok(memory
                .scan_agent(engine, agent.as_str())
                .map_err(storage)?
                .into_iter()
                .any(|(_, record)| record.layer == MemoryLayer::Semantic.table() && record.content == content))
        })
        .await
    }
}

/// [`NewMemory`] possédé — le pont `spawn_blocking` exige des closures
/// `'static`, or `NewMemory` emprunte texte/vecteur/source à l'appelant.
struct OwnedNewMemory {
    id: String,
    layer: MemoryLayer,
    text: String,
    validity: Validity,
    vector: Vec<f32>,
    source: String,
}

impl OwnedNewMemory {
    /// Vue empruntée, pour repasser par l'API commune [`NativeInner::put_one`].
    fn borrowed(&self) -> NewMemory<'_> {
        NewMemory {
            id: self.id.clone(),
            layer: self.layer,
            text: &self.text,
            validity: self.validity,
            vector: &self.vector,
            source: &self.source,
        }
    }
}

fn owned(item: &NewMemory<'_>) -> OwnedNewMemory {
    OwnedNewMemory {
        id: item.id.clone(),
        layer: item.layer,
        text: item.text.to_string(),
        validity: item.validity,
        vector: item.vector.to_vec(),
        source: item.source.to_string(),
    }
}
