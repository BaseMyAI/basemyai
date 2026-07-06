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
//! Écarts assumés, actés par ADR-027 §6 : `put_memory_batch` est atomique
//! **par item** (pas tout-ou-rien), `purge_agent` est idempotent/reprennable
//! (pas globalement atomique). Les métriques non-cosinus retournent une
//! **erreur franche** (N5.3) — jamais un faux résultat.
//!
//! ## Pont sync↔async (ADR-027 §5)
//!
//! Le moteur est sync mono-écrivain ; le trait est async. Chaque méthode
//! s'exécute dans `tokio::task::spawn_blocking`, le verrou pris à
//! l'intérieur de la closure bloquante — jamais tenu à travers un `.await`
//! (lint `await_holding_lock`). Mono-écrivain sérialisé assumé jusqu'à la
//! barre de concurrence N5.5.

use std::path::Path;
use std::sync::{Arc, Mutex};

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
pub struct NativeMemoryStore {
    inner: Arc<Mutex<NativeInner>>,
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
        let mut engine = Engine::open(path).map_err(storage)?;
        let params = basemyai_engine::VectorIndexParams::with_dim(crate::EMBEDDING_DIM);
        let vectors = PersistentVectorIndex::open(&mut engine, params).map_err(storage)?;
        let memory = PersistentMemoryIndex::open(&engine).map_err(storage)?;
        Ok(Self {
            inner: Arc::new(Mutex::new(NativeInner {
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

    /// Exécute `f` sur l'état natif dans le pool bloquant de tokio, verrou
    /// pris à l'intérieur de la closure (jamais à travers un `.await`).
    async fn with_inner<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut NativeInner) -> Result<T> + Send + 'static,
    {
        let inner = Arc::clone(&self.inner);
        tokio::task::spawn_blocking(move || {
            let mut guard = inner.lock().map_err(|_| storage("verrou du store natif empoisonné"))?;
            f(&mut guard)
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
        &mut self,
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
        let Self {
            engine,
            vectors,
            memory,
            fts,
            ..
        } = self;
        memory
            .put(
                engine,
                vectors,
                fts,
                agent.as_str(),
                &item.id,
                &NewMemoryRecord {
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
        // Écart assumé (ADR-027 §6) : atomique par item, pas tout-ou-rien.
        let agent = agent.clone();
        let owned_items: Vec<OwnedNewMemory> = items.iter().map(owned).collect();
        self.with_inner(move |inner| {
            for item in &owned_items {
                inner.put_one(&agent, &item.borrowed())?;
            }
            Ok(())
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
        self.with_inner(move |inner| {
            let found = inner.search_filtered(&agent, &query, k, layer, now)?;
            let ids: Vec<String> = found.iter().map(|(id, _, _)| id.clone()).collect();
            inner.touch(&agent, &ids, now)?;
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
        })
        .await
    }

    async fn recall_graph_filtered(&self, agent: &AgentId, query: &[f32], k: usize, now: i64) -> Result<Vec<Record>> {
        let (agent, query) = (agent.clone(), query.to_vec());
        self.with_inner(move |inner| {
            // Les labels des entités valides de l'agent (comme l'EXISTS
            // original, seule `valid_until` gate la visibilité d'une entité).
            let labels: Vec<String> = {
                let NativeInner { engine, graph, .. } = &mut *inner;
                graph
                    .entities(engine, agent.as_str())
                    .map_err(storage)?
                    .into_iter()
                    .filter(|(_, e)| e.valid_until.is_none_or(|until| until > now))
                    .map(|(_, e)| e.label)
                    .collect()
            };
            // Oversample large puis filtre « le contenu mentionne un label »
            // (l'`instr(content, entity.label) > 0` original).
            let candidates = inner.search_filtered(&agent, &query, k.saturating_mul(OVERSAMPLE), None, now)?;
            let found: Vec<(String, basemyai_engine::MemoryRecord, f32)> = candidates
                .into_iter()
                .filter(|(_, record, _)| labels.iter().any(|label| record.content.contains(label.as_str())))
                .take(k)
                .collect();
            let ids: Vec<String> = found.iter().map(|(id, _, _)| id.clone()).collect();
            inner.touch(&agent, &ids, now)?;
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
        })
        .await
    }

    async fn vector_ranking_ids(&self, agent: &AgentId, query: &[f32], k: usize, now: i64) -> Result<Vec<String>> {
        let (agent, query) = (agent.clone(), query.to_vec());
        self.with_inner(move |inner| {
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
        self.with_inner(move |inner| {
            let NativeInner {
                engine, memory, fts, ..
            } = &mut *inner;
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
        self.with_inner(move |inner| {
            let mut out = Vec::with_capacity(ids.len());
            {
                let NativeInner { engine, memory, .. } = &mut *inner;
                for id in &ids {
                    // Parité : pas de filtre de validité ici (l'original n'en
                    // a pas) ; un id absent ou d'un autre agent est omis.
                    if let Some(record) = memory.get(engine, agent.as_str(), id).map_err(storage)? {
                        out.push(HydratedRecord {
                            id: id.clone(),
                            text: record.content,
                            layer: MemoryLayer::from_table(&record.layer)?,
                        });
                    }
                }
            }
            let touched: Vec<String> = out.iter().map(|r| r.id.clone()).collect();
            inner.touch(&agent, &touched, now)?;
            Ok(out)
        })
        .await
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
        self.with_inner(move |inner| {
            let NativeInner { engine, memory, .. } = &mut *inner;
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
        self.with_inner(move |inner| {
            let NativeInner { engine, graph, .. } = &mut *inner;
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
        self.with_inner(move |inner| {
            let NativeInner { engine, memory, .. } = &mut *inner;
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
        self.with_inner(move |inner| {
            let NativeInner { engine, memory, .. } = &mut *inner;
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
