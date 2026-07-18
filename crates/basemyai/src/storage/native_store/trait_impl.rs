// SPDX-License-Identifier: BUSL-1.1
//! Implémentation du trait [`MemoryStore`] pour [`NativeMemoryStore`] — la
//! surface publique que `crate::memory::Memory` consomme. Voir le doc de
//! module de [`super`] pour la politique de requête (validité, oversampling,
//! parité comportementale) et le pont sync↔async.

use basemyai_core::Metric;

use super::{NativeInner, NativeMemoryStore, OVERSAMPLE, record_valid_at, storage};
use crate::cognition::Reached;
use crate::storage::{HydratedRecord, MemoryStore, NewMemory};
use crate::temporal::Validity;
use crate::{AgentId, AgentStats, MemoryLayer, Record, Result};

/// [`NewMemory`] possédé — le pont `spawn_blocking` exige des closures
/// `'static`, or `NewMemory` emprunte texte/vecteur/source à l'appelant.
struct OwnedNewMemory {
    id: String,
    layer: MemoryLayer,
    text: String,
    validity: Validity,
    vector: Vec<f32>,
    source: String,
    importance: f64,
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
            importance: self.importance,
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
        importance: item.importance,
    }
}

#[async_trait::async_trait]
impl MemoryStore for NativeMemoryStore {
    async fn layer_of(&self, agent: &AgentId, id: &str) -> Result<Option<MemoryLayer>> {
        let (agent, id) = (agent.clone(), id.to_string());
        // Lecture pure — verrou de lecture partagé (N5.5).
        self.with_inner_read(move |inner| {
            match inner.memory.get(&inner.engine, agent.as_str(), &id).map_err(storage)? {
                Some(record) => Ok(Some(MemoryLayer::from_table(&record.layer)?)),
                None => Ok(None),
            }
        })
        .await
    }

    async fn list_memories(
        &self,
        agent: &AgentId,
        layer: Option<MemoryLayer>,
        limit: usize,
        include_invalid: bool,
        now: i64,
    ) -> Result<Vec<crate::storage::ListedRecord>> {
        let agent = agent.clone();
        // Lecture pure — verrou de lecture partagé (N5.5).
        self.with_inner_read(move |inner| {
            let NativeInner { engine, memory, .. } = inner;
            let mut records: Vec<(String, basemyai_engine::MemoryRecord)> =
                memory.scan_agent(engine, agent.as_str()).map_err(storage)?;
            records.retain(|(_, record)| include_invalid || record_valid_at(record, now));
            if let Some(l) = layer {
                records.retain(|(_, record)| record.layer == l.table());
            }
            // Parité ORDER BY valid_from DESC (tri stable, comme
            // `recent_episodes`) : le scan structurel ne garantit qu'un ordre
            // par id, il faut trier explicitement.
            records.sort_by_key(|(_, record)| std::cmp::Reverse(record.valid_from));
            records.truncate(limit);
            records
                .into_iter()
                .map(|(id, record)| {
                    Ok(crate::storage::ListedRecord {
                        id,
                        layer: MemoryLayer::from_table(&record.layer)?,
                        content: record.content,
                        valid_from: record.valid_from,
                        valid_until: record.valid_until,
                    })
                })
                .collect()
        })
        .await
    }

    async fn scan_for_forgetting(
        &self,
        agent: &AgentId,
        now: i64,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<crate::storage::ForgetCandidate>> {
        let agent = agent.clone();
        let after_id = after_id.map(str::to_string);
        // Lecture pure — verrou de lecture partagé (N5.5). Aucun tri par
        // score ici : c'est la politique (`crate::maintenance`) qui décide.
        // Filtre de validité (ADR-038) : seuls les souvenirs actifs à `now`
        // entrent dans la compétition de capacité — un invalidé/expiré
        // n'est jamais un candidat à l'oubli adaptatif (c'est le ressort du
        // GC temporel, `scan_expired`).
        //
        // La pagination brute vient de `scan_agent_page` (ADR-041 §7.3,
        // mémoire O(limit)) ; le filtre de validité s'applique *après*, donc
        // une page brute pleine peut produire moins de `limit` candidats —
        // la boucle re-page alors depuis le dernier id brut examiné, pour
        // que le contrat « page courte ⇔ agent épuisé » reste vrai vu du
        // consommateur (les invalides sautés ne raccourcissent jamais une
        // page).
        self.with_inner_read(move |inner| {
            let NativeInner { engine, memory, .. } = inner;
            let mut out: Vec<crate::storage::ForgetCandidate> = Vec::new();
            if limit == 0 {
                return Ok(out);
            }
            let mut cursor = after_id;
            loop {
                let page = memory
                    .scan_agent_page(engine, agent.as_str(), cursor.as_deref(), limit)
                    .map_err(storage)?;
                let raw_len = page.len();
                let last_raw = page.last().map(|(id, _)| id.clone());
                for (id, record) in page {
                    if record_valid_at(&record, now) {
                        out.push(crate::storage::ForgetCandidate {
                            id,
                            importance: record.importance,
                            last_access: record.last_access,
                        });
                        if out.len() == limit {
                            // Les bruts restants (> dernier candidat renvoyé)
                            // seront relus à l'appel suivant via le curseur.
                            return Ok(out);
                        }
                    }
                }
                if raw_len < limit {
                    return Ok(out);
                }
                cursor = last_raw;
            }
        })
        .await
    }

    async fn scan_expired(
        &self,
        agent: &AgentId,
        now: i64,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<crate::storage::ExpiredCandidate>> {
        let agent = agent.clone();
        let after_id = after_id.map(str::to_string);
        // Lecture pure — verrou de lecture partagé (N5.5). `scan_expiring`
        // (ADR-041 §7.2) interroge l'index temporel dédié via un vrai
        // range-scan `[agent_prefix, expiry_upper_bound(agent, now))` —
        // décode zéro `MemoryRecord`, saute les blocs SST hors plage : le
        // scan complet par agent qu'ADR-038 documentait comme limitation
        // connue est refermé. Le tri par id + curseur restent en mémoire
        // (contrat public inchangé, `after_id`), mais désormais sur le seul
        // ensemble déjà filtré aux souvenirs expirés — jamais tout l'agent.
        self.with_inner_read(move |inner| {
            let NativeInner { engine, memory, .. } = inner;
            let mut candidates = memory.scan_expiring(engine, agent.as_str(), now).map_err(storage)?;
            candidates.sort_by(|(a_id, _), (b_id, _)| a_id.cmp(b_id));
            let mut out: Vec<crate::storage::ExpiredCandidate> = candidates
                .into_iter()
                .filter(|(id, _)| after_id.as_deref().is_none_or(|cursor| id.as_str() > cursor))
                .map(|(id, valid_until)| crate::storage::ExpiredCandidate { id, valid_until })
                .collect();
            out.truncate(limit);
            Ok(out)
        })
        .await
    }

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
    ) -> Result<()> {
        let agent = agent.clone();
        let item = OwnedNewMemory {
            id: id.to_string(),
            layer,
            text: text.to_string(),
            validity,
            vector: vector.to_vec(),
            source: source.to_string(),
            importance,
        };
        self.with_inner(move |inner| inner.put_one(&agent, &item.borrowed()))
            .await
    }

    async fn set_importance(&self, agent: &AgentId, id: &str, importance: f64) -> Result<()> {
        let (agent, id) = (agent.clone(), id.to_string());
        self.with_inner(move |inner| {
            let NativeInner { engine, memory, .. } = &mut *inner;
            // Parité UPDATE : no-op silencieux si absent / autre agent —
            // même discipline que `invalidate`.
            if let Some(mut record) = memory.get(engine, agent.as_str(), &id).map_err(storage)? {
                record.importance = importance;
                memory.update(engine, agent.as_str(), &id, &record).map_err(storage)?;
            }
            Ok(())
        })
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
        include_procedural: bool,
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
            .with_inner_read(move |inner| inner.search_filtered(&agent2, &query2, k, layer, now, include_procedural))
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
                    source: record.source,
                    validity: Validity {
                        valid_from: record.valid_from,
                        valid_until: record.valid_until,
                    },
                })
            })
            .collect()
    }

    async fn recall_graph_filtered(
        &self,
        agent: &AgentId,
        query: &[f32],
        k: usize,
        now: i64,
        include_procedural: bool,
    ) -> Result<Vec<Record>> {
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
                let candidates = inner.search_filtered(
                    &agent2,
                    &query2,
                    k.saturating_mul(OVERSAMPLE),
                    None,
                    now,
                    include_procedural,
                )?;
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
                    source: record.source,
                    validity: Validity {
                        valid_from: record.valid_from,
                        valid_until: record.valid_until,
                    },
                })
            })
            .collect()
    }

    async fn vector_ranking_ids(
        &self,
        agent: &AgentId,
        query: &[f32],
        k: usize,
        now: i64,
        include_procedural: bool,
    ) -> Result<Vec<String>> {
        let (agent, query) = (agent.clone(), query.to_vec());
        // Lecture pure — aucun `touch` (parité avec l'original, ADR-027
        // §6) — verrou de lecture partagé (N5.5).
        self.with_inner_read(move |inner| {
            Ok(inner
                .search_filtered(&agent, &query, k, None, now, include_procedural)?
                .into_iter()
                .map(|(id, _, _)| id)
                .collect())
        })
        .await
    }

    async fn keyword_ranking_ids(
        &self,
        agent: &AgentId,
        match_expr: &str,
        k: usize,
        now: i64,
        include_procedural: bool,
    ) -> Result<Vec<String>> {
        let (agent, match_expr) = (agent.clone(), match_expr.to_string());
        // Lecture pure — verrou de lecture partagé (N5.5).
        self.with_inner_read(move |inner| {
            let NativeInner {
                engine, memory, fts, ..
            } = inner;
            // Le moteur (`search_bm25`) est agnostique de la validité
            // temporelle (mécanisme au moteur, sens au consommateur) ; on
            // sur-échantillonne donc comme le chemin vectoriel (ADR-012) pour
            // ne pas sous-compter après le filtre — oversampling ADR-012
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
                if !include_procedural && record.layer == MemoryLayer::Procedural.table() {
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
                            source: record.source,
                            validity: Validity {
                                valid_from: record.valid_from,
                                valid_until: record.valid_until,
                            },
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

    async fn forget_many(
        &self,
        agent: &AgentId,
        ids: &[String],
        options: crate::storage::ForgetBatchOptions,
    ) -> Result<u64> {
        if ids.is_empty() {
            return Ok(0);
        }
        let (agent, ids) = (agent.clone(), ids.to_vec());
        self.with_inner(move |inner| {
            let NativeInner {
                engine,
                vectors,
                memory,
                fts,
                ..
            } = &mut *inner;
            let borrowed: Vec<&str> = ids.iter().map(String::as_str).collect();
            memory
                .forget_many(
                    engine,
                    vectors,
                    fts,
                    agent.as_str(),
                    &borrowed,
                    basemyai_engine::ForgetBatchOptions {
                        max_items: options.max_items,
                        max_wal_bytes: options.max_wal_bytes,
                    },
                )
                .map_err(storage)
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
            // Parité upsert entité : kind/label/valid_* préservés si l'id existe.
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
            // Parité upsert arête : seul le poids est mis à jour si l'arête existe.
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

    async fn exact_fact_exists(&self, agent: &AgentId, content: &str, at: i64) -> Result<bool> {
        let (agent, content) = (agent.clone(), content.to_string());
        // Lecture pure — verrou de lecture partagé (N5.5).
        self.with_inner_read(move |inner| {
            let NativeInner { engine, memory, .. } = inner;
            Ok(memory
                .scan_agent(engine, agent.as_str())
                .map_err(storage)?
                .into_iter()
                .any(|(_, record)| {
                    record.layer == MemoryLayer::Semantic.table()
                        && record.content == content
                        && record_valid_at(&record, at)
                }))
        })
        .await
    }
}
