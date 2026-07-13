// SPDX-License-Identifier: BUSL-1.1
//! KV-persisted LM-DiskANN vector index (ADR-026 §Décision 2/3/5): the
//! Vamana graph of [`super::graph`], stored one-node-one-KV-record inside
//! the Layer-1 [`Engine`] under the reserved `idx/vector/` keyspace
//! ([`crate::key::vector_index`]).
//!
//! ## Crash consistency (the double invariant, ADR-026 §3)
//!
//! - **Atomicity by construction**: every [`PersistentVectorIndex::insert`]
//!   turns the shared planner's [`InsertPlan`] — the new node block, every
//!   neighbor block whose list was re-pruned, and the refreshed metadata
//!   record (entry point, count, epoch) — into **one**
//!   [`Engine::apply_batch`]; every [`PersistentVectorIndex::delete`] does
//!   the same with the tombstoned block + metadata. The WAL's batch framing
//!   (proven under real kills by the N2 harness, extended to this index by
//!   the `vector` mode of `tests/crash_consistency.rs`) guarantees the
//!   whole operation is visible after a crash or none of it is; the index
//!   can never reopen half-linked.
//! - **The data is the single source of truth**: each node block carries
//!   its own vector *and its tombstone flag*, so the graph (neighbor
//!   lists and metadata) is derived state. [`PersistentVectorIndex::open`] verifies
//!   the metadata record and falls back to
//!   [`PersistentVectorIndex::rebuild`] whenever it is absent, corrupt,
//!   version-unsupported, or inconsistent (entry point missing) — vectors
//!   are never lost to an index bug, and tombstoned blocks are *purged*
//!   (not resurrected) by a rebuild. A rebuild first *deletes* the metadata
//!   record (own atomic batch), then rewrites node blocks in chunks, then
//!   writes fresh metadata (epoch + 1) last — so a crash at any point
//!   mid-rebuild leaves "node blocks without valid metadata", which the
//!   next `open` detects and rebuilds again. Rebuild is crash-safe without
//!   being atomic.
//!
//! ## Deletes and consolidation (ADR-026 §4, FreshDiskANN)
//!
//! [`PersistentVectorIndex::delete`] is a *logical* delete: the block is
//! rewritten with its tombstone flag set (one atomic batch with the
//! metadata's live-count decrement). Tombstones stay traversable as routing
//! points but are excluded from [`PersistentVectorIndex::search`] results.
//!
//! [`PersistentVectorIndex::consolidate`] is the explicit FreshDiskANN
//! repair pass (a maintenance operation like `rebuild` — the engine stays
//! sync and single-writer, no background thread): every live node still
//! referencing tombstones is reconnected via robust prune over its live
//! neighbors-of-neighbors ([`graph::plan_repair`]), the entry point is
//! re-anchored if tombstoned, then the tombstoned blocks are physically
//! removed. It is **batch-atomic per slice, crash-safe by ordering**, not
//! globally atomic — a kill mid-consolidation leaves a *valid intermediate
//! state*, in one of three phases:
//!
//! 1. mid-repair: some live nodes repaired, others still referencing
//!    tombstones — exactly the pre-consolidation situation, just further
//!    along; nothing dangles because no block was removed yet;
//! 2. between the metadata re-anchor and the purge: repairs complete, meta
//!    points at a live entry, tombstoned blocks still present — a plain
//!    "tombstones pending" state;
//! 3. mid-purge: some tombstoned blocks removed. By then **no live node
//!    references them** (phase 1 completed first) and the metadata no
//!    longer routes through them; a leftover half of the tombstones is
//!    simply picked up by the next `consolidate` (or purged by `rebuild`).
//!    Even a hypothetical dangling edge is tolerated by construction:
//!    `greedy_search` skips ids whose block is missing.
//!
//! In every phase the reopened index passes `open` cleanly (metadata stays
//! consistent throughout), confirmed-deleted ids never resurface (the flag
//! is in the block, flipped atomically at delete time) and live ids stay
//! reachable — asserted under real kills by the crash harness's interleaved
//! delete/consolidate churn mode.
//!
//! ## Memory profile (the "LM" point of LM-DiskANN)
//!
//! Search and insert read node blocks on demand through `Engine::get` —
//! the graph is never loaded wholesale into RAM. A small bounded
//! read-through cache ([`CACHE_CAP`] decoded blocks, cleared when full —
//! deliberately the simplest possible policy) absorbs the hot neighborhood;
//! a real block-cache/eviction pass is deferred to the parity-bench step
//! (ADR-026 §5/§6). The two exceptions are
//! [`PersistentVectorIndex::rebuild`] and
//! [`PersistentVectorIndex::consolidate`], which scan the whole keyspace —
//! maintenance paths by design, mirroring "cache RAM + flush par batch"
//! (ADR-026 §5).

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use super::graph::{self, NodeProvider, VectorIndex};
use super::meta::{self, VectorIndexMeta, VectorIndexParams};
use super::node::{self, VectorNode};
use crate::error::{EngineError, Result};
use crate::key::vector_index::{META_KEY, NODE_PREFIX, node_id, node_key};
use crate::store::{Batch, Engine};

/// Maximum number of decoded node blocks kept by the read-through cache.
/// At the product-default 384d a block is ~1.8 KiB decoded, so this caps the
/// cache around ~8 MiB. When full it is simply cleared (no eviction policy
/// yet — see the module doc).
const CACHE_CAP: usize = 4096;

/// Number of node blocks per `apply_batch` when a rebuild or a
/// consolidation flushes rewritten/purged blocks back to the store. Bounds
/// the size of a single WAL record; the metadata record is written *after*
/// the last rebuild chunk (see the module doc's crash-safety argument).
const REBUILD_CHUNK: usize = 512;

/// Read-through provider over `Engine::get`: decodes node blocks on demand
/// and caches them (bounded, see [`CACHE_CAP`]). Takes the cache by shared
/// reference to an interior-mutable [`Mutex`] (N5.5, concurrency barre M6):
/// this is what lets [`PersistentVectorIndex::search_scored`] take `&self`
/// instead of `&mut self` — concurrent reads no longer need exclusive
/// access to the index, only to the small cache map for the instant of a
/// hit/insert.
struct EngineProvider<'a> {
    engine: &'a Engine,
    cache: &'a Mutex<HashMap<u64, VectorNode>>,
}

impl NodeProvider for EngineProvider<'_> {
    fn node(&mut self, id: u64) -> Result<Option<VectorNode>> {
        if let Some(hit) = lock_cache(self.cache).get(&id) {
            return Ok(Some(hit.clone()));
        }
        let Some(bytes) = self.engine.get(node_key(id).as_bytes())? else {
            return Ok(None);
        };
        let decoded = node::decode(&bytes)?;
        cache_put(self.cache, id, decoded.clone());
        Ok(Some(decoded))
    }
}

/// Read-through provider with an overlay of **pending** (planned but not
/// yet committed) node blocks — what lets [`PersistentVectorIndex::insert_many_with`]
/// plan insert *i+1* against the state inserts *0..=i* will have produced,
/// while every block still travels in one single uncommitted batch (N5.5,
/// all-or-nothing `put_memory_batch`). Lookup order: pending first (it
/// shadows both the cache and the store — those only know the committed
/// state), then the shared bounded cache, then `Engine::get`. Used only from
/// `&mut self` call sites (an insert is already exclusive), but shares the
/// same [`Mutex`]-backed cache type as [`EngineProvider`] for one shared
/// `cache_put` helper.
struct OverlayProvider<'a> {
    engine: &'a Engine,
    cache: &'a Mutex<HashMap<u64, VectorNode>>,
    pending: &'a HashMap<u64, VectorNode>,
}

impl NodeProvider for OverlayProvider<'_> {
    fn node(&mut self, id: u64) -> Result<Option<VectorNode>> {
        if let Some(hit) = self.pending.get(&id) {
            return Ok(Some(hit.clone()));
        }
        if let Some(hit) = lock_cache(self.cache).get(&id) {
            return Ok(Some(hit.clone()));
        }
        let Some(bytes) = self.engine.get(node_key(id).as_bytes())? else {
            return Ok(None);
        };
        let decoded = node::decode(&bytes)?;
        cache_put(self.cache, id, decoded.clone());
        Ok(Some(decoded))
    }
}

/// Provider over a fully scanned snapshot of the node keyspace — the
/// consolidation pass reads every block once anyway (it must find every
/// tombstone), so repairs run against this in-RAM snapshot instead of
/// re-fetching through `Engine::get`.
struct SnapshotProvider<'a> {
    nodes: &'a HashMap<u64, VectorNode>,
}

impl NodeProvider for SnapshotProvider<'_> {
    fn node(&mut self, id: u64) -> Result<Option<VectorNode>> {
        Ok(self.nodes.get(&id).cloned())
    }
}

/// Locks the bounded cache, recovering the guard even if a prior holder
/// panicked while it was locked (a panic elsewhere must not permanently wedge
/// every future search behind a poisoned mutex — the cache is a pure
/// performance aid, never a source of truth, so continuing with whatever
/// state it holds is always safe).
fn lock_cache(cache: &Mutex<HashMap<u64, VectorNode>>) -> std::sync::MutexGuard<'_, HashMap<u64, VectorNode>> {
    cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Bounded cache insert: clear-when-full, then insert (see [`CACHE_CAP`]).
fn cache_put(cache: &Mutex<HashMap<u64, VectorNode>>, id: u64, node: VectorNode) {
    let mut cache = lock_cache(cache);
    if !cache.contains_key(&id) && cache.len() >= CACHE_CAP {
        cache.clear();
    }
    cache.insert(id, node);
}

/// The KV-persisted vector index. See the module doc for the crash-
/// consistency, delete/consolidation, and memory-profile contracts.
///
/// Methods take the [`Engine`] as an explicit parameter rather than owning
/// it: the engine is the shared Layer-1 store (data records and index live
/// in the same keyspace so they can share atomic batches), and this type is
/// only a view over its `idx/vector/` slice plus a little cached state.
#[derive(Debug)]
pub struct PersistentVectorIndex {
    params: VectorIndexParams,
    entry_point: Option<u64>,
    epoch: u64,
    /// Live (non-tombstoned) vector count — mirrors the persisted
    /// `VectorIndexMeta::count`.
    count: u64,
    /// Whether `open` had to fall back to a rebuild (metadata absent,
    /// corrupt, or inconsistent). `false` on every clean open — the crash
    /// harness asserts exactly that after every kill.
    rebuilt_on_open: bool,
    cache: Mutex<HashMap<u64, VectorNode>>,
}

impl PersistentVectorIndex {
    /// Opens the index stored in `engine`, or initializes an empty one.
    ///
    /// - Fresh store (no metadata, no node blocks): empty index with
    ///   `params`, epoch 0. Nothing is written until the first insert.
    /// - Valid metadata whose entry point resolves: adopts the *stored*
    ///   parameters (they are the ones the on-disk graph was built with;
    ///   the caller's `max_degree`/`beam_width`/`alpha` are ignored in
    ///   favor of the persisted truth), except that a `dim` disagreement
    ///   with `params` is a hard [`EngineError::VectorDimensionMismatch`] —
    ///   the caller is about to feed vectors of the wrong shape.
    /// - Metadata absent-but-nodes-exist, corrupt, version-unsupported, or
    ///   entry point missing: falls back to [`Self::rebuild`] (data is the
    ///   source of truth, ADR-026 §3); [`Self::rebuilt_on_open`] reports it.
    pub fn open(engine: &mut Engine, params: VectorIndexParams) -> Result<Self> {
        match engine.get(META_KEY)? {
            Some(bytes) => match meta::decode(&bytes) {
                Ok(stored) => {
                    if stored.params.dim != params.dim {
                        return Err(EngineError::VectorDimensionMismatch {
                            expected: stored.params.dim,
                            found: params.dim,
                        });
                    }
                    let entry_resolves = match stored.entry_point {
                        Some(entry) => engine.get(node_key(entry).as_bytes())?.is_some(),
                        None => stored.count == 0,
                    };
                    if !entry_resolves {
                        return Self::rebuild_from_store(engine, params, Some(stored.epoch));
                    }
                    Ok(Self {
                        params: stored.params,
                        entry_point: stored.entry_point,
                        epoch: stored.epoch,
                        count: stored.count,
                        rebuilt_on_open: false,
                        cache: Mutex::new(HashMap::new()),
                    })
                }
                Err(
                    EngineError::CorruptVectorIndexMeta { .. } | EngineError::UnsupportedVectorIndexMetaVersion { .. },
                ) => Self::rebuild_from_store(engine, params, None),
                Err(other) => Err(other),
            },
            None => {
                if engine.scan_prefix(NODE_PREFIX)?.is_empty() {
                    Ok(Self {
                        params,
                        entry_point: None,
                        epoch: 0,
                        count: 0,
                        rebuilt_on_open: false,
                        cache: Mutex::new(HashMap::new()),
                    })
                } else {
                    // Node blocks without metadata: a lost/torn meta record
                    // or an interrupted rebuild — reconstruct from the data.
                    Self::rebuild_from_store(engine, params, None)
                }
            }
        }
    }

    /// Whether this handle was produced by a rebuild instead of a clean
    /// metadata load (see [`Self::open`]).
    #[must_use]
    pub fn rebuilt_on_open(&self) -> bool {
        self.rebuilt_on_open
    }

    #[must_use]
    pub fn params(&self) -> &VectorIndexParams {
        &self.params
    }

    /// Number of live vectors in the index (tombstones excluded).
    #[must_use]
    pub fn len(&self) -> u64 {
        self.count
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Current build generation (bumped by every rebuild, never by inserts,
    /// deletes, or consolidations).
    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Inserts `vector` under `id`, durably: the new node block, every
    /// re-pruned neighbor block, and the refreshed metadata record travel in
    /// **one** [`Engine::apply_batch`] — after a crash the whole insert is
    /// visible or none of it is (ADR-026 §3, proven under real kills by the
    /// `vector` mode of `tests/crash_consistency.rs`).
    ///
    /// Errors with [`EngineError::VectorDimensionMismatch`] on a wrong-sized
    /// vector and [`EngineError::DuplicateVectorId`] if `id` is already
    /// **live**. A *tombstoned* `id` is resurrected with the new vector
    /// (update = delete + reinsert; see `graph.rs`'s module doc).
    pub fn insert(&mut self, engine: &mut Engine, id: u64, vector: Vec<f32>) -> Result<()> {
        self.insert_with(engine, id, vector, &Batch::new())
    }

    /// [`Self::insert`], with the caller's companion operations (`extra`)
    /// merged into the **same** atomic batch (ADR-027 §3): the index blocks
    /// and the consumer's own records (e.g. a `MemoryStore` memory record +
    /// its id mapping) commit together or vanish together under a crash —
    /// the native equivalent of the multi-row libSQL transaction this
    /// replaces. An empty `extra` is exactly [`Self::insert`].
    pub fn insert_with(&mut self, engine: &mut Engine, id: u64, vector: Vec<f32>, extra: &Batch) -> Result<()> {
        let plan = {
            let mut provider = EngineProvider {
                engine,
                cache: &self.cache,
            };
            graph::plan_insert(&mut provider, &self.params, self.entry_point, id, vector)?
        };

        let mut batch = Batch::new();
        for (node_id, node) in &plan.changed {
            batch.put(node_key(*node_id).as_bytes(), &node::encode(node)?);
        }
        let new_meta = VectorIndexMeta {
            params: self.params,
            epoch: self.epoch,
            count: self.count + 1,
            entry_point: Some(plan.entry_point),
        };
        batch.put(META_KEY, &meta::encode(&new_meta)?);
        batch.extend_from(extra);
        engine.apply_batch(&batch)?;

        self.count += 1;
        self.entry_point = Some(plan.entry_point);
        for (node_id, node) in plan.changed {
            cache_put(&self.cache, node_id, node);
        }
        Ok(())
    }

    /// Inserts **several** vectors in one single [`Engine::apply_batch`] —
    /// all-or-nothing under a crash for the *whole group*, plus the caller's
    /// companion `extra` batch (N5.5, closing the per-item-atomicity gap
    /// ADR-027 §6 documented for `put_memory_batch`). Insert *i+1* is
    /// planned against the state inserts *0..=i* will have produced, via an
    /// overlay of the pending blocks over the committed store
    /// ([`OverlayProvider`]) — later stagings of the same block key in the
    /// batch overwrite earlier ones, exactly like sequential commits would.
    ///
    /// Any error (dimension mismatch, duplicate id — including a duplicate
    /// *within* `items`) is returned **before anything is written**: the
    /// planning phase touches no disk state, so the all-or-nothing property
    /// holds on the error path too. An empty `items` applies only `extra`
    /// (itself a no-op when empty).
    pub fn insert_many_with(&mut self, engine: &mut Engine, items: Vec<(u64, Vec<f32>)>, extra: &Batch) -> Result<()> {
        if items.is_empty() {
            if !extra.is_empty() {
                engine.apply_batch(extra)?;
            }
            return Ok(());
        }
        let added = items.len() as u64;
        let mut pending: HashMap<u64, VectorNode> = HashMap::new();
        let mut entry_point = self.entry_point;
        let mut batch = Batch::new();
        for (id, vector) in items {
            let plan = {
                let mut provider = OverlayProvider {
                    engine,
                    cache: &self.cache,
                    pending: &pending,
                };
                graph::plan_insert(&mut provider, &self.params, entry_point, id, vector)?
            };
            entry_point = Some(plan.entry_point);
            for (node_id, node) in plan.changed {
                batch.put(node_key(node_id).as_bytes(), &node::encode(&node)?);
                pending.insert(node_id, node);
            }
        }
        let new_meta = VectorIndexMeta {
            params: self.params,
            epoch: self.epoch,
            count: self.count + added,
            entry_point,
        };
        batch.put(META_KEY, &meta::encode(&new_meta)?);
        batch.extend_from(extra);
        engine.apply_batch(&batch)?;

        self.count += added;
        self.entry_point = entry_point;
        for (node_id, node) in pending {
            cache_put(&self.cache, node_id, node);
        }
        Ok(())
    }

    /// Tombstones `id`, durably: the block is rewritten with its tombstone
    /// flag set and the metadata's live count decremented, both in **one**
    /// [`Engine::apply_batch`] — atomically all-or-nothing under a crash,
    /// like every other index mutation. The node keeps routing traffic
    /// (greedy search still walks through it) but never surfaces in
    /// [`Self::search`] results again; [`Self::consolidate`] removes the
    /// block physically.
    ///
    /// Returns `false` (writing nothing) if `id` is absent or already
    /// tombstoned — deletes are idempotent, never an error.
    pub fn delete(&mut self, engine: &mut Engine, id: u64) -> Result<bool> {
        self.delete_with(engine, id, &Batch::new())
    }

    /// [`Self::delete`], with the caller's companion operations (`extra`)
    /// merged into the same atomic batch (ADR-027 §3), mirror of
    /// [`Self::insert_with`].
    ///
    /// One asymmetry with the plain delete's "absent/tombstoned writes
    /// nothing" contract: when `id` is absent or already tombstoned, a
    /// non-empty `extra` is **still applied** (as its own batch). The
    /// caller's companion records must not survive a no-op tombstone — e.g.
    /// a `MemoryStore::forget` whose vector node was already tombstoned by
    /// an earlier interrupted attempt still has to remove the memory record
    /// and its id mapping. The returned bool keeps the plain semantics:
    /// whether *this call* tombstoned the node.
    pub fn delete_with(&mut self, engine: &mut Engine, id: u64, extra: &Batch) -> Result<bool> {
        let existing = {
            let mut provider = EngineProvider {
                engine,
                cache: &self.cache,
            };
            provider.node(id)?
        };
        let live = match existing {
            Some(node) if !node.deleted => Some(node),
            _ => None,
        };
        let Some(node) = live else {
            if !extra.is_empty() {
                engine.apply_batch(extra)?;
            }
            return Ok(false);
        };

        let tombstoned = VectorNode { deleted: true, ..node };
        let mut batch = Batch::new();
        batch.put(node_key(id).as_bytes(), &node::encode(&tombstoned)?);
        let new_meta = VectorIndexMeta {
            params: self.params,
            epoch: self.epoch,
            // saturating: a live node with a zero count would mean drifted
            // metadata — heal downward instead of panicking in lib code.
            count: self.count.saturating_sub(1),
            entry_point: self.entry_point,
        };
        batch.put(META_KEY, &meta::encode(&new_meta)?);
        batch.extend_from(extra);
        engine.apply_batch(&batch)?;

        self.count = self.count.saturating_sub(1);
        cache_put(&self.cache, id, tombstoned);
        Ok(true)
    }

    /// Tombstones **several** ids in one single [`Engine::apply_batch`] —
    /// the deletion sibling of [`Self::insert_many_with`] (ADR-041 §7.4):
    /// every tombstoned block, one single refreshed metadata record (live
    /// count decremented by the whole group) and the caller's companion
    /// `extra` batch commit together or vanish together under a crash.
    ///
    /// Absent or already-tombstoned ids (including duplicates within `ids`)
    /// are skipped, never an error — same idempotence as [`Self::delete`].
    /// And same asymmetry as [`Self::delete_with`]: when **nothing** ends up
    /// tombstoned, a non-empty `extra` is still applied (as its own batch) —
    /// the caller's companion deletes must not survive a no-op tombstone
    /// pass. Returns how many ids *this call* tombstoned.
    pub fn delete_many_with(&mut self, engine: &mut Engine, ids: &[u64], extra: &Batch) -> Result<u64> {
        let mut pending: Vec<(u64, VectorNode)> = Vec::new();
        let mut seen: HashSet<u64> = HashSet::with_capacity(ids.len());
        for &id in ids {
            if !seen.insert(id) {
                continue;
            }
            let existing = {
                let mut provider = EngineProvider {
                    engine,
                    cache: &self.cache,
                };
                provider.node(id)?
            };
            if let Some(node) = existing
                && !node.deleted
            {
                pending.push((id, VectorNode { deleted: true, ..node }));
            }
        }
        if pending.is_empty() {
            if !extra.is_empty() {
                engine.apply_batch(extra)?;
            }
            return Ok(0);
        }

        let removed = pending.len() as u64;
        let mut batch = Batch::new();
        for (id, tombstoned) in &pending {
            batch.put(node_key(*id).as_bytes(), &node::encode(tombstoned)?);
        }
        let new_meta = VectorIndexMeta {
            params: self.params,
            epoch: self.epoch,
            // saturating: same drifted-metadata healing posture as
            // `delete_with`.
            count: self.count.saturating_sub(removed),
            entry_point: self.entry_point,
        };
        batch.put(META_KEY, &meta::encode(&new_meta)?);
        batch.extend_from(extra);
        engine.apply_batch(&batch)?;

        self.count = self.count.saturating_sub(removed);
        for (id, tombstoned) in pending {
            cache_put(&self.cache, id, tombstoned);
        }
        Ok(removed)
    }

    /// Approximate wire bytes one tombstone rewrite stages (node key + the
    /// re-encoded block: vector + up to `max_degree` neighbors + framing) —
    /// the byte-budget estimate behind `forget_many`'s `max_wal_bytes` bound
    /// (ADR-041 §7.4). An upper-ish bound from the index parameters, not a
    /// per-node read: the bound is a batch-sizing target, block-level
    /// precision is not required.
    #[must_use]
    pub fn approx_tombstone_wire_bytes(&self) -> usize {
        node_key(0).as_bytes().len() + self.params.dim * 4 + self.params.max_degree * 8 + 64
    }

    /// Returns the ids of (up to) the `k` approximate nearest **live**
    /// neighbors of `query`, ordered by ascending cosine distance, reading
    /// node blocks on demand through `Engine::get` (never a full load — see
    /// the module doc). Tombstones route but are filtered out of the
    /// results. `&self` (N5.5, concurrency barre M6): the bounded block
    /// cache the read populates is interior-mutable ([`Mutex`]), so a search
    /// no longer needs exclusive access to the index — concurrent searches
    /// (and a search alongside `get`/`scan_prefix`-based reads elsewhere)
    /// can proceed together; only the mutating methods (`insert*`,
    /// `delete*`, `consolidate`, `rebuild`) still need `&mut self`.
    pub fn search(&self, engine: &Engine, query: &[f32], k: usize) -> Result<Vec<u64>> {
        Ok(self
            .search_scored(engine, query, k)?
            .into_iter()
            .map(|(id, _)| id)
            .collect())
    }

    /// [`Self::search`], keeping each result's cosine distance to `query`
    /// (already computed by the greedy walk). This is what the `MemoryStore`
    /// wiring exposes as `Record::score` (ADR-027 §6) without re-reading
    /// node blocks.
    pub fn search_scored(&self, engine: &Engine, query: &[f32], k: usize) -> Result<Vec<(u64, f32)>> {
        if query.len() != self.params.dim {
            return Err(EngineError::VectorDimensionMismatch {
                expected: self.params.dim,
                found: query.len(),
            });
        }
        let Some(entry) = self.entry_point else {
            return Ok(Vec::new());
        };
        if k == 0 {
            return Ok(Vec::new());
        }
        let beam = self.params.beam_width.max(k);
        let mut provider = EngineProvider {
            engine,
            cache: &self.cache,
        };
        graph::search_live_scored(&mut provider, entry, query, beam, k)
    }

    /// FreshDiskANN consolidation (see the module doc): repairs every live
    /// node still referencing a tombstone, re-anchors the entry point if it
    /// is tombstoned, then physically removes every tombstoned block —
    /// batch-atomic per slice, crash-safe by ordering (repairs → metadata →
    /// purge; every intermediate state is a valid index). Returns the
    /// number of tombstoned blocks purged.
    ///
    /// Also self-healing: the live count and entry point are recomputed
    /// from the scanned blocks (the data), so leftovers of an interrupted
    /// earlier consolidation are absorbed.
    pub fn consolidate(&mut self, engine: &mut Engine) -> Result<u64> {
        // 0. One scan of the node keyspace: the pass must find every
        //    tombstone anyway, and the snapshot doubles as the repair
        //    provider (maintenance path — see the module doc).
        let entries = engine.scan_prefix(NODE_PREFIX)?;
        let mut nodes: HashMap<u64, VectorNode> = HashMap::with_capacity(entries.len());
        for (key, value) in &entries {
            let Some(id) = node_id(key.as_bytes()) else {
                return Err(EngineError::CorruptVectorNode {
                    reason: format!(
                        "malformed node key in the idx/vector/node/ keyspace: {:?}",
                        key.as_bytes()
                    ),
                });
            };
            nodes.insert(id, node::decode(value)?);
        }
        let dead: HashSet<u64> = nodes
            .iter()
            .filter_map(|(&id, node)| node.deleted.then_some(id))
            .collect();
        let live_count = (nodes.len() - dead.len()) as u64;

        if dead.is_empty() {
            // Nothing to purge; still true up the in-memory count if a
            // previous crash left it stale (meta already matches the data
            // in every non-buggy path).
            self.count = live_count;
            return Ok(0);
        }

        // 1. Repair phase: plans computed against the pre-repair snapshot
        //    (deterministic, order-independent — sorted ids), flushed in
        //    bounded atomic batches. A kill here leaves some nodes
        //    repaired, some not: a valid graph either way, since no block
        //    has been removed yet.
        let mut live_ids: Vec<u64> = nodes
            .iter()
            .filter_map(|(&id, node)| (!node.deleted).then_some(id))
            .collect();
        live_ids.sort_unstable();
        let mut repaired: Vec<(u64, VectorNode)> = Vec::new();
        {
            let mut provider = SnapshotProvider { nodes: &nodes };
            for &id in &live_ids {
                if let Some(node) = graph::plan_repair(&mut provider, &self.params, id)? {
                    repaired.push((id, node));
                }
            }
        }
        for chunk in repaired.chunks(REBUILD_CHUNK) {
            let mut batch = Batch::new();
            for (id, node) in chunk {
                batch.put(node_key(*id).as_bytes(), &node::encode(node)?);
            }
            engine.apply_batch(&batch)?;
        }

        // 2. Metadata re-anchor, BEFORE any block is removed: if the entry
        //    point is about to be purged, move it to the live node nearest
        //    the old entry's vector (deterministic tie-break by id). Also
        //    recomputes the live count from the data (self-healing).
        let entry_point = match self.entry_point {
            Some(entry) if !dead.contains(&entry) && nodes.contains_key(&entry) => Some(entry),
            Some(entry) => {
                let reference = nodes.get(&entry).map(|node| node.vector.clone());
                nearest_live_in_snapshot(&nodes, reference.as_deref())
            }
            None => nearest_live_in_snapshot(&nodes, None),
        };
        let new_meta = VectorIndexMeta {
            params: self.params,
            epoch: self.epoch,
            count: live_count,
            entry_point,
        };
        let mut meta_batch = Batch::new();
        meta_batch.put(META_KEY, &meta::encode(&new_meta)?);
        engine.apply_batch(&meta_batch)?;

        // 3. Physical purge, in bounded atomic batches. By now no live
        //    node references the dead blocks (phase 1) and the metadata no
        //    longer routes through them (phase 2); a kill mid-purge leaves
        //    unreferenced tombstoned blocks that the next consolidate (or
        //    rebuild) picks up.
        let mut dead_ids: Vec<u64> = dead.iter().copied().collect();
        dead_ids.sort_unstable();
        for chunk in dead_ids.chunks(REBUILD_CHUNK) {
            let mut batch = Batch::new();
            for id in chunk {
                batch.delete(node_key(*id).as_bytes());
            }
            engine.apply_batch(&batch)?;
        }

        self.entry_point = entry_point;
        self.count = live_count;
        lock_cache(&self.cache).clear();
        Ok(dead_ids.len() as u64)
    }

    /// Rebuilds the whole index from the vectors stored in the node blocks
    /// (the data, not the derived graph): scans `idx/vector/node/`, re-runs
    /// the Vamana build in RAM over the **live** blocks (tombstoned blocks
    /// are purged, not resurrected — the tombstone flag travels in the
    /// block, so a rebuild honors confirmed deletes), flushes the fresh
    /// blocks back in bounded batches, and writes new metadata (epoch + 1)
    /// **last**. Crash-safe by ordering, not atomicity: the metadata record
    /// is deleted first, so an interrupted rebuild is re-detected (and
    /// re-run) by the next `open`.
    pub fn rebuild(&mut self, engine: &mut Engine) -> Result<()> {
        let rebuilt = Self::rebuild_from_store(engine, self.params, Some(self.epoch))?;
        *self = rebuilt;
        Ok(())
    }

    fn rebuild_from_store(engine: &mut Engine, params: VectorIndexParams, prior_epoch: Option<u64>) -> Result<Self> {
        // 1. Invalidate the metadata first (its own durable batch): from
        //    this point until step 4 completes, the store reads as "node
        //    blocks without metadata", which `open` maps right back here —
        //    a kill anywhere in between loses nothing and retries.
        let mut invalidate = Batch::new();
        invalidate.delete(META_KEY);
        engine.apply_batch(&invalidate)?;

        // 2. The data: every stored vector. Neighbor lists read here are
        //    *ignored* — they are the derived state being rebuilt.
        //    Tombstoned blocks are collected for purging, never rebuilt
        //    into the graph. A block that fails its checksum is real data
        //    corruption and surfaces as an error (never silently dropped:
        //    these are memories).
        let entries = engine.scan_prefix(NODE_PREFIX)?;
        let mut vectors: Vec<(u64, Vec<f32>)> = Vec::with_capacity(entries.len());
        let mut tombstones: Vec<u64> = Vec::new();
        for (key, value) in &entries {
            let Some(id) = node_id(key.as_bytes()) else {
                return Err(EngineError::CorruptVectorNode {
                    reason: format!(
                        "malformed node key in the idx/vector/node/ keyspace: {:?}",
                        key.as_bytes()
                    ),
                });
            };
            let decoded = node::decode(value)?;
            if decoded.deleted {
                tombstones.push(id);
                continue;
            }
            if decoded.vector.len() != params.dim {
                return Err(EngineError::VectorDimensionMismatch {
                    expected: params.dim,
                    found: decoded.vector.len(),
                });
            }
            vectors.push((id, decoded.vector));
        }

        // 3. Re-run the build in RAM (ascending id order — deterministic).
        let mut ram = VectorIndex::new(params);
        for (id, vector) in vectors {
            ram.insert(id, vector)?;
        }

        // 4. Flush the fresh blocks back in bounded chunks (purging the
        //    tombstoned blocks along the way), metadata last.
        let all: Vec<(u64, &VectorNode)> = ram.iter_nodes().collect();
        for chunk in all.chunks(REBUILD_CHUNK) {
            let mut batch = Batch::new();
            for (id, node) in chunk {
                batch.put(node_key(*id).as_bytes(), &node::encode(node)?);
            }
            engine.apply_batch(&batch)?;
        }
        for chunk in tombstones.chunks(REBUILD_CHUNK) {
            let mut batch = Batch::new();
            for id in chunk {
                batch.delete(node_key(*id).as_bytes());
            }
            engine.apply_batch(&batch)?;
        }
        let epoch = prior_epoch.map_or(1, |e| e + 1);
        let count = u64::try_from(ram.len()).unwrap_or(u64::MAX);
        let new_meta = VectorIndexMeta {
            params,
            epoch,
            count,
            entry_point: ram.entry_point(),
        };
        let mut finalize = Batch::new();
        finalize.put(META_KEY, &meta::encode(&new_meta)?);
        engine.apply_batch(&finalize)?;

        Ok(Self {
            params,
            entry_point: ram.entry_point(),
            epoch,
            count,
            rebuilt_on_open: true,
            cache: Mutex::new(HashMap::new()),
        })
    }
}

/// The live node in `nodes` whose vector is closest to `reference`
/// (deterministic: distance ascending, id ascending as tie-break); with no
/// reference, the smallest live id; `None` when nothing is live.
fn nearest_live_in_snapshot(nodes: &HashMap<u64, VectorNode>, reference: Option<&[f32]>) -> Option<u64> {
    use super::distance::cosine_distance;
    let live = nodes.iter().filter(|(_, node)| !node.deleted);
    match reference {
        Some(reference) => live
            .map(|(&id, node)| (id, cosine_distance(reference, &node.vector)))
            .min_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)))
            .map(|(id, _)| id),
        None => live.map(|(&id, _)| id).min(),
    }
}
