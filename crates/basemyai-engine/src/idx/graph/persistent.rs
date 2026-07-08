// SPDX-License-Identifier: BUSL-1.1
//! KV-persisted graph index (N4, `docs/PLAN-NATIVE-ENGINE.md` §2 "Couche 3 —
//! Graphe natif"): entities and outgoing edges each get their own
//! self-contained KV record under the reserved `idx/graph/` keyspace
//! ([`crate::key::graph_index`]) — isolation by agent is **structural** (the
//! key layout itself), never an applicative filter bolted on after a
//! broader read, the same way [`super::super::vector`] scopes itself under
//! its own reserved prefix.
//!
//! ## No metadata record — and why that's not an oversight
//!
//! Unlike [`super::super::vector::persistent::PersistentVectorIndex`], this
//! index persists **no** metadata record at all: no entry point, no count,
//! no epoch, nothing that a corrupt/missing record could force a rebuild
//! from. Two reasons the vector index's rebuild machinery has no graph
//! analogue:
//!
//! 1. **No global navigation state to cache.** Every vector search needs a
//!    fixed entry point to start its greedy walk from — computing "some
//!    reasonable starting node" from scratch on every search would be
//!    wasteful, so that choice is cached and must be recoverable if lost. A
//!    graph traversal is handed its start node explicitly by the caller on
//!    every call ([`PersistentGraph::traverse`]'s `start` parameter) — there
//!    is no analogous cached choice to lose or corrupt.
//! 2. **No derived structure to desync from the data.** The vector index's
//!    neighbor lists are a *build artifact* (the Vamana graph) computed from
//!    the vectors — exactly the kind of thing that can drift from its
//!    ground truth and need reconstructing. Here, the entity and edge
//!    records ARE the data, not a derived index layered on top of it; a
//!    traversal just walks them directly via [`crate::store::Engine::get`]/
//!    `scan_prefix`. A missing entity or edge behaves exactly as it would on
//!    the very first read of a fresh store — simply "absent" — and a
//!    present-but-corrupt block surfaces a hard decode error rather than
//!    being silently dropped (these are memories); either way there is no
//!    separate index generation that could be stale *relative to* the data,
//!    because there is no separate index generation at all.
//!
//! ## Atomicity
//!
//! [`PersistentGraph::upsert_entity`] and [`PersistentGraph::upsert_edge`]
//! each write exactly **one** KV record via a plain [`crate::store::Engine::put`].
//! `Engine::put` is already durable and atomic per key (WAL-fsync-then-
//! memtable, see `store::engine`'s own doc) — wrapping a single `put` in a
//! `Batch`/`apply_batch` would add nothing here. This is the one structural
//! difference from the vector index's inserts, which always touch multiple
//! records (the new node plus every re-pruned neighbor plus shared
//! metadata) and therefore need `apply_batch`'s all-or-nothing guarantee; a
//! graph upsert never touches more than the one record it names.
//! [`PersistentGraph::traverse`] is read-only.

use super::edge::{self, GraphEdgeMeta};
use super::entity::{self, GraphEntity};
use super::traverse::{self, GraphProvider, OutEdge, Reached};
use crate::error::{EngineError, Result};
use crate::key::graph_index;
use crate::store::Engine;

/// Stateless handle over the KV-persisted graph index — holds no cached
/// state at all (see the module doc for why no metadata record is needed).
/// Exists as a named type for API symmetry with `PersistentVectorIndex` and
/// a stable name for the future `MemoryStore` wiring (N5, currently blocked
/// on this milestone per `docs/TODO-NATIVE-ENGINE.md`).
#[derive(Debug, Default, Clone, Copy)]
pub struct PersistentGraph;

impl PersistentGraph {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Inserts or overwrites the entity `(agent, id)`: one durable
    /// `Engine::put` (see the module doc's atomicity note). Takes a whole
    /// [`GraphEntity`] rather than its individual fields, both to keep the
    /// argument count small and because "kind + label + validity" is
    /// exactly the block this writes — no partial-field update exists.
    pub fn upsert_entity(&self, engine: &mut Engine, agent: &str, id: &str, entity: GraphEntity) -> Result<()> {
        let key = graph_index::entity_key(agent, id)?;
        let value = entity::encode(&entity)?;
        engine.put(key.as_bytes(), &value)
    }

    /// Inserts or overwrites the directed edge `(agent, src) --relation--> dst`:
    /// one durable `Engine::put`. `relation`/`dst` stay separate parameters
    /// (they belong in the *key*, see `key::graph_index`), while the edge's
    /// own attributes travel together as a [`GraphEdgeMeta`].
    pub fn upsert_edge(
        &self,
        engine: &mut Engine,
        agent: &str,
        src: &str,
        relation: &str,
        dst: &str,
        meta: GraphEdgeMeta,
    ) -> Result<()> {
        let key = graph_index::edge_key(agent, src, relation, dst)?;
        let value = edge::encode(&meta);
        engine.put(key.as_bytes(), &value)
    }

    /// Breadth-first traversal — see [`traverse::run`] for the exact,
    /// ported-from-CTE semantics (including the deliberately-preserved
    /// `valid_until`-only visibility gate).
    pub fn traverse(
        &self,
        engine: &Engine,
        agent: &str,
        start: &str,
        max_depth: u32,
        now: i64,
    ) -> Result<Vec<Reached>> {
        let mut provider = EngineGraphProvider { engine };
        traverse::run(&mut provider, agent, start, max_depth, now)
    }

    /// Every entity of `agent`, as `(id, entity)` pairs in ascending id-byte
    /// order (the structural scan order). This is the "list all entities of
    /// an agent" consumer `key::graph_index::entity_agent_prefix` was kept
    /// for — `MemoryStore::recall_graph_filtered` matches memory contents
    /// against every valid entity label (N5.1, ADR-027 §6). A malformed key
    /// inside the reserved keyspace is a hard error, never silently skipped.
    pub fn entities(&self, engine: &Engine, agent: &str) -> Result<Vec<(String, GraphEntity)>> {
        let prefix = graph_index::entity_agent_prefix(agent)?;
        let entries = engine.scan_prefix(&prefix)?;
        let mut out = Vec::with_capacity(entries.len());
        for (key, value) in entries {
            let Some(id) = graph_index::entity_id(prefix.len(), key.as_bytes()) else {
                return Err(EngineError::CorruptGraphEntity {
                    reason: format!("malformed entity key under the (agent={agent:?}) scan prefix"),
                });
            };
            out.push((id, entity::decode(&value)?));
        }
        Ok(out)
    }

    /// The entity block for `(agent, id)`, if any — the point lookup behind
    /// an idempotent import's "already present?" check (ADR-032), public
    /// mirror of the private [`EngineGraphProvider::entity`] read the BFS
    /// traversal uses.
    pub fn entity(&self, engine: &Engine, agent: &str, id: &str) -> Result<Option<GraphEntity>> {
        let key = graph_index::entity_key(agent, id)?;
        let Some(bytes) = engine.get(key.as_bytes())? else {
            return Ok(None);
        };
        Ok(Some(entity::decode(&bytes)?))
    }

    /// Every edge of `agent`, as `(src, relation, dst, meta)` tuples in
    /// ascending key-byte order (the structural scan order) — the
    /// whole-agent enumeration behind exporting an agent's graph (ADR-032),
    /// mirror of [`Self::entities`]. A malformed key inside the reserved
    /// keyspace is a hard error, never silently skipped.
    pub fn edges(&self, engine: &Engine, agent: &str) -> Result<Vec<(String, String, String, GraphEdgeMeta)>> {
        let prefix = graph_index::edge_agent_prefix(agent)?;
        let entries = engine.scan_prefix(&prefix)?;
        let mut out = Vec::with_capacity(entries.len());
        for (key, value) in entries {
            let Some((src, relation, dst)) = graph_index::edge_src_relation_dst(prefix.len(), key.as_bytes()) else {
                return Err(EngineError::CorruptGraphEdge {
                    reason: format!("malformed edge key under the (agent={agent:?}) scan prefix"),
                });
            };
            out.push((src, relation, dst, edge::decode(&value)?));
        }
        Ok(out)
    }

    /// The attributes of the edge `(agent, src) --relation--> dst`, if any —
    /// what a caller needs to reproduce the libSQL upsert's
    /// `ON CONFLICT ... DO UPDATE SET weight` semantics (update the weight,
    /// preserve the original validity window; ADR-027 §6) without this crate
    /// hard-coding that policy.
    pub fn edge_meta(
        &self,
        engine: &Engine,
        agent: &str,
        src: &str,
        relation: &str,
        dst: &str,
    ) -> Result<Option<GraphEdgeMeta>> {
        let key = graph_index::edge_key(agent, src, relation, dst)?;
        let Some(bytes) = engine.get(key.as_bytes())? else {
            return Ok(None);
        };
        Ok(Some(edge::decode(&bytes)?))
    }

    /// Removes **every** entity and edge of `agent`, in one atomic batch
    /// (unlike the memory index's per-item purge, nothing here needs to ride
    /// a vector-index mutation — plain KV deletes suffice). Returns the
    /// number of records removed. A no-op (empty batch skipped) when the
    /// agent has no graph.
    pub fn purge_agent(&self, engine: &mut Engine, agent: &str) -> Result<u64> {
        let mut batch = crate::store::Batch::new();
        for prefix in [
            graph_index::entity_agent_prefix(agent)?,
            graph_index::edge_agent_prefix(agent)?,
        ] {
            for (key, _) in engine.scan_prefix(&prefix)? {
                batch.delete(key.as_bytes());
            }
        }
        if batch.is_empty() {
            return Ok(0);
        }
        let removed = batch.len() as u64;
        engine.apply_batch(&batch)?;
        Ok(removed)
    }
}

/// Read-through provider over `Engine::get`/`scan_prefix` — decodes entity
/// and edge blocks on demand, no caching (unlike the vector index's bounded
/// block cache): a graph traversal's working set is the frontier it's
/// currently expanding, not a hot neighborhood revisited across many
/// queries, so the added complexity of a cache isn't justified here.
struct EngineGraphProvider<'a> {
    engine: &'a Engine,
}

impl GraphProvider for EngineGraphProvider<'_> {
    fn entity(&mut self, agent: &str, id: &str) -> Result<Option<GraphEntity>> {
        let key = graph_index::entity_key(agent, id)?;
        let Some(bytes) = self.engine.get(key.as_bytes())? else {
            return Ok(None);
        };
        Ok(Some(entity::decode(&bytes)?))
    }

    fn out_edges(&mut self, agent: &str, src: &str) -> Result<Vec<OutEdge>> {
        let prefix = graph_index::edge_src_prefix(agent, src)?;
        let entries = self.engine.scan_prefix(&prefix)?;
        let mut out = Vec::with_capacity(entries.len());
        for (key, value) in entries {
            let Some((relation, dst)) = graph_index::edge_relation_dst(prefix.len(), key.as_bytes()) else {
                return Err(EngineError::CorruptGraphEdge {
                    reason: format!("malformed edge key under the (agent={agent:?}, src={src:?}) scan prefix"),
                });
            };
            let meta = edge::decode(&value)?;
            out.push((relation, dst, meta));
        }
        Ok(out)
    }
}
