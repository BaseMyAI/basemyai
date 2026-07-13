// SPDX-License-Identifier: BUSL-1.1
//! KV-persisted memory index (N5.1, ADR-027): memory records, their
//! vector-id mappings and the monotonic id allocator, under the reserved
//! `idx/memory/` keyspace ([`crate::key::memory_index`]). Isolation by agent
//! is **structural** (the key layout), same discipline as `idx::graph`.
//!
//! ## What lives here vs at the consumer
//!
//! This type owns every **crash-critical composition**: a put stages the
//! record + its reverse mapping + the allocator bump as the `extra` batch of
//! [`PersistentVectorIndex::insert_with`], so the whole memory — data AND
//! index — commits as **one** WAL record (ADR-027 §3), the native equivalent
//! of the multi-row libSQL transaction it replaces; a forget rides
//! [`PersistentVectorIndex::delete_with`] the same way. Keeping the
//! composition here (not in `basemyai`'s `NativeMemoryStore`) is what lets
//! the crash-consistency harness exercise it engine-side one day, like the
//! existing `vector`/`graph` modes (N5.5).
//!
//! Query *policy* — validity windows, layer filtering, oversampling,
//! hydration order — stays at the consumer. The record's `layer` is an
//! opaque tag here; the engine never interprets it.
//!
//! ## The allocator (ADR-027 §4)
//!
//! `next_vec_id` is strictly monotonic and persisted in the same atomic
//! batch as every put, so it can never lag behind the vector nodes it
//! allocated. If its record is absent or corrupt, [`Self::open`] **heals
//! from the data**: max over the vector-node keys ∪ vecmap keys, + 1. That
//! is safe *only because* of the same-batch guarantee — a healed counter can
//! never land on a live or tombstoned id. A counter value bumped in RAM but
//! never committed (failed insert) just skips an id, which is benign.

use std::collections::HashSet;

use crate::error::{EngineError, Result};
use crate::idx::fts::PersistentFts;
use crate::idx::vector::PersistentVectorIndex;
use crate::key::{agent_registry, memory_index, temporal_index, vector_index};
use crate::store::{Batch, Engine};

use super::meta::{self, MemoryIndexMeta};
use super::record::{self, MemoryRecord};
use super::vecmap::{self, VecMapEntry};

/// The attribute set of a memory about to be inserted — everything a
/// [`MemoryRecord`] carries except `vec_id`, which [`PersistentMemoryIndex::put`]
/// allocates itself (the allocator is not the caller's to drive).
#[derive(Debug, Clone)]
pub struct NewMemoryRecord<'a> {
    /// Opaque layer tag (the consumer's `MemoryLayer::table()` string).
    pub layer: &'a str,
    pub content: &'a str,
    /// Provenance tag (`"user"`, `"consolidation"`, …).
    pub source: &'a str,
    pub valid_from: i64,
    pub valid_until: Option<i64>,
    pub importance: f64,
    pub last_access: i64,
}

/// Bounds of one physical deletion batch of [`PersistentMemoryIndex::forget_many`]
/// (ADR-041 §7.4): a group of memories is split into chunks so that no
/// single WAL record ever aggregates more than `max_items` memories or
/// (approximately) more than `max_wal_bytes` of staged operations. Both are
/// sizing targets, not exact wire bounds: byte accounting is estimated
/// ([`crate::store::Batch::approx_wire_bytes`], tombstone/FTS footprints),
/// and one single memory whose own footprint exceeds `max_wal_bytes` still
/// ships as a one-item chunk (per-memory atomicity is the floor — a memory
/// is never split across two batches).
#[derive(Debug, Clone, Copy)]
pub struct ForgetBatchOptions {
    /// Maximum number of memories deleted per atomic batch. `0` is clamped
    /// to `1` (a zero-item batch would never make progress).
    pub max_items: usize,
    /// Approximate byte budget of one atomic batch's WAL record.
    pub max_wal_bytes: usize,
}

impl Default for ForgetBatchOptions {
    /// Order-of-magnitude defaults (same posture as the engine's block-cache
    /// default: a sane starting point, not a measured optimum): 256 memories
    /// or ~4 MiB per WAL record, whichever comes first.
    fn default() -> Self {
        Self {
            max_items: 256,
            max_wal_bytes: 4 * 1024 * 1024,
        }
    }
}

/// Stages a temporal-expiry index entry for `(agent, id, valid_until)` into
/// `batch` — a no-op if `valid_until` is `None` (an eternal memory has
/// nothing to ever expire, ADR-041 §7.2).
fn stage_expiry_insert(batch: &mut Batch, agent: &str, id: &str, valid_until: Option<i64>) -> Result<()> {
    if let Some(valid_until) = valid_until {
        batch.put(temporal_index::expiry_key(agent, valid_until, id)?.as_bytes(), &[]);
    }
    Ok(())
}

/// Removes the temporal-expiry index entry for `(agent, id, valid_until)`
/// from `batch` — a no-op if `valid_until` is `None` (nothing was ever
/// indexed for it).
fn stage_expiry_delete(batch: &mut Batch, agent: &str, id: &str, valid_until: Option<i64>) -> Result<()> {
    if let Some(valid_until) = valid_until {
        batch.delete(temporal_index::expiry_key(agent, valid_until, id)?.as_bytes());
    }
    Ok(())
}

/// Handle over the KV-persisted memory index. Holds exactly one piece of
/// cached state: the in-RAM copy of the monotonic allocator (see the module
/// doc). Everything else is read through the [`Engine`] on demand.
#[derive(Debug)]
pub struct PersistentMemoryIndex {
    next_vec_id: u64,
}

impl PersistentMemoryIndex {
    /// Opens the index stored in `engine`, or initializes an empty one.
    ///
    /// The allocator record is loaded when present and valid; **healed from
    /// the data** (max of vector-node keys ∪ vecmap keys, + 1 — see the
    /// module doc for why that is safe) when absent or corrupt. A
    /// version-unsupported record is a hard error — a newer build wrote it,
    /// healing over it could silently regress the counter.
    pub fn open(engine: &Engine) -> Result<Self> {
        let next_vec_id = match engine.get(memory_index::META_KEY)? {
            Some(bytes) => match meta::decode(&bytes) {
                Ok(stored) => stored.next_vec_id,
                Err(EngineError::CorruptMemoryIndexMeta { .. }) => Self::heal_next_vec_id(engine)?,
                Err(other) => return Err(other),
            },
            None => Self::heal_next_vec_id(engine)?,
        };
        Ok(Self { next_vec_id })
    }

    /// Recomputes the allocator from the data: one past the highest id ever
    /// observed in either the vector-node keyspace or the vecmap keyspace
    /// (a fully purged id may be re-allocated — harmless, nothing references
    /// it anywhere anymore).
    fn heal_next_vec_id(engine: &Engine) -> Result<u64> {
        let mut max_seen: Option<u64> = None;
        for (key, _) in engine.scan_prefix(vector_index::NODE_PREFIX)? {
            if let Some(id) = vector_index::node_id(key.as_bytes()) {
                max_seen = Some(max_seen.map_or(id, |m| m.max(id)));
            }
        }
        for (key, _) in engine.scan_prefix(memory_index::VECMAP_PREFIX)? {
            if let Some(id) = memory_index::vecmap_id(key.as_bytes()) {
                max_seen = Some(max_seen.map_or(id, |m| m.max(id)));
            }
        }
        Ok(max_seen.map_or(0, |m| m + 1))
    }

    /// The next id [`Self::put`] will allocate. Exposed for tests and
    /// diagnostics; not the caller's to drive.
    #[must_use]
    pub fn next_vec_id(&self) -> u64 {
        self.next_vec_id
    }

    /// Inserts the memory `(agent, id)` durably and **atomically** with its
    /// vector: record block + reverse mapping + allocator bump + FTS
    /// postings/doc-terms/stats (ADR-028 §4) travel as the `extra` batch of
    /// [`PersistentVectorIndex::insert_with`] — one WAL record,
    /// all-or-nothing under a crash (ADR-027 §3). Returns the allocated
    /// vector id.
    ///
    /// Errors with [`EngineError::DuplicateMemoryId`] if a record already
    /// exists for `(agent, id)` — mirroring the libSQL UNIQUE violation,
    /// never a silent overwrite (which would strand the old record's live
    /// vector node, ADR-027 §6).
    // Composing three sibling indexes' (vector, memory, fts) crash-critical
    // writes into one atomic batch genuinely needs a handle to each — a
    // grouping struct would just rename this same argument list, not reduce it.
    #[allow(clippy::too_many_arguments)]
    pub fn put(
        &mut self,
        engine: &mut Engine,
        vectors: &mut PersistentVectorIndex,
        fts: &PersistentFts,
        agent: &str,
        id: &str,
        new: &NewMemoryRecord<'_>,
        vector: Vec<f32>,
    ) -> Result<u64> {
        // Single source of truth: a put is a one-item put_many (same
        // duplicate check, same staging, same single WAL record).
        let allocated = self.put_many(engine, vectors, fts, agent, &[(id, new.clone(), vector)])?;
        Ok(allocated[0])
    }

    /// Inserts **several** memories of one `agent` in one single atomic
    /// batch — the all-or-nothing `put_memory_batch` (N5.5), closing the
    /// per-item-atomicity gap ADR-027 §6 documented: every record block,
    /// reverse mapping, FTS staging ([`PersistentFts::stage_insert_many`],
    /// one aggregated stats record), the single final allocator bump and
    /// every vector node ride **one** WAL record via
    /// [`PersistentVectorIndex::insert_many_with`]. After a crash the whole
    /// group is visible or none of it is — the native equivalent of the
    /// multi-row libSQL transaction, at last for N > 1 too.
    ///
    /// Any error — including [`EngineError::DuplicateMemoryId`] for an
    /// existing `(agent, id)` **or a duplicate id within `items`** — is
    /// returned before anything is written. Returns the allocated vector
    /// ids, in item order. An empty `items` writes nothing.
    pub fn put_many(
        &mut self,
        engine: &mut Engine,
        vectors: &mut PersistentVectorIndex,
        fts: &PersistentFts,
        agent: &str,
        items: &[(&str, NewMemoryRecord<'_>, Vec<f32>)],
    ) -> Result<Vec<u64>> {
        if items.is_empty() {
            return Ok(Vec::new());
        }
        for (i, (id, _, _)) in items.iter().enumerate() {
            let record_key = memory_index::record_key(agent, id)?;
            if engine.get(record_key.as_bytes())?.is_some() || items[..i].iter().any(|(prior, _, _)| prior == id) {
                return Err(EngineError::DuplicateMemoryId {
                    agent: agent.to_string(),
                    id: (*id).to_string(),
                });
            }
        }

        let first_vec_id = self.next_vec_id;
        let mut extra = Batch::new();
        let mut fts_docs: Vec<(u64, &str)> = Vec::with_capacity(items.len());
        let mut vector_items: Vec<(u64, Vec<f32>)> = Vec::with_capacity(items.len());
        for (offset, (id, new, vector)) in items.iter().enumerate() {
            let vec_id = first_vec_id + offset as u64;
            let stored = MemoryRecord {
                layer: new.layer.to_string(),
                content: new.content.to_string(),
                source: new.source.to_string(),
                valid_from: new.valid_from,
                valid_until: new.valid_until,
                importance: new.importance,
                last_access: new.last_access,
                vec_id,
            };
            let mapping = VecMapEntry {
                agent: agent.to_string(),
                id: (*id).to_string(),
            };
            extra.put(
                memory_index::record_key(agent, id)?.as_bytes(),
                &record::encode(&stored)?,
            );
            extra.put(memory_index::vecmap_key(vec_id).as_bytes(), &vecmap::encode(&mapping)?);
            stage_expiry_insert(&mut extra, agent, id, new.valid_until)?;
            fts_docs.push((vec_id, new.content));
            vector_items.push((vec_id, vector.clone()));
        }
        let next_vec_id = first_vec_id + items.len() as u64;
        extra.put(memory_index::META_KEY, &meta::encode(&MemoryIndexMeta { next_vec_id })?);
        // Agent-registry marker (ADR-041 §7.5): staged with every insert
        // batch — an idempotent overwrite of an empty value, so re-inserts
        // cost one no-op-shaped op rather than a read-before-write.
        extra.put(agent_registry::agent_key(agent)?.as_bytes(), &[]);
        fts.stage_insert_many(engine, agent, &fts_docs, &mut extra)?;
        vectors.insert_many_with(engine, vector_items, &extra)?;

        self.next_vec_id = next_vec_id;
        Ok((first_vec_id..next_vec_id).collect())
    }

    /// The memory record `(agent, id)`, if any — regardless of its validity
    /// window (validity is consumer policy).
    pub fn get(&self, engine: &Engine, agent: &str, id: &str) -> Result<Option<MemoryRecord>> {
        let key = memory_index::record_key(agent, id)?;
        let Some(bytes) = engine.get(key.as_bytes())? else {
            return Ok(None);
        };
        Ok(Some(record::decode(&bytes)?))
    }

    /// Overwrites the record `(agent, id)` in place — the write path behind
    /// `invalidate` (validity rewrite) and `set_importance`. `record.vec_id`
    /// must be the stored one — this method never re-allocates.
    ///
    /// Self-sufficient for the temporal-expiry index (ADR-041 §7.2): reads
    /// the record's **previous** `valid_until` to decide whether the expiry
    /// entry must move (`invalidate` changes it), stay (`set_importance`
    /// doesn't), or be created/removed (an eternal memory gaining/losing its
    /// expiry). All of it — the record overwrite and the expiry delta —
    /// rides **one** atomic batch, never two separate writes.
    pub fn update(&self, engine: &mut Engine, agent: &str, id: &str, updated: &MemoryRecord) -> Result<()> {
        let key = memory_index::record_key(agent, id)?;
        let mut batch = Batch::new();
        batch.put(key.as_bytes(), &record::encode(updated)?);
        let previous_valid_until = self.get(engine, agent, id)?.and_then(|r| r.valid_until);
        if previous_valid_until != updated.valid_until {
            stage_expiry_delete(&mut batch, agent, id, previous_valid_until)?;
            stage_expiry_insert(&mut batch, agent, id, updated.valid_until)?;
        }
        engine.apply_batch(&batch)
    }

    /// Rewrites `last_access = now` on every existing `(agent, id)` of `ids`
    /// in **one** atomic batch; absent ids are silently skipped (mirrors the
    /// libSQL `UPDATE`'s zero-row no-op).
    pub fn touch_last_access<'a>(
        &self,
        engine: &mut Engine,
        agent: &str,
        ids: impl IntoIterator<Item = &'a str>,
        now: i64,
    ) -> Result<()> {
        let mut batch = Batch::new();
        for id in ids {
            if let Some(mut stored) = self.get(engine, agent, id)? {
                stored.last_access = now;
                let key = memory_index::record_key(agent, id)?;
                batch.put(key.as_bytes(), &record::encode(&stored)?);
            }
        }
        if batch.is_empty() {
            return Ok(());
        }
        engine.apply_batch(&batch)
    }

    /// Physically forgets the memory `(agent, id)`: record, reverse mapping
    /// and FTS postings/doc-terms/stats (ADR-028 §4) removed and the vector
    /// node tombstoned, all in **one** atomic batch via
    /// [`PersistentVectorIndex::delete_with`] (which applies the companion
    /// deletes even when the node is already gone — leftovers of an
    /// interrupted earlier attempt must not survive). Returns `false` (a
    /// no-op) when no record exists, mirroring the libSQL `DELETE`'s
    /// zero-row behavior.
    pub fn forget(
        &self,
        engine: &mut Engine,
        vectors: &mut PersistentVectorIndex,
        fts: &PersistentFts,
        agent: &str,
        id: &str,
    ) -> Result<bool> {
        let Some(stored) = self.get(engine, agent, id)? else {
            return Ok(false);
        };
        let record_key = memory_index::record_key(agent, id)?;
        let mut extra = Batch::new();
        extra.delete(record_key.as_bytes());
        extra.delete(memory_index::vecmap_key(stored.vec_id).as_bytes());
        stage_expiry_delete(&mut extra, agent, id, stored.valid_until)?;
        fts.stage_delete(engine, agent, stored.vec_id, &mut extra)?;
        vectors.delete_with(engine, stored.vec_id, &extra)?;
        Ok(true)
    }

    /// Physically forgets **several** memories of one `agent`, in bounded
    /// atomic chunks (ADR-041 §7.4): within one chunk, every record, reverse
    /// mapping, temporal-expiry entry, FTS staging (one aggregated stats
    /// record, [`PersistentFts::stage_delete_many`]) and vector tombstone
    /// (one refreshed metadata record,
    /// [`PersistentVectorIndex::delete_many_with`]) travel as **one** WAL
    /// record — never one engine transaction per memory, and never one
    /// gigantic record for the whole group either: chunks are cut at
    /// `options.max_items` memories or approximately `options.max_wal_bytes`
    /// of staged bytes, whichever comes first.
    ///
    /// Idempotent and resumable **between** chunks, atomic **within** one:
    /// absent ids (and duplicates within `ids`) are silently skipped, so a
    /// crash between two chunks re-runs to completion — each memory is
    /// either fully present or fully gone, never half-deleted (same
    /// per-boundary discipline as [`Self::purge_agent`], with the boundary
    /// widened from one memory to one chunk). Returns the number of
    /// memories removed.
    pub fn forget_many(
        &self,
        engine: &mut Engine,
        vectors: &mut PersistentVectorIndex,
        fts: &PersistentFts,
        agent: &str,
        ids: &[&str],
        options: ForgetBatchOptions,
    ) -> Result<u64> {
        let max_items = options.max_items.max(1);
        // Fixed per-chunk overhead: the vector META rewrite + the FTS stats
        // record the chunk stages once regardless of its item count.
        const CHUNK_OVERHEAD_BYTES: usize = 256;

        let mut removed = 0u64;
        let mut seen: HashSet<&str> = HashSet::with_capacity(ids.len());
        // The pending chunk: (id, vec_id, valid_until) of each memory, plus
        // its running byte estimate.
        let mut chunk: Vec<(&str, u64, Option<i64>)> = Vec::new();
        let mut chunk_bytes = CHUNK_OVERHEAD_BYTES;

        for &id in ids {
            if !seen.insert(id) {
                continue;
            }
            let Some(stored) = self.get(engine, agent, id)? else {
                // Absent (or already deleted by an earlier interrupted run):
                // skipped, never an error — this is what makes a re-run
                // resume instead of fail.
                continue;
            };
            let record_key = memory_index::record_key(agent, id)?;
            let mut item_bytes = record_key.as_bytes().len()
                + memory_index::vecmap_key(stored.vec_id).as_bytes().len()
                + vectors.approx_tombstone_wire_bytes()
                + fts.delete_footprint(engine, agent, stored.vec_id)?;
            if let Some(until) = stored.valid_until {
                item_bytes += temporal_index::expiry_key(agent, until, id)?.as_bytes().len();
            }
            if !chunk.is_empty() && (chunk.len() >= max_items || chunk_bytes + item_bytes > options.max_wal_bytes) {
                removed += Self::apply_forget_chunk(engine, vectors, *fts, agent, &chunk)?;
                chunk.clear();
                chunk_bytes = CHUNK_OVERHEAD_BYTES;
            }
            chunk.push((id, stored.vec_id, stored.valid_until));
            chunk_bytes += item_bytes;
        }
        if !chunk.is_empty() {
            removed += Self::apply_forget_chunk(engine, vectors, *fts, agent, &chunk)?;
        }
        Ok(removed)
    }

    /// Applies one [`Self::forget_many`] chunk as a single atomic batch:
    /// record + vecmap + expiry deletes for every item, one aggregated FTS
    /// staging, one vector multi-tombstone. Returns the chunk's item count.
    fn apply_forget_chunk(
        engine: &mut Engine,
        vectors: &mut PersistentVectorIndex,
        fts: PersistentFts,
        agent: &str,
        chunk: &[(&str, u64, Option<i64>)],
    ) -> Result<u64> {
        let mut extra = Batch::new();
        let mut vec_ids: Vec<u64> = Vec::with_capacity(chunk.len());
        for &(id, vec_id, valid_until) in chunk {
            extra.delete(memory_index::record_key(agent, id)?.as_bytes());
            extra.delete(memory_index::vecmap_key(vec_id).as_bytes());
            stage_expiry_delete(&mut extra, agent, id, valid_until)?;
            vec_ids.push(vec_id);
        }
        fts.stage_delete_many(engine, agent, &vec_ids, &mut extra)?;
        vectors.delete_many_with(engine, &vec_ids, &extra)?;
        Ok(chunk.len() as u64)
    }

    /// Every agent id present in the registry (ADR-041 §7.5), in ascending
    /// byte order. **Identifiers only** — never any per-agent data. An agent
    /// is registered by its first memory insert and unregistered by
    /// [`Self::purge_agent`]; forgetting an agent's last memory
    /// intentionally leaves its entry (the registry answers "who should a
    /// maintenance pass visit", and visiting an empty agent is a cheap
    /// no-op — a read-modify-check on every forget would not be). A
    /// malformed key inside the reserved keyspace is a hard error, never
    /// silently skipped.
    pub fn list_agents(&self, engine: &Engine) -> Result<Vec<String>> {
        let entries = engine.scan_prefix(agent_registry::REGISTRY_PREFIX)?;
        let mut out = Vec::with_capacity(entries.len());
        for (key, _) in entries {
            let Some(agent) = agent_registry::agent_decode(key.as_bytes()) else {
                return Err(EngineError::CorruptMemoryRecord {
                    reason: "malformed agent-registry key under the reserved meta/agents/ prefix".to_string(),
                });
            };
            out.push(agent);
        }
        Ok(out)
    }

    /// Every memory record of `agent`, as `(id, record)` pairs in ascending
    /// id-byte order (the scan order of the structural prefix). A malformed
    /// key inside the reserved keyspace is a hard error, never silently
    /// skipped — these are memories.
    pub fn scan_agent(&self, engine: &Engine, agent: &str) -> Result<Vec<(String, MemoryRecord)>> {
        let prefix = memory_index::record_agent_prefix(agent)?;
        let entries = engine.scan_prefix(&prefix)?;
        let mut out = Vec::with_capacity(entries.len());
        for (key, value) in entries {
            let Some(id) = memory_index::record_id(prefix.len(), key.as_bytes()) else {
                return Err(EngineError::CorruptMemoryRecord {
                    reason: format!("malformed record key under the (agent={agent:?}) scan prefix"),
                });
            };
            out.push((id, record::decode(&value)?));
        }
        Ok(out)
    }

    /// One page of [`Self::scan_agent`] (ADR-041 §7.3): at most `limit`
    /// records of `agent` in ascending id-byte order, strictly after
    /// `after_id` (`None` for the first page) — the bounded-memory scan
    /// behind a paged full-population maintenance pass. **A page shorter
    /// than `limit` means the agent is exhausted** (the internal loop only
    /// stops short at the end of the keyspace); a full page means "call
    /// again with `after_id` = the last returned id". `limit == 0` returns
    /// an empty page (which by that contract also reads as exhausted —
    /// callers should not pass it).
    ///
    /// Built on [`Engine::scan_range_page`], whose pages can be empty
    /// without being final (tombstone-only stretches): the loop here chains
    /// on the engine's `next_start`, so *this* method's contract stays the
    /// simple id-cursor one. Same wire-distrust rule as [`Self::scan_agent`]:
    /// a malformed key inside the reserved keyspace is a hard error.
    pub fn scan_agent_page(
        &self,
        engine: &Engine,
        agent: &str,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(String, MemoryRecord)>> {
        let prefix = memory_index::record_agent_prefix(agent)?;
        let end = memory_index::record_agent_upper_bound(agent)?;
        let mut start: Vec<u8> = match after_id {
            // Successor of the exact record key: resume strictly after it.
            Some(id) => {
                let mut key = memory_index::record_key(agent, id)?.into_bytes();
                key.push(0x00);
                key
            }
            None => prefix.clone(),
        };
        let mut out = Vec::new();
        while out.len() < limit {
            let page = engine.scan_range_page(&start, &end, limit - out.len())?;
            for (key, value) in page.entries {
                // An engine page can overshoot its limit by up to one SST
                // block (block granularity): stop at exactly `limit`, the
                // undecoded rest is re-read by the next id-cursor call.
                if out.len() == limit {
                    break;
                }
                let Some(id) = memory_index::record_id(prefix.len(), key.as_bytes()) else {
                    return Err(EngineError::CorruptMemoryRecord {
                        reason: format!("malformed record key under the (agent={agent:?}) scan range"),
                    });
                };
                out.push((id, record::decode(&value)?));
            }
            match page.next_start {
                Some(next) => start = next,
                None => break,
            }
        }
        Ok(out)
    }

    /// Every memory of `agent` whose `valid_until <= at`, as `(id,
    /// valid_until)` pairs — the temporal-expiry index query (ADR-041 §7.2)
    /// behind GC (`scan_expired`, ADR-038). Unlike [`Self::scan_agent`], this
    /// **never decodes a `MemoryRecord`**: `id` and `valid_until` are both
    /// recovered straight from the expiry-index key, and
    /// [`Engine::scan_range`] skips whole SST blocks outside `[agent_prefix,
    /// expiry_upper_bound(agent, at))` rather than reading the agent's full
    /// keyspace — the O(n) full-agent scan ADR-038 flagged as a known gap is
    /// closed here. Results come back in the index's own key order
    /// (`valid_until` ascending, then `id`) — a malformed key inside the
    /// reserved keyspace is a hard error, never silently skipped.
    pub fn scan_expiring(&self, engine: &Engine, agent: &str, at: i64) -> Result<Vec<(String, i64)>> {
        let start = temporal_index::expiry_agent_prefix(agent)?;
        let end = temporal_index::expiry_upper_bound(agent, at)?;
        let entries = engine.scan_range(&start, &end)?;
        let mut out = Vec::with_capacity(entries.len());
        for (key, _) in entries {
            let Some((valid_until, id)) = temporal_index::expiry_decode(start.len(), key.as_bytes()) else {
                return Err(EngineError::CorruptMemoryRecord {
                    reason: format!("malformed temporal expiry key under the (agent={agent:?}) scan range"),
                });
            };
            out.push((id, valid_until));
        }
        Ok(out)
    }

    /// Total number of memory records across **every** agent — a plain count
    /// over the whole reserved `idx/memory/rec/` keyspace, distinct from
    /// [`Self::scan_agent`]'s per-agent prefix. The container-level read
    /// behind a CLI/diagnostic "how many memories total" (ADR-032) — no
    /// per-record decode needed, just the entry count.
    pub fn count_all(&self, engine: &Engine) -> Result<u64> {
        Ok(engine.scan_prefix(memory_index::RECORD_PREFIX)?.len() as u64)
    }

    /// Resolves a vector-index id back to the `(agent, id)` pair owning it,
    /// or `None` for ids with no mapping (e.g. a hit whose memory was
    /// forgotten by an interrupted earlier attempt).
    pub fn resolve(&self, engine: &Engine, vec_id: u64) -> Result<Option<VecMapEntry>> {
        let Some(bytes) = engine.get(memory_index::vecmap_key(vec_id).as_bytes())? else {
            return Ok(None);
        };
        Ok(Some(vecmap::decode(&bytes)?))
    }

    /// Physically forgets **every** memory of `agent`. Per-item atomic
    /// (each memory rides its own [`Self::forget`]-shaped batch), idempotent
    /// and resumable rather than globally atomic — a crash mid-purge leaves
    /// each memory either fully present or fully gone, and re-running the
    /// purge finishes the job (ADR-027 §6). Returns the number of memories
    /// removed.
    pub fn purge_agent(
        &self,
        engine: &mut Engine,
        vectors: &mut PersistentVectorIndex,
        fts: &PersistentFts,
        agent: &str,
    ) -> Result<u64> {
        let mut purged = 0u64;
        for (id, stored) in self.scan_agent(engine, agent)? {
            let record_key = memory_index::record_key(agent, &id)?;
            let mut extra = Batch::new();
            extra.delete(record_key.as_bytes());
            extra.delete(memory_index::vecmap_key(stored.vec_id).as_bytes());
            stage_expiry_delete(&mut extra, agent, &id, stored.valid_until)?;
            fts.stage_delete(engine, agent, stored.vec_id, &mut extra)?;
            vectors.delete_with(engine, stored.vec_id, &extra)?;
            purged += 1;
        }
        // Registry entry last (ADR-041 §7.5): a crash anywhere above leaves
        // the entry in place, and re-running the purge (the documented
        // recovery, ADR-027 §6) removes it — never the reverse order, which
        // could leave un-purged memories invisible to registry consumers.
        engine.delete(agent_registry::agent_key(agent)?.as_bytes())?;
        Ok(purged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::idx::vector::VectorIndexParams;

    const DIM: usize = 8;

    fn vec_for(seed: u8) -> Vec<f32> {
        let mut v = vec![0.0_f32; DIM];
        v[usize::from(seed) % DIM] = 1.0;
        v[0] += 0.001;
        v
    }

    fn new_record<'a>(content: &'a str, layer: &'a str) -> NewMemoryRecord<'a> {
        NewMemoryRecord {
            layer,
            content,
            source: "user",
            valid_from: 0,
            valid_until: None,
            importance: 1.0,
            last_access: 0,
        }
    }

    fn open_all(dir: &std::path::Path) -> (Engine, PersistentVectorIndex, PersistentMemoryIndex, PersistentFts) {
        let mut engine = Engine::open(dir).expect("open engine");
        let vectors =
            PersistentVectorIndex::open(&mut engine, VectorIndexParams::with_dim(DIM)).expect("open vector index");
        let memory = PersistentMemoryIndex::open(&engine).expect("open memory index");
        (engine, vectors, memory, PersistentFts::new())
    }

    #[test]
    fn put_get_search_resolve_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut engine, mut vectors, mut memory, fts) = open_all(dir.path());

        let vec_id = memory
            .put(
                &mut engine,
                &mut vectors,
                &fts,
                "agent-a",
                "m1",
                &new_record("bonjour", "episodic"),
                vec_for(1),
            )
            .expect("put");
        assert_eq!(vec_id, 0);

        let stored = memory
            .get(&engine, "agent-a", "m1")
            .expect("get")
            .expect("record present");
        assert_eq!(stored.content, "bonjour");
        assert_eq!(stored.layer, "episodic");
        assert_eq!(stored.vec_id, vec_id);

        let hits = vectors.search_scored(&engine, &vec_for(1), 1).expect("search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, vec_id);
        let mapping = memory
            .resolve(&engine, hits[0].0)
            .expect("resolve")
            .expect("mapping present");
        assert_eq!(mapping.agent, "agent-a");
        assert_eq!(mapping.id, "m1");
    }

    #[test]
    fn duplicate_put_is_a_loud_error_and_writes_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut engine, mut vectors, mut memory, fts) = open_all(dir.path());

        memory
            .put(
                &mut engine,
                &mut vectors,
                &fts,
                "agent-a",
                "m1",
                &new_record("v1", "episodic"),
                vec_for(1),
            )
            .expect("first put");
        let err = memory
            .put(
                &mut engine,
                &mut vectors,
                &fts,
                "agent-a",
                "m1",
                &new_record("v2", "episodic"),
                vec_for(2),
            )
            .expect_err("duplicate must error");
        assert!(matches!(err, EngineError::DuplicateMemoryId { .. }));

        // Original content untouched, allocator advanced only once.
        let stored = memory.get(&engine, "agent-a", "m1").expect("get").expect("present");
        assert_eq!(stored.content, "v1");
        assert_eq!(memory.next_vec_id(), 1);

        // Same id under a DIFFERENT agent is fine (isolation is structural).
        memory
            .put(
                &mut engine,
                &mut vectors,
                &fts,
                "agent-b",
                "m1",
                &new_record("autre", "episodic"),
                vec_for(3),
            )
            .expect("same id, other agent");
    }

    #[test]
    fn forget_removes_record_mapping_and_search_hit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut engine, mut vectors, mut memory, fts) = open_all(dir.path());

        let vec_id = memory
            .put(
                &mut engine,
                &mut vectors,
                &fts,
                "agent-a",
                "m1",
                &new_record("x", "episodic"),
                vec_for(1),
            )
            .expect("put");

        assert!(
            memory
                .forget(&mut engine, &mut vectors, &fts, "agent-a", "m1")
                .expect("forget")
        );
        assert!(memory.get(&engine, "agent-a", "m1").expect("get").is_none());
        assert!(memory.resolve(&engine, vec_id).expect("resolve").is_none());
        assert!(
            vectors
                .search_scored(&engine, &vec_for(1), 5)
                .expect("search")
                .is_empty()
        );

        // Second forget is a no-op, not an error.
        assert!(
            !memory
                .forget(&mut engine, &mut vectors, &fts, "agent-a", "m1")
                .expect("re-forget")
        );
    }

    #[test]
    fn scan_agent_is_structurally_isolated() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut engine, mut vectors, mut memory, fts) = open_all(dir.path());

        for (agent, id, seed) in [("agent-a", "m1", 1u8), ("agent-a", "m2", 2), ("agent-b", "m3", 3)] {
            memory
                .put(
                    &mut engine,
                    &mut vectors,
                    &fts,
                    agent,
                    id,
                    &new_record(id, "semantic"),
                    vec_for(seed),
                )
                .expect("put");
        }
        let a: Vec<String> = memory
            .scan_agent(&engine, "agent-a")
            .expect("scan")
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(a, vec!["m1".to_string(), "m2".to_string()]);
        assert_eq!(memory.scan_agent(&engine, "agent-b").expect("scan").len(), 1);
        assert!(memory.scan_agent(&engine, "agent-absent").expect("scan").is_empty());
    }

    #[test]
    fn scan_agent_page_chained_pages_equal_scan_agent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut engine, mut vectors, mut memory, fts) = open_all(dir.path());

        // Two agents interleaved, a flush mid-way (SST + memtable layers)
        // and a forget (tombstone the pager must skip over, not stop at).
        for i in 0..12u8 {
            for agent in ["agent-a", "agent-b"] {
                let id = format!("m{i:02}");
                memory
                    .put(
                        &mut engine,
                        &mut vectors,
                        &fts,
                        agent,
                        &id,
                        &new_record("x", "semantic"),
                        vec_for(i),
                    )
                    .expect("put");
            }
            if i == 5 {
                engine.flush().expect("flush");
            }
        }
        memory
            .forget(&mut engine, &mut vectors, &fts, "agent-a", "m03")
            .expect("forget");

        let full: Vec<String> = memory
            .scan_agent(&engine, "agent-a")
            .expect("scan")
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(full.len(), 11);

        for limit in [1usize, 3, 11, 50] {
            let mut cursor: Option<String> = None;
            let mut paged: Vec<String> = Vec::new();
            loop {
                let page = memory
                    .scan_agent_page(&engine, "agent-a", cursor.as_deref(), limit)
                    .expect("scan page");
                let len = page.len();
                assert!(len <= limit, "a page must never exceed its limit");
                paged.extend(page.into_iter().map(|(id, _)| id));
                if len < limit {
                    break;
                }
                cursor = paged.last().cloned();
            }
            assert_eq!(paged, full, "chained pages at limit={limit} must equal scan_agent");
        }

        // The cursor contract: resuming after an id yields strictly later ids.
        let resumed = memory
            .scan_agent_page(&engine, "agent-a", Some("m05"), 100)
            .expect("scan page");
        assert!(resumed.iter().all(|(id, _)| id.as_str() > "m05"));
        assert_eq!(resumed.len(), 6);

        // Isolation: agent-b pages never see agent-a and vice versa.
        let b_page = memory
            .scan_agent_page(&engine, "agent-b", None, 100)
            .expect("scan page");
        assert_eq!(b_page.len(), 12);
        assert!(
            memory
                .scan_agent_page(&engine, "agent-absent", None, 10)
                .expect("scan page")
                .is_empty()
        );
    }

    #[test]
    fn touch_last_access_batches_and_skips_absent_ids() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut engine, mut vectors, mut memory, fts) = open_all(dir.path());

        memory
            .put(
                &mut engine,
                &mut vectors,
                &fts,
                "agent-a",
                "m1",
                &new_record("x", "episodic"),
                vec_for(1),
            )
            .expect("put");
        memory
            .touch_last_access(&mut engine, "agent-a", ["m1", "fantome"], 777)
            .expect("touch");
        let stored = memory.get(&engine, "agent-a", "m1").expect("get").expect("present");
        assert_eq!(stored.last_access, 777);
    }

    #[test]
    fn purge_agent_only_removes_that_agent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut engine, mut vectors, mut memory, fts) = open_all(dir.path());

        memory
            .put(
                &mut engine,
                &mut vectors,
                &fts,
                "agent-a",
                "m1",
                &new_record("a1", "episodic"),
                vec_for(1),
            )
            .expect("put");
        memory
            .put(
                &mut engine,
                &mut vectors,
                &fts,
                "agent-a",
                "m2",
                &new_record("a2", "episodic"),
                vec_for(2),
            )
            .expect("put");
        memory
            .put(
                &mut engine,
                &mut vectors,
                &fts,
                "agent-b",
                "m1",
                &new_record("b1", "episodic"),
                vec_for(3),
            )
            .expect("put");

        assert_eq!(
            memory
                .purge_agent(&mut engine, &mut vectors, &fts, "agent-a")
                .expect("purge"),
            2
        );
        assert!(memory.scan_agent(&engine, "agent-a").expect("scan").is_empty());
        assert_eq!(memory.scan_agent(&engine, "agent-b").expect("scan").len(), 1);
        // agent-b's memory still findable through the vector index.
        assert_eq!(vectors.search_scored(&engine, &vec_for(3), 1).expect("search").len(), 1);
    }

    #[test]
    fn allocator_is_monotonic_across_reopen_and_forgets() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let (mut engine, mut vectors, mut memory, fts) = open_all(dir.path());
            memory
                .put(
                    &mut engine,
                    &mut vectors,
                    &fts,
                    "agent-a",
                    "m1",
                    &new_record("x", "episodic"),
                    vec_for(1),
                )
                .expect("put");
            memory
                .put(
                    &mut engine,
                    &mut vectors,
                    &fts,
                    "agent-a",
                    "m2",
                    &new_record("y", "episodic"),
                    vec_for(2),
                )
                .expect("put");
            // Forget the HIGHEST id, then close: a naive max-scan allocator
            // would hand id 1 out again after the consolidate purge.
            assert!(
                memory
                    .forget(&mut engine, &mut vectors, &fts, "agent-a", "m2")
                    .expect("forget")
            );
            vectors.consolidate(&mut engine).expect("consolidate");
            engine.close().expect("close");
        }
        {
            let (mut engine, mut vectors, mut memory, fts) = open_all(dir.path());
            assert_eq!(
                memory.next_vec_id(),
                2,
                "counter must survive reopen, never re-derive lower"
            );
            let vec_id = memory
                .put(
                    &mut engine,
                    &mut vectors,
                    &fts,
                    "agent-a",
                    "m3",
                    &new_record("z", "episodic"),
                    vec_for(3),
                )
                .expect("put after reopen");
            assert_eq!(vec_id, 2);
        }
    }

    #[test]
    fn open_heals_a_missing_or_corrupt_allocator_from_the_data() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut engine, mut vectors, mut memory, fts) = open_all(dir.path());
        memory
            .put(
                &mut engine,
                &mut vectors,
                &fts,
                "agent-a",
                "m1",
                &new_record("x", "episodic"),
                vec_for(1),
            )
            .expect("put");

        // Missing: healed one past the highest observed id.
        engine.delete(memory_index::META_KEY).expect("delete meta");
        let healed = PersistentMemoryIndex::open(&engine).expect("reopen");
        assert_eq!(healed.next_vec_id(), 1);

        // Corrupt: same healing path.
        engine.put(memory_index::META_KEY, b"garbage").expect("corrupt meta");
        let healed = PersistentMemoryIndex::open(&engine).expect("reopen");
        assert_eq!(healed.next_vec_id(), 1);

        // Fresh store: allocator starts at 0.
        let fresh_dir = tempfile::tempdir().expect("tempdir");
        let fresh_engine = Engine::open(fresh_dir.path()).expect("open");
        let fresh = PersistentMemoryIndex::open(&fresh_engine).expect("open");
        assert_eq!(fresh.next_vec_id(), 0);
    }

    #[test]
    fn put_survives_close_and_reopen_byte_identically() {
        let dir = tempfile::tempdir().expect("tempdir");
        let expected = MemoryRecord {
            layer: "semantic".to_string(),
            content: "fenêtre bornée".to_string(),
            source: "consolidation".to_string(),
            valid_from: 100,
            valid_until: Some(200),
            importance: 0.5,
            last_access: 150,
            vec_id: 0,
        };
        {
            let (mut engine, mut vectors, mut memory, fts) = open_all(dir.path());
            memory
                .put(
                    &mut engine,
                    &mut vectors,
                    &fts,
                    "agent-a",
                    "m1",
                    &NewMemoryRecord {
                        layer: &expected.layer,
                        content: &expected.content,
                        source: &expected.source,
                        valid_from: expected.valid_from,
                        valid_until: expected.valid_until,
                        importance: expected.importance,
                        last_access: expected.last_access,
                    },
                    vec_for(1),
                )
                .expect("put");
            engine.close().expect("close");
        }
        let (engine, _vectors, memory, _fts) = open_all(dir.path());
        let stored = memory.get(&engine, "agent-a", "m1").expect("get").expect("present");
        assert_eq!(stored, expected);
    }

    #[test]
    fn put_many_is_one_batch_and_everything_is_queryable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut engine, mut vectors, mut memory, fts) = open_all(dir.path());

        let items = vec![
            ("m1", new_record("le chat dort", "episodic"), vec_for(1)),
            ("m2", new_record("le chien court", "episodic"), vec_for(2)),
            ("m3", new_record("chat et chien", "semantic"), vec_for(3)),
        ];
        let ids = memory
            .put_many(&mut engine, &mut vectors, &fts, "agent-a", &items)
            .expect("put_many");
        assert_eq!(ids, vec![0, 1, 2]);
        assert_eq!(memory.next_vec_id(), 3);

        // Records, mappings, vector search and FTS all see the whole group.
        for (id, expected_content) in [
            ("m1", "le chat dort"),
            ("m2", "le chien court"),
            ("m3", "chat et chien"),
        ] {
            let stored = memory.get(&engine, "agent-a", id).expect("get").expect("present");
            assert_eq!(stored.content, expected_content);
        }
        assert_eq!(vectors.len(), 3);
        assert_eq!(vectors.search_scored(&engine, &vec_for(2), 1).expect("search")[0].0, 1);
        // FTS stats aggregated once for the whole group: 3 docs, 9 terms.
        let hits = fts.search_bm25(&engine, "agent-a", r#""chat""#, 10).expect("bm25");
        assert_eq!(hits.len(), 2, "m1 et m3 contiennent 'chat'");
    }

    #[test]
    fn put_many_duplicate_anywhere_writes_nothing_at_all() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut engine, mut vectors, mut memory, fts) = open_all(dir.path());

        memory
            .put(
                &mut engine,
                &mut vectors,
                &fts,
                "agent-a",
                "existing",
                &new_record("déjà là", "episodic"),
                vec_for(9),
            )
            .expect("seed");

        // Duplicate against the store (middle item) — the valid first item
        // must NOT survive: all-or-nothing on the error path too.
        let err = memory
            .put_many(
                &mut engine,
                &mut vectors,
                &fts,
                "agent-a",
                &[
                    ("fresh-1", new_record("un", "episodic"), vec_for(1)),
                    ("existing", new_record("dup", "episodic"), vec_for(2)),
                    ("fresh-2", new_record("deux", "episodic"), vec_for(3)),
                ],
            )
            .expect_err("duplicate must error");
        assert!(matches!(err, EngineError::DuplicateMemoryId { .. }));
        assert!(memory.get(&engine, "agent-a", "fresh-1").expect("get").is_none());
        assert!(memory.get(&engine, "agent-a", "fresh-2").expect("get").is_none());
        assert_eq!(memory.next_vec_id(), 1, "allocator untouched by the failed group");
        assert_eq!(vectors.len(), 1, "no vector from the failed group");

        // Duplicate WITHIN the group: same guarantee.
        let err = memory
            .put_many(
                &mut engine,
                &mut vectors,
                &fts,
                "agent-a",
                &[
                    ("twin", new_record("a", "episodic"), vec_for(4)),
                    ("twin", new_record("b", "episodic"), vec_for(5)),
                ],
            )
            .expect_err("intra-group duplicate must error");
        assert!(matches!(err, EngineError::DuplicateMemoryId { .. }));
        assert!(memory.get(&engine, "agent-a", "twin").expect("get").is_none());
    }

    #[test]
    fn put_many_group_survives_reopen_and_empty_group_is_noop() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let (mut engine, mut vectors, mut memory, fts) = open_all(dir.path());
            assert!(
                memory
                    .put_many(&mut engine, &mut vectors, &fts, "agent-a", &[])
                    .expect("empty group")
                    .is_empty()
            );
            memory
                .put_many(
                    &mut engine,
                    &mut vectors,
                    &fts,
                    "agent-a",
                    &[
                        ("m1", new_record("un chat", "episodic"), vec_for(1)),
                        ("m2", new_record("un chien", "episodic"), vec_for(2)),
                    ],
                )
                .expect("put_many");
            engine.close().expect("close");
        }
        let (engine, vectors, memory, fts) = open_all(dir.path());
        assert!(!vectors.rebuilt_on_open(), "clean reopen after a batched insert");
        assert_eq!(memory.next_vec_id(), 2);
        assert_eq!(memory.scan_agent(&engine, "agent-a").expect("scan").len(), 2);
        assert_eq!(vectors.search_scored(&engine, &vec_for(1), 1).expect("search")[0].0, 0);
        assert_eq!(
            fts.search_bm25(&engine, "agent-a", r#""chien""#, 10)
                .expect("bm25")
                .len(),
            1
        );
    }

    #[test]
    fn put_stages_fts_atomically_and_forget_removes_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut engine, mut vectors, mut memory, fts) = open_all(dir.path());

        let vec_id = memory
            .put(
                &mut engine,
                &mut vectors,
                &fts,
                "agent-a",
                "m1",
                &new_record("le chat dort", "episodic"),
                vec_for(1),
            )
            .expect("put");

        let hits = fts.search_bm25(&engine, "agent-a", r#""chat""#, 10).expect("search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, vec_id);

        assert!(
            memory
                .forget(&mut engine, &mut vectors, &fts, "agent-a", "m1")
                .expect("forget")
        );
        assert!(
            fts.search_bm25(&engine, "agent-a", r#""chat""#, 10)
                .expect("search after forget")
                .is_empty()
        );
    }

    #[test]
    fn purge_agent_removes_fts_entries_too() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut engine, mut vectors, mut memory, fts) = open_all(dir.path());

        memory
            .put(
                &mut engine,
                &mut vectors,
                &fts,
                "agent-a",
                "m1",
                &new_record("chat", "episodic"),
                vec_for(1),
            )
            .expect("put");
        memory
            .put(
                &mut engine,
                &mut vectors,
                &fts,
                "agent-b",
                "m1",
                &new_record("chat", "episodic"),
                vec_for(2),
            )
            .expect("put");

        memory
            .purge_agent(&mut engine, &mut vectors, &fts, "agent-a")
            .expect("purge");

        assert!(
            fts.search_bm25(&engine, "agent-a", r#""chat""#, 10)
                .expect("search")
                .is_empty()
        );
        assert_eq!(
            fts.search_bm25(&engine, "agent-b", r#""chat""#, 10)
                .expect("search")
                .len(),
            1
        );
    }

    fn record_until<'a>(content: &'a str, layer: &'a str, valid_until: Option<i64>) -> NewMemoryRecord<'a> {
        NewMemoryRecord {
            valid_until,
            ..new_record(content, layer)
        }
    }

    #[test]
    fn scan_expiring_returns_only_expired_of_that_agent_within_the_window() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut engine, mut vectors, mut memory, fts) = open_all(dir.path());

        // agent-a: one eternal (no entry expected), two expiring at 100/200,
        // one already past (50).
        memory
            .put(
                &mut engine,
                &mut vectors,
                &fts,
                "agent-a",
                "eternal",
                &new_record("x", "episodic"),
                vec_for(1),
            )
            .expect("put");
        memory
            .put(
                &mut engine,
                &mut vectors,
                &fts,
                "agent-a",
                "at-100",
                &record_until("x", "episodic", Some(100)),
                vec_for(2),
            )
            .expect("put");
        memory
            .put(
                &mut engine,
                &mut vectors,
                &fts,
                "agent-a",
                "at-200",
                &record_until("x", "episodic", Some(200)),
                vec_for(3),
            )
            .expect("put");
        memory
            .put(
                &mut engine,
                &mut vectors,
                &fts,
                "agent-a",
                "at-50",
                &record_until("x", "episodic", Some(50)),
                vec_for(4),
            )
            .expect("put");
        // agent-b: same valid_until as agent-a's at-100 — must never leak
        // across the agent isolation boundary (ADR-006).
        memory
            .put(
                &mut engine,
                &mut vectors,
                &fts,
                "agent-b",
                "b-at-100",
                &record_until("x", "episodic", Some(100)),
                vec_for(5),
            )
            .expect("put");

        let mut expiring = memory.scan_expiring(&engine, "agent-a", 100).expect("scan_expiring");
        expiring.sort();
        assert_eq!(
            expiring,
            vec![("at-100".to_string(), 100), ("at-50".to_string(), 50)],
            "eternal and future (200) memories must be excluded; agent-b must never appear"
        );

        // A window covering everything picks up the last one too.
        let expiring_all = memory.scan_expiring(&engine, "agent-a", 200).expect("scan_expiring");
        assert_eq!(expiring_all.len(), 3, "at-50, at-100, at-200 — never eternal");

        // agent-b sees only its own entry.
        let b = memory.scan_expiring(&engine, "agent-b", 100).expect("scan_expiring");
        assert_eq!(b, vec![("b-at-100".to_string(), 100)]);
    }

    #[test]
    fn invalidate_moves_the_expiry_index_entry_and_forget_removes_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut engine, mut vectors, mut memory, fts) = open_all(dir.path());

        memory
            .put(
                &mut engine,
                &mut vectors,
                &fts,
                "agent-a",
                "m1",
                &new_record("x", "episodic"),
                vec_for(1),
            )
            .expect("put");
        assert!(
            memory
                .scan_expiring(&engine, "agent-a", 1_000)
                .expect("scan")
                .is_empty(),
            "eternal memory has no expiry entry yet"
        );

        // `update` (the `invalidate` write path) sets valid_until = 500.
        let mut stored = memory.get(&engine, "agent-a", "m1").expect("get").expect("present");
        stored.valid_until = Some(500);
        memory.update(&mut engine, "agent-a", "m1", &stored).expect("update");
        assert_eq!(
            memory.scan_expiring(&engine, "agent-a", 500).expect("scan"),
            vec![("m1".to_string(), 500)],
            "invalidate must create the expiry entry"
        );

        // `update` again with the SAME valid_until (the `set_importance`
        // write path) must not duplicate or corrupt the entry.
        let mut stored2 = memory.get(&engine, "agent-a", "m1").expect("get").expect("present");
        stored2.importance = 42.0;
        memory.update(&mut engine, "agent-a", "m1", &stored2).expect("update");
        assert_eq!(memory.scan_expiring(&engine, "agent-a", 500).expect("scan").len(), 1);

        // A further `update` moving valid_until must move the entry, not
        // duplicate it.
        let mut stored3 = memory.get(&engine, "agent-a", "m1").expect("get").expect("present");
        stored3.valid_until = Some(900);
        memory.update(&mut engine, "agent-a", "m1", &stored3).expect("update");
        assert!(memory.scan_expiring(&engine, "agent-a", 500).expect("scan").is_empty());
        assert_eq!(
            memory.scan_expiring(&engine, "agent-a", 900).expect("scan"),
            vec![("m1".to_string(), 900)]
        );

        // `forget` must remove the expiry entry too.
        memory
            .forget(&mut engine, &mut vectors, &fts, "agent-a", "m1")
            .expect("forget");
        assert!(memory.scan_expiring(&engine, "agent-a", 900).expect("scan").is_empty());
    }

    #[test]
    fn purge_agent_removes_expiry_entries_too() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut engine, mut vectors, mut memory, fts) = open_all(dir.path());

        memory
            .put(
                &mut engine,
                &mut vectors,
                &fts,
                "agent-a",
                "m1",
                &record_until("x", "episodic", Some(10)),
                vec_for(1),
            )
            .expect("put");
        memory
            .put(
                &mut engine,
                &mut vectors,
                &fts,
                "agent-b",
                "m1",
                &record_until("x", "episodic", Some(10)),
                vec_for(2),
            )
            .expect("put");

        memory
            .purge_agent(&mut engine, &mut vectors, &fts, "agent-a")
            .expect("purge");

        assert!(memory.scan_expiring(&engine, "agent-a", 10).expect("scan").is_empty());
        assert_eq!(
            memory.scan_expiring(&engine, "agent-b", 10).expect("scan").len(),
            1,
            "purge must stay scoped to agent-a"
        );
    }

    /// `forget_many` (ADR-041 §7.4): record + vecmap + expiry + FTS + vector
    /// tombstone all gone for every deleted memory, absent/duplicate ids
    /// silently skipped (idempotent re-run), other agents untouched — and
    /// the final state is identical whatever the chunking bounds.
    #[test]
    fn forget_many_removes_every_structure_and_is_idempotent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut engine, mut vectors, mut memory, fts) = open_all(dir.path());

        for (id, seed, until) in [
            ("m1", 1u8, Some(100)),
            ("m2", 2, None),
            ("m3", 3, Some(200)),
            ("m4", 4, None),
        ] {
            memory
                .put(
                    &mut engine,
                    &mut vectors,
                    &fts,
                    "agent-a",
                    id,
                    &record_until(&format!("contenu {id}"), "episodic", until),
                    vec_for(seed),
                )
                .expect("put");
        }
        memory
            .put(
                &mut engine,
                &mut vectors,
                &fts,
                "agent-b",
                "m1",
                &record_until("autre agent", "episodic", Some(100)),
                vec_for(5),
            )
            .expect("put");

        // m1..m3 deleted; "fantome" absent and "m1" duplicated — skipped.
        let removed = memory
            .forget_many(
                &mut engine,
                &mut vectors,
                &fts,
                "agent-a",
                &["m1", "m2", "fantome", "m3", "m1"],
                ForgetBatchOptions::default(),
            )
            .expect("forget_many");
        assert_eq!(removed, 3);

        for id in ["m1", "m2", "m3"] {
            assert!(memory.get(&engine, "agent-a", id).expect("get").is_none());
        }
        assert!(memory.get(&engine, "agent-a", "m4").expect("get").is_some());
        // Vector index: only m4 (agent-a) + m1 (agent-b) live.
        assert_eq!(vectors.len(), 2);
        // Expiry index: agent-a's entries gone, agent-b's intact.
        assert!(
            memory
                .scan_expiring(&engine, "agent-a", 1_000)
                .expect("scan")
                .is_empty()
        );
        assert_eq!(memory.scan_expiring(&engine, "agent-b", 1_000).expect("scan").len(), 1);
        // FTS: deleted contents unfindable, survivor still indexed.
        assert!(
            fts.search_bm25(&engine, "agent-a", r#""m1""#, 10)
                .expect("search")
                .is_empty()
        );
        assert_eq!(
            fts.search_bm25(&engine, "agent-a", r#""m4""#, 10)
                .expect("search")
                .len(),
            1
        );

        // Re-running the same call is a pure no-op (resume-after-crash shape).
        let removed = memory
            .forget_many(
                &mut engine,
                &mut vectors,
                &fts,
                "agent-a",
                &["m1", "m2", "m3"],
                ForgetBatchOptions::default(),
            )
            .expect("re-run");
        assert_eq!(removed, 0);
    }

    /// The chunking bounds change how many batches ship, never the result:
    /// max_items = 1, a byte budget so small every chunk is a single memory,
    /// and the defaults must all converge to the same final state.
    #[test]
    fn forget_many_result_is_independent_of_chunking_bounds() {
        for options in [
            ForgetBatchOptions::default(),
            ForgetBatchOptions {
                max_items: 1,
                max_wal_bytes: usize::MAX,
            },
            ForgetBatchOptions {
                max_items: usize::MAX,
                max_wal_bytes: 1, // every item overflows the budget: 1-item chunks
            },
            ForgetBatchOptions {
                max_items: 0, // clamped to 1
                max_wal_bytes: 0,
            },
        ] {
            let dir = tempfile::tempdir().expect("tempdir");
            let (mut engine, mut vectors, mut memory, fts) = open_all(dir.path());
            let ids: Vec<String> = (0..7).map(|i| format!("m{i}")).collect();
            for (i, id) in ids.iter().enumerate() {
                memory
                    .put(
                        &mut engine,
                        &mut vectors,
                        &fts,
                        "agent-a",
                        id,
                        &record_until("le chat dort", "episodic", (i % 2 == 0).then_some(100 + i as i64)),
                        vec_for(i as u8),
                    )
                    .expect("put");
            }
            let borrowed: Vec<&str> = ids.iter().map(String::as_str).collect();
            let removed = memory
                .forget_many(&mut engine, &mut vectors, &fts, "agent-a", &borrowed, options)
                .expect("forget_many");
            assert_eq!(removed, 7, "options {options:?}");
            assert!(memory.scan_agent(&engine, "agent-a").expect("scan").is_empty());
            assert_eq!(vectors.len(), 0);
            assert!(
                memory
                    .scan_expiring(&engine, "agent-a", i64::MAX - 1)
                    .expect("scan")
                    .is_empty()
            );
            assert!(
                fts.search_bm25(&engine, "agent-a", r#""chat""#, 10)
                    .expect("search")
                    .is_empty(),
                "FTS stats/postings must be fully unwound under options {options:?}"
            );
        }
    }

    /// `forget_many` survives a close/reopen right after a bounded run —
    /// the group's WAL records replay to the same state (no rebuild).
    #[test]
    fn forget_many_state_survives_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let (mut engine, mut vectors, mut memory, fts) = open_all(dir.path());
            for i in 0..5u8 {
                memory
                    .put(
                        &mut engine,
                        &mut vectors,
                        &fts,
                        "agent-a",
                        &format!("m{i}"),
                        &record_until("x", "episodic", Some(50)),
                        vec_for(i),
                    )
                    .expect("put");
            }
            memory
                .forget_many(
                    &mut engine,
                    &mut vectors,
                    &fts,
                    "agent-a",
                    &["m0", "m1", "m2"],
                    ForgetBatchOptions {
                        max_items: 2,
                        max_wal_bytes: usize::MAX,
                    },
                )
                .expect("forget_many");
            engine.close().expect("close");
        }
        let (engine, vectors, memory, _fts) = open_all(dir.path());
        assert!(!vectors.rebuilt_on_open(), "clean reopen after bounded batches");
        assert_eq!(memory.scan_agent(&engine, "agent-a").expect("scan").len(), 2);
        assert_eq!(vectors.len(), 2);
        assert_eq!(memory.scan_expiring(&engine, "agent-a", 50).expect("scan").len(), 2);
    }

    /// Agent registry (ADR-041 §7.5): registered by the first insert,
    /// unregistered by purge — and intentionally NOT unregistered when the
    /// last memory is forgotten (the registry answers "who should a
    /// maintenance pass visit"; an empty agent is a cheap no-op visit).
    #[test]
    fn agent_registry_tracks_inserts_and_purges_identifiers_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (mut engine, mut vectors, mut memory, fts) = open_all(dir.path());
        assert!(memory.list_agents(&engine).expect("list").is_empty());

        for (agent, seed) in [("agent-b", 1u8), ("agent-a", 2)] {
            memory
                .put(
                    &mut engine,
                    &mut vectors,
                    &fts,
                    agent,
                    "m1",
                    &new_record("x", "episodic"),
                    vec_for(seed),
                )
                .expect("put");
        }
        assert_eq!(
            memory.list_agents(&engine).expect("list"),
            vec!["agent-a".to_string(), "agent-b".to_string()],
            "ascending byte order, insertion order irrelevant"
        );

        // Re-insert for a registered agent: still exactly one entry.
        memory
            .put(
                &mut engine,
                &mut vectors,
                &fts,
                "agent-a",
                "m2",
                &new_record("y", "episodic"),
                vec_for(3),
            )
            .expect("put");
        assert_eq!(memory.list_agents(&engine).expect("list").len(), 2);

        // Forgetting an agent's LAST memory keeps it registered (documented).
        memory
            .forget(&mut engine, &mut vectors, &fts, "agent-b", "m1")
            .expect("forget");
        assert_eq!(memory.list_agents(&engine).expect("list").len(), 2);

        // Purge unregisters — and only that agent.
        memory
            .purge_agent(&mut engine, &mut vectors, &fts, "agent-b")
            .expect("purge");
        assert_eq!(memory.list_agents(&engine).expect("list"), vec!["agent-a".to_string()]);

        // The registry survives a reopen (it is ordinary KV state).
        engine.close().expect("close");
        let (engine, _vectors, memory, _fts) = open_all(dir.path());
        assert_eq!(memory.list_agents(&engine).expect("list"), vec!["agent-a".to_string()]);
    }
}
