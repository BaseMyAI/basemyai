// SPDX-License-Identifier: BUSL-1.1
//! Vamana graph algorithm (ADR-026 — DiskANN family, flat single-level
//! proximity graph): greedy beam search, robust prune (α), incremental
//! insert with bidirectional linking, tombstone deletes with lazy
//! FreshDiskANN-style repair.
//!
//! The algorithm is written **once**, against the [`NodeProvider`] trait
//! (read a node by id), and shared by both index flavors:
//! - [`VectorIndex`] — the in-RAM index (a `HashMap` provider), kept as the
//!   algorithm's reference implementation and judged on recall@10 by
//!   `tests/vector_recall.rs` / `tests/vector_churn.rs`;
//! - [`super::persistent::PersistentVectorIndex`] — the KV-persisted index
//!   (a read-through provider over `Engine::get`), which turns an
//!   [`InsertPlan`] into one atomic `Engine::apply_batch`.
//!
//! Sharing the planner is the point: the two flavors cannot drift apart on
//! the subtle parts (robust prune, back-edge re-prune, tombstone repair),
//! the storage layer is the only difference. [`plan_insert`] therefore
//! *plans* a mutation — it returns every node block the insert touches —
//! and each flavor applies the plan to its own storage (HashMap insert vs
//! `apply_batch`). [`plan_repair`] does the same for the consolidation
//! pass.
//!
//! ## Deletes (ADR-026 §4, FreshDiskANN style)
//!
//! A delete only flips the node's tombstone flag: the node keeps its vector
//! and neighbor list and stays **traversable as a routing point** — greedy
//! search still walks through it — but is **excluded from results**
//! ([`search_live_scored`]). Newly written neighbor lists never point at
//! tombstones ([`robust_prune`] skips them), so tombstone references decay
//! naturally as the graph is edited; the explicit consolidation pass
//! ([`plan_repair`] per node) purges the rest: every live node that still
//! references tombstones is reconnected via robust prune over the union of
//! its live neighbors and its tombstoned neighbors' live neighbors — the
//! standard FreshDiskANN α-prune patch — after which tombstoned nodes can
//! be physically removed.
//!
//! **Re-insert semantics**: inserting an id whose node is tombstoned is a
//! *resurrection* — the insert proceeds exactly like a fresh insert (greedy
//! search, robust prune, back-linking) and the new block overwrites the
//! tombstone. Stale in-edges pointing at the old position remain as
//! harmless approximate edges (the node is live again, returning it is
//! correct). [`EngineError::DuplicateVectorId`] is reserved for *live*
//! duplicates only — this is what makes "update = delete + reinsert" work.

use std::collections::{HashMap, HashSet};

use super::distance::cosine_distance;
use super::meta::VectorIndexParams;
use super::node::VectorNode;
use crate::error::{EngineError, Result};

/// Node ids paired with their distance to some reference vector, ascending
/// where sorted.
pub(crate) type Scored = Vec<(u64, f32)>;

/// Read access to node blocks, whatever their storage. `&mut self` because
/// persistent providers cache decoded blocks on read (read-through); the
/// in-RAM provider simply clones out of its map.
///
/// Returning an owned [`VectorNode`] (a clone) keeps the trait object-simple
/// and borrow-friendly; the copy cost is accepted for now — ADR-026 promises
/// no performance number at this step, and the block-cache/perf pass comes
/// with the parity bench.
pub(crate) trait NodeProvider {
    fn node(&mut self, id: u64) -> Result<Option<VectorNode>>;
}

/// A planned insert: every node block the insert writes (the new node plus
/// each neighbor whose list changed), and the entry point after the insert.
/// Fresh inserts and resurrections produce the same shape of plan — the
/// caller's live count goes up by one either way (a tombstone was never
/// counted as live).
pub(crate) struct InsertPlan {
    pub(crate) changed: Vec<(u64, VectorNode)>,
    pub(crate) entry_point: u64,
}

/// Overlay provider used while planning: reads see the pending (planned)
/// writes first, then fall through to the base storage — exactly the view
/// the in-RAM implementation used to have by mutating its map in place.
struct Overlay<'a, P: NodeProvider> {
    base: &'a mut P,
    changed: HashMap<u64, VectorNode>,
}

impl<P: NodeProvider> NodeProvider for Overlay<'_, P> {
    fn node(&mut self, id: u64) -> Result<Option<VectorNode>> {
        if let Some(node) = self.changed.get(&id) {
            return Ok(Some(node.clone()));
        }
        self.base.node(id)
    }
}

/// Standard Vamana greedy beam search from `entry`.
///
/// Returns `(frontier, visited)`:
/// - `frontier` — the final candidate list (≤ `beam` entries), sorted by
///   ascending distance to `query`; its prefix is the search result.
///   Tombstoned nodes are *included* here (they are routing points); result
///   paths filter them via [`search_live_scored`].
/// - `visited` — every node actually expanded, with its distance; the
///   robust-prune candidate pool for inserts.
///
/// A missing entry node yields empty results (the persistent flavor treats
/// that as metadata inconsistency and rebuilds instead of searching).
pub(crate) fn greedy_search<P: NodeProvider>(
    provider: &mut P,
    entry: u64,
    query: &[f32],
    beam: usize,
) -> Result<(Scored, Scored)> {
    let Some(entry_state) = provider.node(entry)? else {
        return Ok((Vec::new(), Vec::new()));
    };

    let mut seen: HashSet<u64> = HashSet::from([entry]);
    let mut expanded: HashSet<u64> = HashSet::new();
    let mut frontier: Scored = vec![(entry, cosine_distance(query, &entry_state.vector))];
    let mut visited: Scored = Vec::new();

    // Loop invariant: `frontier` is sorted ascending by distance and
    // capped at `beam`. Terminates when every frontier entry has been
    // expanded (each iteration expands exactly one new node).
    while let Some(&(current, current_dist)) = frontier.iter().find(|(id, _)| !expanded.contains(id)) {
        expanded.insert(current);
        visited.push((current, current_dist));

        let Some(state) = provider.node(current)? else {
            continue;
        };
        for &neighbor_id in &state.neighbors {
            if !seen.insert(neighbor_id) {
                continue;
            }
            let Some(neighbor_state) = provider.node(neighbor_id)? else {
                continue;
            };
            frontier.push((neighbor_id, cosine_distance(query, &neighbor_state.vector)));
        }
        frontier.sort_by(|a, b| a.1.total_cmp(&b.1));
        frontier.truncate(beam);
    }
    Ok((frontier, visited))
}

/// Greedy search, then filter tombstones out of the frontier: the first `k`
/// **live** ids in ascending distance order, each with its distance to
/// `query` (already computed by the greedy walk — no re-measurement).
/// Tombstones route (they were walked through above) but never surface as
/// results. The distances are what a consumer exposes as a ranking *score*
/// (e.g. `MemoryStore::recall_vector`'s cosine-distance `Record::score`,
/// ADR-027 §6); the id-only `search` wrappers just drop them.
pub(crate) fn search_live_scored<P: NodeProvider>(
    provider: &mut P,
    entry: u64,
    query: &[f32],
    beam: usize,
    k: usize,
) -> Result<Scored> {
    let (frontier, _) = greedy_search(provider, entry, query, beam)?;
    let mut out = Vec::with_capacity(k);
    for (id, dist) in frontier {
        if let Some(node) = provider.node(id)?
            && !node.deleted
        {
            out.push((id, dist));
            if out.len() == k {
                break;
            }
        }
    }
    Ok(out)
}

/// Vamana robust prune: from `candidates` (ids with their precomputed
/// distance to the base point), pick up to `R` neighbors, dropping every
/// candidate `v` dominated by an already-picked `p*` — i.e. where
/// `α · d(p*, v) ≤ d(base, v)`. The base vector itself is not needed:
/// every distance-to-base is already carried by `candidates`.
///
/// Tombstoned candidates are skipped: rewritten neighbor lists only ever
/// point at live nodes, so tombstone references never *spread* — they only
/// linger in lists written before the delete, until repair/consolidation.
pub(crate) fn robust_prune<P: NodeProvider>(
    provider: &mut P,
    params: &VectorIndexParams,
    mut candidates: Scored,
) -> Result<Vec<u64>> {
    // Dedupe by id (the visited set never repeats, but merged pools —
    // re-prune paths — can), then prefetch every candidate's vector once
    // (dropping candidates whose node no longer exists or is tombstoned),
    // then order by distance to `base`.
    candidates.sort_by_key(|(id, _)| *id);
    candidates.dedup_by_key(|(id, _)| *id);

    let mut vectors: HashMap<u64, Vec<f32>> = HashMap::with_capacity(candidates.len());
    let mut pool: Scored = Vec::with_capacity(candidates.len());
    for (id, dist) in candidates {
        if let Some(node) = provider.node(id)? {
            if node.deleted {
                continue;
            }
            vectors.insert(id, node.vector);
            pool.push((id, dist));
        }
    }
    pool.sort_by(|a, b| a.1.total_cmp(&b.1));

    let mut result: Vec<u64> = Vec::new();
    while let Some(&(best, _)) = pool.first() {
        result.push(best);
        if result.len() >= params.max_degree {
            break;
        }
        let best_vector = vectors
            .get(&best)
            .expect("prefetched above: every pool id has a vector");
        let alpha = params.alpha;
        pool.retain(|(id, dist_to_base)| {
            if *id == best {
                return false;
            }
            let Some(vector) = vectors.get(id) else {
                return false;
            };
            alpha * cosine_distance(best_vector, vector) > *dist_to_base
        });
    }
    Ok(result)
}

/// Plans the insertion of `vector` under `id`: greedy search for the
/// insertion neighborhood, robust prune of the visited set into the new
/// node's out-neighbors, then bidirectional linking with re-prune of any
/// neighbor whose degree overflows `R` — never truncated arbitrarily.
///
/// Pure planning: `base` is only read. The returned plan carries every node
/// block that must be (re)written; applying all of them together with the
/// data in one atomic batch is exactly ADR-026's crash-consistency invariant.
///
/// Errors with [`EngineError::VectorDimensionMismatch`] on a wrong-sized
/// vector and [`EngineError::DuplicateVectorId`] if `id` is already **live**
/// in the index. A *tombstoned* `id` is resurrected instead (see the module
/// doc) — the plan overwrites the tombstone with the freshly linked node.
pub(crate) fn plan_insert<P: NodeProvider>(
    base: &mut P,
    params: &VectorIndexParams,
    entry_point: Option<u64>,
    id: u64,
    vector: Vec<f32>,
) -> Result<InsertPlan> {
    if vector.len() != params.dim {
        return Err(EngineError::VectorDimensionMismatch {
            expected: params.dim,
            found: vector.len(),
        });
    }
    if let Some(existing) = base.node(id)?
        && !existing.deleted
    {
        return Err(EngineError::DuplicateVectorId { id });
    }

    // First node: becomes the entry point, no edges to build.
    let Some(entry) = entry_point else {
        return Ok(InsertPlan {
            changed: vec![(id, VectorNode::live(vector, Vec::new()))],
            entry_point: id,
        });
    };

    let mut overlay = Overlay {
        base,
        changed: HashMap::new(),
    };

    // 1. Greedy search for the insertion neighborhood: the *visited* set
    //    (every node the beam expanded) is the robust-prune candidate
    //    pool, per the Vamana paper. On resurrection the old tombstone of
    //    `id` may appear in it — a node must never neighbor itself.
    let (_, mut visited) = greedy_search(&mut overlay, entry, &vector, params.beam_width)?;
    visited.retain(|(vid, _)| *vid != id);

    // 2. Robust prune the pool into the new node's out-neighbors (live
    //    only — see `robust_prune`). Safety net for heavily tombstoned
    //    neighborhoods: if pruning leaves nothing (every visited node is a
    //    tombstone), fall back to the nearest visited nodes regardless of
    //    tombstone state, so the new node is never born unreachable —
    //    tombstones are legitimate routing points and consolidation will
    //    patch these edges through later.
    let mut neighbors = robust_prune(&mut overlay, params, visited.clone())?;
    if neighbors.is_empty() && !visited.is_empty() {
        visited.sort_by(|a, b| a.1.total_cmp(&b.1));
        neighbors = visited.iter().take(params.max_degree).map(|&(vid, _)| vid).collect();
    }

    // 3. Materialize the node in the overlay, then link back: each chosen
    //    neighbor gets an edge to the new node; any neighbor whose degree
    //    overflows R gets its own list re-pruned.
    overlay.changed.insert(id, VectorNode::live(vector, neighbors.clone()));
    for neighbor_id in neighbors {
        let Some(mut state) = overlay.node(neighbor_id)? else {
            continue; // unreachable: pruned ids come from the graph
        };
        if !state.neighbors.contains(&id) {
            state.neighbors.push(id);
        }
        let overflows = state.neighbors.len() > params.max_degree;
        overlay.changed.insert(neighbor_id, state);
        if overflows {
            reprune_node(&mut overlay, params, neighbor_id)?;
        }
    }

    Ok(InsertPlan {
        changed: overlay.changed.into_iter().collect(),
        entry_point: entry,
    })
}

/// Re-prunes one node's neighbor list back under `R` after a back-edge
/// pushed it over. Operates on the overlay so it sees (and records) pending
/// planned writes. Preserves the node's tombstone flag: re-pruning a
/// routing tombstone must not resurrect it.
fn reprune_node<P: NodeProvider>(overlay: &mut Overlay<'_, P>, params: &VectorIndexParams, id: u64) -> Result<()> {
    let Some(state) = overlay.node(id)? else {
        return Ok(());
    };
    let mut candidates: Scored = Vec::with_capacity(state.neighbors.len());
    for &neighbor_id in &state.neighbors {
        if neighbor_id == id {
            continue;
        }
        if let Some(neighbor_state) = overlay.node(neighbor_id)? {
            candidates.push((neighbor_id, cosine_distance(&state.vector, &neighbor_state.vector)));
        }
    }
    let pruned = robust_prune(overlay, params, candidates)?;
    overlay.changed.insert(
        id,
        VectorNode {
            vector: state.vector,
            neighbors: pruned,
            deleted: state.deleted,
        },
    );
    Ok(())
}

/// Plans the FreshDiskANN repair of one node for the consolidation pass:
/// if the **live** node `id` still references tombstoned (or vanished)
/// neighbors, rebuild its list by robust-pruning the union of its live
/// neighbors and its tombstoned neighbors' live neighbors (the
/// neighbor-of-neighbor patch — the paths that used to route *through* the
/// tombstone are re-established directly).
///
/// Returns `Ok(None)` when there is nothing to do: the node is missing,
/// itself tombstoned (tombstones are about to be dropped, no point
/// repairing them), or references no tombstone.
pub(crate) fn plan_repair<P: NodeProvider>(
    provider: &mut P,
    params: &VectorIndexParams,
    id: u64,
) -> Result<Option<VectorNode>> {
    let Some(node) = provider.node(id)? else {
        return Ok(None);
    };
    if node.deleted {
        return Ok(None);
    }

    let mut has_dead = false;
    let mut pool_ids: Vec<u64> = Vec::with_capacity(node.neighbors.len());
    for &neighbor_id in &node.neighbors {
        match provider.node(neighbor_id)? {
            Some(neighbor) if neighbor.deleted => {
                has_dead = true;
                for &through in &neighbor.neighbors {
                    if through != id {
                        pool_ids.push(through);
                    }
                }
            }
            Some(_) => pool_ids.push(neighbor_id),
            // Dangling reference (block already purged): treat like a
            // tombstone with no neighbors to patch through.
            None => has_dead = true,
        }
    }
    if !has_dead {
        return Ok(None);
    }

    pool_ids.sort_unstable();
    pool_ids.dedup();
    let mut candidates: Scored = Vec::with_capacity(pool_ids.len());
    for candidate_id in pool_ids {
        if candidate_id == id {
            continue;
        }
        if let Some(candidate) = provider.node(candidate_id)?
            && !candidate.deleted
        {
            candidates.push((candidate_id, cosine_distance(&node.vector, &candidate.vector)));
        }
    }
    let pruned = robust_prune(provider, params, candidates)?;
    Ok(Some(VectorNode::live(node.vector, pruned)))
}

/// Provider over a plain in-RAM node map.
struct MapProvider<'a> {
    nodes: &'a HashMap<u64, VectorNode>,
}

impl NodeProvider for MapProvider<'_> {
    fn node(&mut self, id: u64) -> Result<Option<VectorNode>> {
        Ok(self.nodes.get(&id).cloned())
    }
}

/// An in-RAM LM-DiskANN-style vector index (Vamana graph, cosine distance).
///
/// - [`VectorIndex::insert`] — incremental: greedy search from the entry
///   point, robust prune of the visited set into the new node's neighbor
///   list, then bidirectional linking with re-prune of any neighbor whose
///   degree overflows `R`.
/// - [`VectorIndex::search`] — greedy beam search (width `max(L, k)`),
///   returning the `k` closest **live** ids in ascending distance order.
/// - [`VectorIndex::delete`] — tombstone (kept as a routing point, excluded
///   from results); [`VectorIndex::consolidate`] — FreshDiskANN repair +
///   physical purge of every tombstone. See the module doc.
#[derive(Debug)]
pub struct VectorIndex {
    params: VectorIndexParams,
    nodes: HashMap<u64, VectorNode>,
    /// Number of **live** (non-tombstoned) nodes — what [`Self::len`]
    /// reports; `nodes.len()` additionally counts tombstones awaiting
    /// consolidation.
    live_count: usize,
    /// First inserted id; the fixed navigation start of every search
    /// (Vamana uses a single global entry point — persisted via the index
    /// metadata in the KV-backed flavor, see `meta.rs`). May point at a
    /// tombstone (still a valid routing start); consolidation re-anchors it
    /// onto a live node.
    entry_point: Option<u64>,
}

impl VectorIndex {
    #[must_use]
    pub fn new(params: VectorIndexParams) -> Self {
        Self {
            params,
            nodes: HashMap::new(),
            live_count: 0,
            entry_point: None,
        }
    }

    #[must_use]
    pub fn params(&self) -> &VectorIndexParams {
        &self.params
    }

    /// Number of live vectors (tombstones excluded).
    #[must_use]
    pub fn len(&self) -> usize {
        self.live_count
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.live_count == 0
    }

    /// Current entry point (`None` iff nothing was ever inserted). May be a
    /// tombstone between a delete and the next consolidation.
    #[must_use]
    pub fn entry_point(&self) -> Option<u64> {
        self.entry_point
    }

    /// Iterates over `(id, node)` pairs in unspecified order — tombstones
    /// included (used by the persistent flavor's rebuild path, which skips
    /// and purges them).
    pub(crate) fn iter_nodes(&self) -> impl Iterator<Item = (u64, &VectorNode)> {
        self.nodes.iter().map(|(&id, node)| (id, node))
    }

    /// Inserts a vector under `id`.
    ///
    /// Errors with [`EngineError::VectorDimensionMismatch`] on a wrong-sized
    /// vector and [`EngineError::DuplicateVectorId`] if `id` is already
    /// live. A tombstoned `id` is resurrected (see the module doc).
    pub fn insert(&mut self, id: u64, vector: Vec<f32>) -> Result<()> {
        let plan = plan_insert(
            &mut MapProvider { nodes: &self.nodes },
            &self.params,
            self.entry_point,
            id,
            vector,
        )?;
        for (node_id, node) in plan.changed {
            self.nodes.insert(node_id, node);
        }
        self.entry_point = Some(plan.entry_point);
        self.live_count += 1;
        Ok(())
    }

    /// Tombstones `id`: excluded from every future [`Self::search`] result
    /// but kept in the graph as a routing point until [`Self::consolidate`].
    /// Returns `false` (and changes nothing) if `id` is absent or already
    /// tombstoned — deletes are idempotent, never an error.
    pub fn delete(&mut self, id: u64) -> bool {
        match self.nodes.get_mut(&id) {
            Some(node) if !node.deleted => {
                node.deleted = true;
                self.live_count -= 1;
                true
            }
            _ => false,
        }
    }

    /// FreshDiskANN consolidation: repairs every live node that still
    /// references a tombstone (robust prune over live
    /// neighbors-of-neighbors, [`plan_repair`]), re-anchors the entry point
    /// on the live node nearest the old entry if the entry was tombstoned,
    /// then physically removes every tombstone. Returns the number of
    /// tombstones purged.
    pub fn consolidate(&mut self) -> Result<usize> {
        let dead: Vec<u64> = self
            .nodes
            .iter()
            .filter_map(|(&id, node)| node.deleted.then_some(id))
            .collect();
        if dead.is_empty() {
            return Ok(0);
        }

        // 1. Repair every live node still referencing a tombstone. Plans
        //    are computed against the pre-repair graph (deterministic,
        //    order-independent), then applied together.
        let live_ids: Vec<u64> = self
            .nodes
            .iter()
            .filter_map(|(&id, node)| (!node.deleted).then_some(id))
            .collect();
        let mut repaired: Vec<(u64, VectorNode)> = Vec::new();
        {
            let mut provider = MapProvider { nodes: &self.nodes };
            for &id in &live_ids {
                if let Some(node) = plan_repair(&mut provider, &self.params, id)? {
                    repaired.push((id, node));
                }
            }
        }
        for (id, node) in repaired {
            self.nodes.insert(id, node);
        }

        // 2. Re-anchor the entry point if it is about to be purged: the
        //    live node closest to the old entry's vector (deterministic
        //    tie-break by id).
        if let Some(entry) = self.entry_point
            && self.nodes.get(&entry).is_none_or(|node| node.deleted)
        {
            let old_vector = self.nodes.get(&entry).map(|node| node.vector.clone());
            self.entry_point = nearest_live(&self.nodes, old_vector.as_deref());
        }

        // 3. Physical purge.
        for id in &dead {
            self.nodes.remove(id);
        }
        Ok(dead.len())
    }

    /// Returns the ids of (up to) the `k` approximate nearest **live**
    /// neighbors of `query`, ordered by ascending cosine distance.
    pub fn search(&self, query: &[f32], k: usize) -> Result<Vec<u64>> {
        Ok(self.search_scored(query, k)?.into_iter().map(|(id, _)| id).collect())
    }

    /// [`Self::search`], keeping each result's cosine distance to `query`
    /// (same shape as [`super::persistent::PersistentVectorIndex::search_scored`]
    /// — the two flavors expose the same API surface, same discipline as the
    /// shared planner).
    pub fn search_scored(&self, query: &[f32], k: usize) -> Result<Vec<(u64, f32)>> {
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
        search_live_scored(&mut MapProvider { nodes: &self.nodes }, entry, query, beam, k)
    }
}

/// The live node whose vector is closest to `reference` (deterministic:
/// distance ascending, id ascending as tie-break); with no reference (or no
/// live node), the smallest live id, or `None` on an all-dead map.
fn nearest_live(nodes: &HashMap<u64, VectorNode>, reference: Option<&[f32]>) -> Option<u64> {
    let live = nodes.iter().filter(|(_, node)| !node.deleted);
    match reference {
        Some(reference) => live
            .map(|(&id, node)| (id, cosine_distance(reference, &node.vector)))
            .min_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)))
            .map(|(id, _)| id),
        None => live.map(|(&id, _)| id).min(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn small_params() -> VectorIndexParams {
        VectorIndexParams::with_dim(4)
    }

    #[test]
    fn empty_index_returns_no_results() {
        let index = VectorIndex::new(small_params());
        let results = index.search(&[0.0, 0.0, 0.0, 1.0], 10).expect("search ok");
        assert!(results.is_empty());
    }

    #[test]
    fn single_node_is_found() {
        let mut index = VectorIndex::new(small_params());
        index.insert(1, vec![1.0, 0.0, 0.0, 0.0]).expect("insert ok");
        let results = index.search(&[1.0, 0.0, 0.0, 0.0], 5).expect("search ok");
        assert_eq!(results, vec![1]);
    }

    #[test]
    fn exact_match_ranks_first_among_a_few_nodes() {
        let mut index = VectorIndex::new(small_params());
        index.insert(1, vec![1.0, 0.0, 0.0, 0.0]).expect("insert ok");
        index.insert(2, vec![0.0, 1.0, 0.0, 0.0]).expect("insert ok");
        index.insert(3, vec![0.0, 0.0, 1.0, 0.0]).expect("insert ok");
        index.insert(4, vec![0.9, 0.1, 0.0, 0.0]).expect("insert ok");
        let results = index.search(&[0.0, 1.0, 0.0, 0.0], 2).expect("search ok");
        assert_eq!(results.first(), Some(&2));
    }

    #[test]
    fn wrong_dimension_is_rejected_on_insert_and_search() {
        let mut index = VectorIndex::new(small_params());
        let err = index.insert(1, vec![1.0, 0.0]).expect_err("dim mismatch");
        assert!(matches!(
            err,
            EngineError::VectorDimensionMismatch { expected: 4, found: 2 }
        ));
        let err = index.search(&[1.0], 3).expect_err("dim mismatch");
        assert!(matches!(
            err,
            EngineError::VectorDimensionMismatch { expected: 4, found: 1 }
        ));
    }

    #[test]
    fn duplicate_live_id_is_rejected() {
        let mut index = VectorIndex::new(small_params());
        index.insert(7, vec![1.0, 0.0, 0.0, 0.0]).expect("insert ok");
        let err = index.insert(7, vec![0.0, 1.0, 0.0, 0.0]).expect_err("duplicate id");
        assert!(matches!(err, EngineError::DuplicateVectorId { id: 7 }));
    }

    #[test]
    fn deleted_id_is_excluded_from_results_but_len_and_routing_survive() {
        let mut index = VectorIndex::new(small_params());
        index.insert(1, vec![1.0, 0.0, 0.0, 0.0]).expect("insert ok");
        index.insert(2, vec![0.9, 0.1, 0.0, 0.0]).expect("insert ok");
        index.insert(3, vec![0.0, 1.0, 0.0, 0.0]).expect("insert ok");
        assert_eq!(index.len(), 3);

        assert!(index.delete(1));
        assert_eq!(index.len(), 2);
        // Idempotent: second delete of the same id is a no-op.
        assert!(!index.delete(1));
        assert!(!index.delete(999));
        assert_eq!(index.len(), 2);

        let results = index.search(&[1.0, 0.0, 0.0, 0.0], 3).expect("search ok");
        assert!(!results.contains(&1), "tombstoned id must never surface: {results:?}");
        assert!(results.contains(&2), "live nodes must stay reachable: {results:?}");
        assert!(results.contains(&3));
    }

    #[test]
    fn reinserting_a_tombstoned_id_resurrects_it_with_the_new_vector() {
        let mut index = VectorIndex::new(small_params());
        index.insert(1, vec![1.0, 0.0, 0.0, 0.0]).expect("insert ok");
        index.insert(2, vec![0.0, 1.0, 0.0, 0.0]).expect("insert ok");
        assert!(index.delete(1));

        // update = delete + reinsert: must succeed, with the NEW vector.
        index.insert(1, vec![0.0, 0.0, 1.0, 0.0]).expect("resurrection ok");
        assert_eq!(index.len(), 2);
        let results = index.search(&[0.0, 0.0, 1.0, 0.0], 1).expect("search ok");
        assert_eq!(results, vec![1]);
        // And it is live again: re-inserting now is a duplicate.
        let err = index.insert(1, vec![1.0, 1.0, 0.0, 0.0]).expect_err("live duplicate");
        assert!(matches!(err, EngineError::DuplicateVectorId { id: 1 }));
    }

    #[test]
    fn consolidate_purges_tombstones_and_keeps_live_nodes_searchable() {
        let mut index = VectorIndex::new(small_params());
        for i in 0..30u64 {
            let angle = i as f32 * 0.21;
            index
                .insert(
                    i,
                    vec![angle.cos(), angle.sin(), (angle * 0.7).cos(), (angle * 0.7).sin()],
                )
                .expect("insert ok");
        }
        // Delete the entry point (id 0) and a third of the rest.
        let deleted: Vec<u64> = (0..30).step_by(3).collect();
        for &id in &deleted {
            assert!(index.delete(id));
        }
        let purged = index.consolidate().expect("consolidate ok");
        assert_eq!(purged, deleted.len());
        assert_eq!(index.len(), 30 - deleted.len());
        assert_eq!(index.nodes.len(), index.len(), "tombstones must be physically gone");
        // No neighbor list references a purged id.
        for (id, node) in index.iter_nodes() {
            for neighbor in &node.neighbors {
                assert!(
                    index.nodes.contains_key(neighbor),
                    "node {id} references purged neighbor {neighbor}"
                );
            }
        }
        // Entry point was re-anchored on a live node.
        let entry = index.entry_point().expect("entry point");
        assert!(!index.nodes[&entry].deleted);
        // Every live node is still findable by its own vector.
        for (id, node) in index.nodes.clone() {
            let results = index.search(&node.vector, 3).expect("search ok");
            assert!(results.contains(&id), "live node {id} lost after consolidate");
        }
        // Idempotent when there is nothing to purge.
        assert_eq!(index.consolidate().expect("consolidate ok"), 0);
    }

    #[test]
    fn consolidate_of_fully_deleted_index_empties_it() {
        let mut index = VectorIndex::new(small_params());
        index.insert(1, vec![1.0, 0.0, 0.0, 0.0]).expect("insert ok");
        index.insert(2, vec![0.0, 1.0, 0.0, 0.0]).expect("insert ok");
        index.delete(1);
        index.delete(2);
        assert!(index.search(&[1.0, 0.0, 0.0, 0.0], 5).expect("search ok").is_empty());
        assert_eq!(index.consolidate().expect("consolidate ok"), 2);
        assert!(index.is_empty());
        assert!(index.entry_point().is_none());
        // And the index is usable again afterwards.
        index.insert(9, vec![0.5, 0.5, 0.0, 0.0]).expect("insert ok");
        assert_eq!(index.search(&[0.5, 0.5, 0.0, 0.0], 1).expect("search ok"), vec![9]);
    }

    #[test]
    fn degrees_never_exceed_max_degree() {
        let params = VectorIndexParams {
            dim: 4,
            max_degree: 3,
            beam_width: 8,
            alpha: 1.2,
        };
        let mut index = VectorIndex::new(params);
        for i in 0..50u64 {
            let angle = i as f32 * 0.13;
            index
                .insert(
                    i,
                    vec![angle.cos(), angle.sin(), (angle * 0.7).cos(), (angle * 0.7).sin()],
                )
                .expect("insert ok");
        }
        for state in index.nodes.values() {
            assert!(
                state.neighbors.len() <= params.max_degree,
                "degree {} exceeds R={}",
                state.neighbors.len(),
                params.max_degree
            );
        }
    }

    #[test]
    fn search_returns_at_most_k_ids_sorted_by_distance() {
        let mut index = VectorIndex::new(small_params());
        for i in 0..20u64 {
            let x = i as f32 / 20.0;
            index.insert(i, vec![x, 1.0 - x, 0.5, 0.25]).expect("insert ok");
        }
        let query = vec![0.2, 0.8, 0.5, 0.25];
        let results = index.search(&query, 5).expect("search ok");
        assert_eq!(results.len(), 5);
        let dists: Vec<f32> = results
            .iter()
            .map(|id| cosine_distance(&query, &index.nodes[id].vector))
            .collect();
        for pair in dists.windows(2) {
            assert!(pair[0] <= pair[1], "results not sorted: {dists:?}");
        }
    }

    /// The plan of an insert must contain the new node plus every neighbor
    /// whose list changed — and nothing that stayed untouched.
    #[test]
    fn plan_insert_reports_exactly_the_touched_nodes() {
        let mut index = VectorIndex::new(small_params());
        index.insert(1, vec![1.0, 0.0, 0.0, 0.0]).expect("insert ok");
        index.insert(2, vec![0.0, 1.0, 0.0, 0.0]).expect("insert ok");

        let plan = plan_insert(
            &mut MapProvider { nodes: &index.nodes },
            &index.params,
            index.entry_point,
            3,
            vec![0.7, 0.7, 0.0, 0.0],
        )
        .expect("plan ok");
        let changed_ids: HashSet<u64> = plan.changed.iter().map(|(id, _)| *id).collect();
        assert!(changed_ids.contains(&3), "the new node must be in the plan");
        // Back-linking touched at least one existing neighbor.
        assert!(
            changed_ids.len() >= 2,
            "expected the new node plus at least one back-linked neighbor, got {changed_ids:?}"
        );
        // Planning must not have mutated the base index.
        assert_eq!(index.len(), 2);
        assert!(!index.nodes.contains_key(&3));
    }
}
