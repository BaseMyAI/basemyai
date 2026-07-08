// SPDX-License-Identifier: BUSL-1.1
//! Seule implémentation V1 de [`MemoryStore`] : enveloppe
//! [`basemyai_core::Store`] et concentre tout le SQL/`Filter` brut du crate.
//! Le SQL ici est un **déplacement**, pas une réécriture, des requêtes qui
//! vivaient auparavant dans `memory/mod.rs`, `cognition/graph.rs` et
//! `cognition/consolidation.rs`.

use basemyai_core::libsql::{self, Connection};
use basemyai_core::{Filter, Metric, Store, Value};

use super::{HydratedRecord, MemoryStore, NewMemory};
use crate::cognition::Reached;
use crate::temporal::Validity;
use crate::{AgentId, AgentStats, MemoryLayer, Record, Result};

/// Filtre `WHERE` agent + validité temporelle, commun à tous les recalls.
/// `layer` ajoute une quatrième condition/valeur quand présent.
fn agent_temporal_filter(agent: &AgentId, now: i64, layer: Option<MemoryLayer>) -> Filter {
    match layer {
        Some(l) => Filter::new(
            "agent_id = ? AND valid_from <= ? AND (valid_until IS NULL OR valid_until > ?) AND layer = ?",
            vec![
                Value::Text(agent.as_str().to_string()),
                Value::Integer(now),
                Value::Integer(now),
                Value::Text(l.table().to_string()),
            ],
        ),
        None => Filter::new(
            "agent_id = ? AND valid_from <= ? AND (valid_until IS NULL OR valid_until > ?)",
            vec![
                Value::Text(agent.as_str().to_string()),
                Value::Integer(now),
                Value::Integer(now),
            ],
        ),
    }
}

/// Moteur de stockage libSQL — V1 unique, ADR-011.
pub struct LibsqlMemoryStore {
    store: Store,
}

impl LibsqlMemoryStore {
    /// Enveloppe un [`Store`] déjà ouvert (et migré) dans un moteur mémoire.
    #[must_use]
    pub fn new(store: Store) -> Self {
        Self { store }
    }

    /// Store libSQL sous-jacent. `pub(crate)` : réservé à `memory::porting`
    /// (export/import JSONL), qui a besoin de colonnes (`importance`,
    /// `last_access`) hors du contrat sémantique [`MemoryStore`] et reste, à
    /// ce titre, couplé au backend concret V1 — au même titre que
    /// `maintenance::{gc, forgetting}` (suivi possible si un second backend
    /// apparaît un jour, pas une régression introduite par ce refactor).
    pub(crate) fn store(&self) -> &Store {
        &self.store
    }

    /// Couche d'un souvenir par id + agent, ou `None` si absent (ou appartenant
    /// à un autre agent — la requête est bornée par `agent_id`). `pub(crate)` :
    /// la façade [`Memory`](crate::Memory) s'en sert pour étiqueter les
    /// événements `Invalidated`/`Forgotten` de la bonne couche, et n'émet que si
    /// un souvenir existe réellement pour cet agent (pas sur un no-op cross-agent).
    pub(crate) async fn layer_of(&self, agent: &AgentId, id: &str) -> Result<Option<MemoryLayer>> {
        let conn = self.store.reader();
        let mut rows = conn
            .query(
                "SELECT layer FROM memory WHERE id = ?1 AND agent_id = ?2 LIMIT 1",
                libsql::params![id, agent.as_str()],
            )
            .await
            .map_err(storage)?;
        match rows.next().await.map_err(storage)? {
            Some(row) => {
                let layer_str: String = row.get(0).map_err(storage)?;
                Ok(Some(MemoryLayer::from_table(&layer_str)?))
            }
            None => Ok(None),
        }
    }

    /// `(content, layer)` d'un souvenir par id + agent, ou `None` si absent.
    async fn query_row_content_layer(
        &self,
        conn: &Connection,
        agent: &AgentId,
        id: &str,
    ) -> Result<Option<(String, MemoryLayer)>> {
        let mut rows = conn
            .query(
                "SELECT content, layer FROM memory WHERE id = ?1 AND agent_id = ?2",
                libsql::params![id, agent.as_str()],
            )
            .await
            .map_err(storage)?;
        match rows.next().await.map_err(storage)? {
            Some(row) => {
                let content: String = row.get(0).map_err(storage)?;
                let layer_str: String = row.get(1).map_err(storage)?;
                Ok(Some((content, MemoryLayer::from_table(&layer_str)?)))
            }
            None => Ok(None),
        }
    }

    /// Hydrate des [`basemyai_core::Neighbor`] en [`Record`] (score = distance),
    /// puis marque `last_access` — brique commune de `recall_vector` et
    /// `recall_graph_filtered`.
    async fn hydrate_neighbors(
        &self,
        agent: &AgentId,
        neighbors: Vec<basemyai_core::Neighbor>,
        now: i64,
    ) -> Result<Vec<Record>> {
        let conn = self.store.connect();
        let mut out = Vec::with_capacity(neighbors.len());
        for n in &neighbors {
            if let Some((content, layer)) = self.query_row_content_layer(&conn, agent, &n.id).await? {
                out.push(Record {
                    id: n.id.clone(),
                    text: content,
                    layer,
                    score: n.distance,
                });
            }
        }
        touch_last_access(&conn, out.iter().map(|r| r.id.as_str()), now).await?;
        Ok(out)
    }
}

/// Marque `last_access = now` pour chaque id, sur la connexion fournie.
async fn touch_last_access<'a>(conn: &Connection, ids: impl Iterator<Item = &'a str>, now: i64) -> Result<()> {
    for id in ids {
        conn.execute(
            "UPDATE memory SET last_access = ?1 WHERE id = ?2",
            libsql::params![now, id.to_string()],
        )
        .await
        .map_err(storage)?;
    }
    Ok(())
}

#[async_trait::async_trait]
impl MemoryStore for LibsqlMemoryStore {
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
        let txn = self.store.begin_write().await?;
        insert_memory_row(&txn, id, agent.as_str(), layer, text, validity, vector, source).await?;
        txn.commit().await?;
        Ok(())
    }

    async fn put_memory_batch(&self, agent: &AgentId, items: &[NewMemory<'_>]) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        let txn = self.store.begin_write().await?;
        for item in items {
            insert_memory_row(
                &txn,
                &item.id,
                agent.as_str(),
                item.layer,
                item.text,
                item.validity,
                item.vector,
                item.source,
            )
            .await?;
        }
        txn.commit().await?;
        Ok(())
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
        let filter = agent_temporal_filter(agent, now, layer);
        let neighbors = self
            .store
            .vector_knn_metric("memory", query, k, Some(&filter), metric)
            .await?;
        self.hydrate_neighbors(agent, neighbors, now).await
    }

    async fn recall_graph_filtered(&self, agent: &AgentId, query: &[f32], k: usize, now: i64) -> Result<Vec<Record>> {
        let filter = Filter::new(
            "agent_id = ? AND valid_from <= ? AND (valid_until IS NULL OR valid_until > ?) \
             AND EXISTS (\
               SELECT 1 FROM entity \
               WHERE entity.agent_id = ? \
                 AND (entity.valid_until IS NULL OR entity.valid_until > ?) \
                 AND instr(content, entity.label) > 0\
             )",
            vec![
                Value::Text(agent.as_str().to_string()),
                Value::Integer(now),
                Value::Integer(now),
                Value::Text(agent.as_str().to_string()),
                Value::Integer(now),
            ],
        );
        let neighbors = self.store.vector_knn("memory", query, k, Some(&filter)).await?;
        self.hydrate_neighbors(agent, neighbors, now).await
    }

    async fn vector_ranking_ids(&self, agent: &AgentId, query: &[f32], k: usize, now: i64) -> Result<Vec<String>> {
        let filter = agent_temporal_filter(agent, now, None);
        let neighbors = self.store.vector_knn("memory", query, k, Some(&filter)).await?;
        Ok(neighbors.into_iter().map(|n| n.id).collect())
    }

    async fn keyword_ranking_ids(&self, agent: &AgentId, match_expr: &str, k: usize, now: i64) -> Result<Vec<String>> {
        let conn = self.store.reader();
        let mut rows = conn
            .query(
                // FTS5 exige le nom réel de la table dans MATCH/bm25 (pas un alias).
                "SELECT memory_fts.id FROM memory_fts JOIN memory m ON m.id = memory_fts.id \
                 WHERE memory_fts MATCH ?1 AND memory_fts.agent_id = ?2 \
                   AND m.valid_from <= ?3 AND (m.valid_until IS NULL OR m.valid_until > ?3) \
                 ORDER BY bm25(memory_fts) LIMIT ?4",
                libsql::params![match_expr, agent.as_str(), now, i64::try_from(k).unwrap_or(i64::MAX)],
            )
            .await
            .map_err(storage)?;
        let mut ids = Vec::new();
        while let Some(row) = rows.next().await.map_err(storage)? {
            ids.push(row.get::<String>(0).map_err(storage)?);
        }
        Ok(ids)
    }

    async fn hydrate(&self, agent: &AgentId, ids: &[String], now: i64) -> Result<Vec<HydratedRecord>> {
        let conn = self.store.connect();
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some((content, layer)) = self.query_row_content_layer(&conn, agent, id).await? {
                out.push(HydratedRecord {
                    id: id.clone(),
                    text: content,
                    layer,
                });
            }
        }
        touch_last_access(&conn, out.iter().map(|r| r.id.as_str()), now).await?;
        Ok(out)
    }

    async fn invalidate(&self, agent: &AgentId, id: &str, now: i64) -> Result<()> {
        let conn = self.store.connect();
        conn.execute(
            "UPDATE memory SET valid_until = ?1 WHERE id = ?2 AND agent_id = ?3",
            libsql::params![now, id, agent.as_str()],
        )
        .await
        .map_err(storage)?;
        Ok(())
    }

    async fn forget(&self, agent: &AgentId, id: &str) -> Result<()> {
        let txn = self.store.begin_write().await?;
        txn.execute(
            "DELETE FROM memory WHERE id = ?1 AND agent_id = ?2",
            libsql::params![id, agent.as_str()],
        )
        .await
        .map_err(storage)?;
        txn.execute(
            "DELETE FROM memory_fts WHERE id = ?1 AND agent_id = ?2",
            libsql::params![id, agent.as_str()],
        )
        .await
        .map_err(storage)?;
        txn.commit().await?;
        Ok(())
    }

    async fn purge_agent(&self, agent: &AgentId) -> Result<()> {
        let txn = self.store.begin_write().await?;
        // Noms de tables en dur (jamais d'input) ; l'agent passe en paramètre lié.
        // `memory_fts` (miroir BM25) est purgé avec le reste (ADR-014).
        for table in ["memory", "entity", "edge", "memory_fts"] {
            txn.execute(
                &format!("DELETE FROM {table} WHERE agent_id = ?1"),
                libsql::params![agent.as_str()],
            )
            .await
            .map_err(storage)?;
        }
        txn.commit().await?;
        Ok(())
    }

    async fn agent_stats(&self, agent: &AgentId, now: i64) -> Result<AgentStats> {
        let conn = self.store.reader();
        let mut rows = conn
            .query(
                "SELECT layer, COUNT(*) FROM memory \
                 WHERE agent_id = ?1 AND valid_from <= ?2 \
                   AND (valid_until IS NULL OR valid_until > ?2) \
                 GROUP BY layer",
                libsql::params![agent.as_str(), now],
            )
            .await
            .map_err(storage)?;

        let mut stats = AgentStats::default();
        while let Some(row) = rows.next().await.map_err(storage)? {
            let layer_str: String = row.get(0).map_err(storage)?;
            let count: i64 = row.get(1).map_err(storage)?;
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

    async fn graph_upsert_entity(
        &self,
        agent: &AgentId,
        id: &str,
        kind: &str,
        label: &str,
        validity: Validity,
    ) -> Result<()> {
        let conn = self.store.connect();
        conn.execute(
            "INSERT INTO entity (id, agent_id, kind, label, valid_from, valid_until) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(agent_id, id) DO UPDATE SET \
               kind = excluded.kind, label = excluded.label, \
               valid_from = excluded.valid_from, valid_until = excluded.valid_until",
            libsql::params![
                id,
                agent.as_str(),
                kind,
                label,
                validity.valid_from,
                validity.valid_until
            ],
        )
        .await
        .map_err(storage)?;
        Ok(())
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
        let conn = self.store.connect();
        conn.execute(
            "INSERT INTO edge (src, dst, agent_id, relation, weight, valid_from, valid_until) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL) \
             ON CONFLICT(agent_id, src, dst, relation) DO UPDATE SET weight = excluded.weight",
            libsql::params![src, dst, agent.as_str(), relation, weight, now],
        )
        .await
        .map_err(storage)?;
        Ok(())
    }

    async fn graph_traverse(&self, agent: &AgentId, start: &str, max_depth: u32, now: i64) -> Result<Vec<Reached>> {
        let conn = self.store.reader();
        let sql = "\
            WITH RECURSIVE reach(node, depth) AS ( \
                SELECT ?1, 0 \
                UNION \
                SELECT e.dst, r.depth + 1 \
                FROM edge e JOIN reach r ON e.src = r.node \
                WHERE e.agent_id = ?2 \
                  AND (e.valid_until IS NULL OR e.valid_until > ?3) \
                  AND r.depth < ?4 \
            ) \
            SELECT e.id, e.kind, e.label, MIN(r.depth) AS d \
            FROM reach r \
            JOIN entity e ON e.id = r.node \
            WHERE r.node <> ?1 \
              AND e.agent_id = ?2 \
              AND (e.valid_until IS NULL OR e.valid_until > ?3) \
            GROUP BY e.id, e.kind, e.label \
            ORDER BY d, e.id";

        let mut rows = conn
            .query(sql, libsql::params![start, agent.as_str(), now, i64::from(max_depth)])
            .await
            .map_err(storage)?;

        let mut out = Vec::new();
        while let Some(row) = rows.next().await.map_err(storage)? {
            let depth: i64 = row.get(3).map_err(storage)?;
            out.push(Reached {
                id: row.get::<String>(0).map_err(storage)?,
                kind: row.get::<String>(1).map_err(storage)?,
                label: row.get::<String>(2).map_err(storage)?,
                depth: u32::try_from(depth).unwrap_or(u32::MAX),
            });
        }
        Ok(out)
    }

    async fn recent_episodes(&self, agent: &AgentId, limit: usize, now: i64) -> Result<Vec<String>> {
        let conn = self.store.reader();
        let mut rows = conn
            .query(
                "SELECT content FROM memory \
                 WHERE agent_id = ?1 AND layer = 'episodic' \
                   AND valid_from <= ?2 AND (valid_until IS NULL OR valid_until > ?2) \
                 ORDER BY valid_from DESC LIMIT ?3",
                libsql::params![agent.as_str(), now, i64::try_from(limit).unwrap_or(i64::MAX)],
            )
            .await
            .map_err(storage)?;

        let mut out = Vec::new();
        while let Some(row) = rows.next().await.map_err(storage)? {
            out.push(row.get::<String>(0).map_err(storage)?);
        }
        Ok(out)
    }

    async fn exact_fact_exists(&self, agent: &AgentId, content: &str) -> Result<bool> {
        let conn = self.store.reader();
        let mut rows = conn
            .query(
                "SELECT 1 FROM memory \
                 WHERE agent_id = ?1 AND layer = 'semantic' AND content = ?2 LIMIT 1",
                libsql::params![agent.as_str(), content],
            )
            .await
            .map_err(storage)?;
        Ok(rows.next().await.map_err(storage)?.is_some())
    }
}

/// Mappe une erreur libSQL en [`crate::MemoryError`] (via `CoreError::Storage`).
fn storage(e: libsql::Error) -> crate::MemoryError {
    basemyai_core::CoreError::Storage(e.to_string()).into()
}

/// Insère un souvenir (`memory` + miroir FTS, ADR-014) sur la connexion
/// fournie — une [`basemyai_core::WriteTxn`] en pratique, pour que les deux
/// écritures soient atomiques. `source` trace la provenance (`'user'` direct,
/// `'consolidation'` promu par le pipeline LLM, ADR-018 / audit sécurité).
#[allow(clippy::too_many_arguments)]
async fn insert_memory_row(
    conn: &Connection,
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
