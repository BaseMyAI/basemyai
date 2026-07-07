// SPDX-License-Identifier: BUSL-1.1
//! Deterministic, checksummable key/value content shared by the
//! crash-consistency harness (N2, `docs/TODO-NATIVE-ENGINE.md`): the
//! `crash_writer` child binary (`src/bin/crash_writer.rs`) writes values
//! produced by this module, and the driver (`tests/crash_consistency.rs`)
//! recomputes the *expected* value independently from the counter alone —
//! it never trusts the writer's own claim about what it wrote, only the
//! Engine's answer compared against this pure function.
//!
//! Kept in the lib (rather than duplicated in the bin and the test) so both
//! sides can never silently drift apart on encoding.

/// Payload size in bytes for [`expected_value`].
pub const VALUE_LEN: usize = 64;

/// Fixed user key shared by the encrypted crash-consistency variant (N5.4,
/// ADR-030): the writer opens the engine with it, the driver reopens and
/// verifies with it. A constant, not a secret — the harness proves that
/// kill-time atomicity/durability survive the AEAD envelopes, not key
/// management.
pub const CRYPTO_KEY: &[u8] = b"crash-harness-encryption-key";

/// Number of keys per batch used by the batch-atomicity crash-consistency
/// harness (`src/bin/crash_writer.rs` batch mode + `tests/crash_consistency.rs`).
/// Batches are always `[k * BATCH_SIZE, k * BATCH_SIZE + BATCH_SIZE - 1]` for
/// some `k`, starting from counter 0 — the writer only ever resumes from
/// `last_confirmed + 1`, and a batch is only ever confirmed as a whole after
/// `Engine::apply_batch` returns `Ok`, so `last_confirmed + 1` is always a
/// multiple of `BATCH_SIZE`. That invariant is what lets the driver compute
/// the *next*, possibly in-flight-at-kill-time batch's key range purely from
/// the last confirmed counter, without the writer needing to log anything
/// before a batch is fully durable.
pub const BATCH_SIZE: u64 = 6;

/// Encodes `counter` as a big-endian 8-byte key. Big-endian keeps
/// lexicographic byte order equal to numeric order, which is incidental here
/// (the harness never relies on `Engine` iteration order) but keeps the
/// on-disk keys human-inspectable if this ever needs debugging.
#[must_use]
pub fn encode_key(counter: u64) -> Vec<u8> {
    counter.to_be_bytes().to_vec()
}

/// A deterministic payload derived from `counter` via a cheap multiplicative
/// hash, repeated to fill [`VALUE_LEN`] bytes. Any bit-flip, truncation, or
/// swap anywhere in the value is detectable by exact comparison against a
/// freshly recomputed [`expected_value`] — the harness never compares
/// against a value cached from before the crash.
#[must_use]
pub fn expected_value(counter: u64) -> Vec<u8> {
    let hash = counter.wrapping_mul(0x9E37_79B9_7F4A_7C15).to_be_bytes();
    let mut v = Vec::with_capacity(VALUE_LEN);
    while v.len() < VALUE_LEN {
        v.extend_from_slice(&hash);
    }
    v.truncate(VALUE_LEN);
    v
}

/// Vector dimension used by the vector-index crash-consistency mode
/// (`crash_writer` mode `vector`). Deliberately small: the harness proves
/// atomicity/durability of index batches under kill, not embedding-scale
/// performance — a small dimension lets each bounded run window insert
/// enough vectors to matter.
pub const VECTOR_DIM: usize = 32;

/// The index parameters both sides of the vector crash harness must agree
/// on (the writer to build, the driver to reopen and verify).
#[must_use]
pub fn vector_index_params() -> crate::idx::vector::VectorIndexParams {
    crate::idx::vector::VectorIndexParams::with_dim(VECTOR_DIM)
}

/// A deterministic [`VECTOR_DIM`]-dimensional vector derived from `counter`
/// alone (xorshift64* seeded by a multiplicative hash of the counter),
/// components in [-1, 1). Like [`expected_value`], the driver recomputes it
/// independently after a kill and compares against what the store returns —
/// any corruption of the persisted node block is detectable exactly.
///
/// The first component is nudged away from zero so the vector always has a
/// well-defined direction (cosine distance needs a non-zero norm).
#[must_use]
pub fn expected_vector(counter: u64) -> Vec<f32> {
    let mut state = counter
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(0x00DD_BA11)
        .max(1);
    let mut next = move || {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        let bits = (state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40) as f32; // 24 random bits
        bits / (1u64 << 23) as f32 - 1.0
    };
    let mut v: Vec<f32> = (0..VECTOR_DIM).map(|_| next()).collect();
    if v[0].abs() < 0.25 {
        v[0] = if v[0] < 0.0 { -1.0 } else { 1.0 };
    }
    v
}

/// One step of the vector-churn crash schedule (see [`churn_op`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChurnOp {
    /// Insert `expected_vector(id)` under `id`.
    Insert { id: u64 },
    /// Tombstone `id` (guaranteed by the schedule to have been inserted by
    /// an earlier step, and never deleted before).
    Delete { id: u64 },
    /// Run a full `PersistentVectorIndex::consolidate` pass.
    Consolidate,
}

/// Every [`CHURN_DELETE_PERIOD`]-th step is a delete (unless it is also a
/// consolidate step — the consolidate rule wins).
pub const CHURN_DELETE_PERIOD: u64 = 7;
/// Every [`CHURN_CONSOLIDATE_PERIOD`]-th step is a consolidation pass.
pub const CHURN_CONSOLIDATE_PERIOD: u64 = 43;

/// Number of Consolidate ops among steps `0..step`. Closed form: the
/// consolidate steps are exactly `x ≡ 42 (mod 43)`, and 42 is the largest
/// residue, so the count of such `x < step` is `step / 43`.
#[must_use]
pub fn churn_consolidates_before(step: u64) -> u64 {
    step / CHURN_CONSOLIDATE_PERIOD
}

/// Number of Delete ops among steps `0..step`. Delete steps are
/// `x ≡ 6 (mod 7)` *minus* the ones claimed by the consolidate rule
/// (`x ≡ 42 (mod 43)`, i.e. by CRT `x ≡ 300 (mod 301)`). Both 6 and 300
/// are the largest residues of their modulus, so the same closed form as
/// [`churn_consolidates_before`] applies to each term.
#[must_use]
pub fn churn_deletes_before(step: u64) -> u64 {
    step / CHURN_DELETE_PERIOD - step / (CHURN_DELETE_PERIOD * CHURN_CONSOLIDATE_PERIOD)
}

/// Number of Insert ops among steps `0..step` (everything that is neither
/// a delete nor a consolidate).
#[must_use]
pub fn churn_inserts_before(step: u64) -> u64 {
    step - churn_deletes_before(step) - churn_consolidates_before(step)
}

/// The deterministic vector-churn op for `step` — a pure function, so the
/// writer (`crash_writer` mode `vector`) can resume from the last confirmed
/// step and the driver (`tests/crash_consistency.rs`) can recompute exactly
/// which ids were confirmed inserted/deleted, including which single op may
/// have been in flight when the kill landed.
///
/// Schedule: inserts get sequential fresh ids (`churn_inserts_before`);
/// the k-th delete targets id `3k` — always already inserted (at the k-th
/// delete step `s = 7k + 6` at most, `churn_inserts_before(s) ≥ 5k + 5 >
/// 3k`), never deleted twice (targets are distinct), never re-inserted
/// (insert ids are fresh); consolidation runs periodically so kills land
/// inside it too.
#[must_use]
pub fn churn_op(step: u64) -> ChurnOp {
    if step % CHURN_CONSOLIDATE_PERIOD == CHURN_CONSOLIDATE_PERIOD - 1 {
        return ChurnOp::Consolidate;
    }
    if step % CHURN_DELETE_PERIOD == CHURN_DELETE_PERIOD - 1 {
        return ChurnOp::Delete {
            id: 3 * churn_deletes_before(step),
        };
    }
    ChurnOp::Insert {
        id: churn_inserts_before(step),
    }
}

/// Agent scope used by the graph crash-consistency harness (`crash_writer`
/// mode `graph`). A single constant, not a per-step value, keeps the
/// schedule focused on the property under test (adjacency durability under
/// kill), the same way the vector churn harness uses one fixed
/// [`VECTOR_DIM`] rather than varying it per step.
pub const GRAPH_AGENT: &str = "crash-harness";

/// One step of the deterministic graph crash schedule (see [`graph_op`]):
/// entities and edges are interleaved so a linear chain
/// `0 -> 1 -> 2 -> ...` builds up one hop at a time, confirmed durable one
/// op at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphOp {
    /// Upsert entity `id` (kind/label from [`graph_entity_kind`] /
    /// [`graph_entity_label`]).
    UpsertEntity { id: u64 },
    /// Upsert the edge `src -> dst` (always the chain's `"next"` relation).
    UpsertEdge { src: u64, dst: u64 },
}

/// The deterministic graph-chain op for `step`: even steps create entity
/// `step / 2 + 1`, odd steps link it to the previous entity
/// (`step / 2 -> step / 2 + 1`) — by the time an edge step runs, both
/// endpoints were created by earlier (already-confirmed-before-this-one)
/// steps, so a reopened store can always tell whether a given edge's
/// endpoints are themselves confirmed. Entity `0` is not produced by this
/// schedule — the writer seeds it once, unconditionally, before entering the
/// loop (see `crash_writer`'s `run_graph_mode`), since it is the same
/// idempotent content on every run and needs no resume bookkeeping.
#[must_use]
pub fn graph_op(step: u64) -> GraphOp {
    if step.is_multiple_of(2) {
        GraphOp::UpsertEntity { id: step / 2 + 1 }
    } else {
        let k = step / 2;
        GraphOp::UpsertEdge { src: k, dst: k + 1 }
    }
}

/// Fixed entity kind used by every node the graph crash harness writes.
#[must_use]
pub fn graph_entity_kind() -> String {
    "chain-node".to_string()
}

/// Deterministic label for entity `id`, independently recomputable by the
/// driver after a kill (same trust discipline as [`expected_value`]/
/// [`expected_vector`]: never compare against a value cached from before the
/// crash).
#[must_use]
pub fn graph_entity_label(id: u64) -> String {
    format!("node-{id}")
}

/// Agent scope used by the memory-triplet crash-consistency harness
/// (`crash_writer` mode `memory`, N5.5) — a single constant, same rationale
/// as [`GRAPH_AGENT`].
pub const MEMORY_AGENT: &str = "crash-harness-memory";

/// Vector dimension for the memory-triplet harness — deliberately the same
/// as [`VECTOR_DIM`] so [`expected_vector`] can be reused directly instead
/// of a second near-duplicate generator.
pub const MEMORY_VECTOR_DIM: usize = VECTOR_DIM;

/// The record id string of the `id`-th memory op — also its vec-id, since
/// [`memory_op`]'s schedule assigns sequential vec-ids to puts in the same
/// order it assigns `id`s (no forget ever consumes a vec-id, ADR-027 §4:
/// the allocator only advances on put).
#[must_use]
pub fn memory_record_id(id: u64) -> String {
    format!("m{id}")
}

/// Deterministic content for memory `id`, containing exactly one token
/// unique to `id` (`term<id>tag`, alphanumeric — never split by the
/// tokenizer's `!is_alphanumeric` boundary, ADR-028 §1) so a BM25 search for
/// that exact term is a precise, independently-recomputable oracle.
#[must_use]
pub fn expected_memory_content(id: u64) -> String {
    format!("memory harness payload term{id}tag")
}

/// The `match_expr` (already in the subset `fts_match_expr()` produces,
/// ADR-028 §1) that finds exactly the document [`expected_memory_content`]
/// produces for `id`, and no other.
#[must_use]
pub fn memory_match_expr(id: u64) -> String {
    format!(r#""term{id}tag""#)
}

/// One step of the deterministic memory-triplet crash schedule (see
/// [`memory_op`]): puts interleaved with forgets, exercising the composed
/// record+vector+FTS atomic write ([`crate::idx::memory::PersistentMemoryIndex::put`])
/// and its companion delete under a real kill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryOp {
    /// Put [`memory_record_id`]`(id)` with [`expected_memory_content`]`(id)`
    /// and `expected_vector(id)` (reusing the vector-churn generator, see
    /// [`MEMORY_VECTOR_DIM`]).
    Put { id: u64 },
    /// Forget an earlier, already-put, never-yet-forgotten id.
    Forget { id: u64 },
}

/// Every [`MEMORY_FORGET_PERIOD`]-th step is a forget.
pub const MEMORY_FORGET_PERIOD: u64 = 5;

/// Number of `Forget` ops among steps `0..step`. Same closed-form technique
/// as [`churn_deletes_before`]: forget steps are exactly `x ≡ 4 (mod 5)`,
/// the largest residue of the modulus, so the count of such `x < step` is
/// `step / 5`.
#[must_use]
pub fn memory_forgets_before(step: u64) -> u64 {
    step / MEMORY_FORGET_PERIOD
}

/// Number of `Put` ops among steps `0..step` — everything that isn't a
/// forget. Also the vec-id the *next* put will receive (ADR-027 §4: the
/// allocator only advances on put), and the count of ids the schedule has
/// assigned so far.
#[must_use]
pub fn memory_puts_before(step: u64) -> u64 {
    step - memory_forgets_before(step)
}

/// The deterministic memory-triplet op for `step`. The `k`-th forget
/// (`step = 5k + 4`) targets id `2k`: always already put
/// (`memory_puts_before(step) = 4k + 4 > 2k` for every `k ≥ 0`), never
/// forgotten twice (targets `2k` are distinct across `k`), never re-put (put
/// ids are the fresh sequential counter [`memory_puts_before`] itself).
#[must_use]
pub fn memory_op(step: u64) -> MemoryOp {
    if step % MEMORY_FORGET_PERIOD == MEMORY_FORGET_PERIOD - 1 {
        return MemoryOp::Forget {
            id: 2 * memory_forgets_before(step),
        };
    }
    MemoryOp::Put {
        id: memory_puts_before(step),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The closed-form counters must match a straight sequential replay of
    /// the schedule — they are what lets writer and driver agree on state
    /// without any shared log beyond "last confirmed step".
    #[test]
    fn churn_schedule_closed_forms_match_sequential_replay() {
        let mut inserts = 0u64;
        let mut deletes = 0u64;
        let mut consolidates = 0u64;
        let mut inserted = std::collections::HashSet::new();
        let mut deleted = std::collections::HashSet::new();
        for step in 0..5_000u64 {
            assert_eq!(churn_inserts_before(step), inserts, "inserts drift at step {step}");
            assert_eq!(churn_deletes_before(step), deletes, "deletes drift at step {step}");
            assert_eq!(
                churn_consolidates_before(step),
                consolidates,
                "consolidates drift at step {step}"
            );
            match churn_op(step) {
                ChurnOp::Insert { id } => {
                    assert_eq!(id, inserts);
                    assert!(inserted.insert(id), "insert id {id} reused at step {step}");
                    inserts += 1;
                }
                ChurnOp::Delete { id } => {
                    assert!(inserted.contains(&id), "step {step} deletes {id} before insertion");
                    assert!(deleted.insert(id), "step {step} deletes {id} twice");
                    deletes += 1;
                }
                ChurnOp::Consolidate => consolidates += 1,
            }
        }
        assert!(deletes > 0 && consolidates > 0, "schedule must exercise all op kinds");
    }

    #[test]
    fn expected_vector_is_deterministic_right_size_and_distinct() {
        assert_eq!(expected_vector(9), expected_vector(9));
        assert_eq!(expected_vector(9).len(), VECTOR_DIM);
        assert_ne!(expected_vector(1), expected_vector(2));
    }

    #[test]
    fn expected_vector_never_has_zero_norm() {
        for counter in 0..1000 {
            let v = expected_vector(counter);
            let norm: f32 = v.iter().map(|x| x * x).sum();
            assert!(norm > 0.01, "near-zero norm at counter={counter}");
        }
    }

    /// Every edge step's endpoints must already have been produced by an
    /// earlier entity step — the property `run_graph_cycles` relies on to
    /// tell whether a given edge's endpoints are themselves confirmed.
    #[test]
    fn graph_schedule_edges_always_follow_their_endpoints() {
        let mut entities_created: std::collections::HashSet<u64> = std::collections::HashSet::from([0]);
        for step in 0..2_000u64 {
            match graph_op(step) {
                GraphOp::UpsertEntity { id } => {
                    entities_created.insert(id);
                }
                GraphOp::UpsertEdge { src, dst } => {
                    assert!(
                        entities_created.contains(&src),
                        "step {step}: src {src} not yet created"
                    );
                    assert!(
                        entities_created.contains(&dst),
                        "step {step}: dst {dst} not yet created"
                    );
                }
            }
        }
    }

    #[test]
    fn graph_entity_label_is_deterministic_and_distinct() {
        assert_eq!(graph_entity_label(9), graph_entity_label(9));
        assert_ne!(graph_entity_label(1), graph_entity_label(2));
    }

    /// Same discipline as `churn_schedule_closed_forms_match_sequential_replay`:
    /// the closed-form counters must match a straight sequential replay.
    #[test]
    fn memory_schedule_closed_forms_match_sequential_replay() {
        let mut puts = 0u64;
        let mut forgets = 0u64;
        let mut put_ids = std::collections::HashSet::new();
        let mut forgotten_ids = std::collections::HashSet::new();
        for step in 0..5_000u64 {
            assert_eq!(memory_puts_before(step), puts, "puts drift at step {step}");
            assert_eq!(memory_forgets_before(step), forgets, "forgets drift at step {step}");
            match memory_op(step) {
                MemoryOp::Put { id } => {
                    assert_eq!(id, puts);
                    assert!(put_ids.insert(id), "put id {id} reused at step {step}");
                    puts += 1;
                }
                MemoryOp::Forget { id } => {
                    assert!(put_ids.contains(&id), "step {step} forgets {id} before it was put");
                    assert!(forgotten_ids.insert(id), "step {step} forgets {id} twice");
                    forgets += 1;
                }
            }
        }
        assert!(forgets > 0, "schedule must exercise forgets");
    }

    #[test]
    fn memory_record_id_and_content_are_deterministic_and_distinct() {
        assert_eq!(memory_record_id(7), memory_record_id(7));
        assert_ne!(memory_record_id(1), memory_record_id(2));
        assert_eq!(expected_memory_content(7), expected_memory_content(7));
        assert_ne!(expected_memory_content(1), expected_memory_content(2));
        assert!(expected_memory_content(42).contains("term42tag"));
        assert_eq!(memory_match_expr(42), r#""term42tag""#);
    }

    #[test]
    fn expected_value_is_deterministic_and_right_length() {
        assert_eq!(expected_value(42), expected_value(42));
        assert_eq!(expected_value(42).len(), VALUE_LEN);
    }

    #[test]
    fn different_counters_produce_different_values() {
        assert_ne!(expected_value(1), expected_value(2));
    }
}
