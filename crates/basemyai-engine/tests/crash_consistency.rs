//! Crash-consistency kill-loop harness (N2, `docs/TODO-NATIVE-ENGINE.md`):
//! "le harnais d'abord, le moteur ensuite" — this is that harness, run
//! against the store that already exists.
//!
//! Spawns the `crash_writer` child binary (writes a continuous,
//! deterministic, checksummable key/value stream into the *same*,
//! cumulative `Engine` directory across cycles), lets it run for a short
//! bounded window, then forcefully kills it — `taskkill /F /PID` on
//! Windows (this repo's CI/dev target; deliberately not relying on Unix
//! `kill -9`), `kill -9` elsewhere for portability. After each kill, it
//! reopens the `Engine` and verifies every key the child durably confirmed
//! (via a side-channel log, not stdout — see `src/bin/crash_writer.rs`) is
//! present with the exact expected value, recomputed independently from the
//! counter alone.
//!
//! Deliberately varies the run window per cycle so kills land at different
//! points relative to the flush/rename/truncate sequence (`Engine::flush`)
//! — the specific interleaving flagged as needing proof, not just reasoning
//! about: a kill between "SST renamed into place" and "WAL truncated" must
//! replay safely (same data written twice), never corrupt.
//!
//! Three variants share the spawn/kill/reopen machinery below:
//! - [`kill_reopen_verify_loop`]: single-key durability (`crash_writer`
//!   default "single" mode) — the pre-existing proof.
//! - [`batch_kill_reopen_verify_loop`]: batch *atomicity* (`crash_writer`
//!   "batch" mode) — proves that a real forced kill never leaves a batch
//!   partially applied, for or against every batch the writer attempted
//!   (confirmed or in-flight at kill time; see that test's doc comment).
//! - [`vector_kill_reopen_verify_loop`]: vector-index consistency under
//!   *churn* (`crash_writer` "vector" mode, N3/ADR-026 §3/§4) — inserts
//!   interleaved with tombstone deletes and periodic `consolidate()`
//!   passes. Proves the persistent vector index reopens **cleanly** (no
//!   rebuild) after every kill, that every durably-confirmed live vector is
//!   byte-exact and findable by `search`, and that a confirmed-deleted id
//!   **never** resurfaces — its block is either purged or tombstoned, and
//!   tombstoned blocks are excluded from results by construction.
//!
//! Wired as `cargo xtask test-crash-consistency` (its own CI job, mirroring
//! `embed`/`crypto` — heavier/slower than the default gate).

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use basemyai_engine::harness::{
    BATCH_SIZE, ChurnOp, GRAPH_AGENT, GraphOp, MEMORY_AGENT, MEMORY_VECTOR_DIM, MemoryOp, churn_deletes_before,
    churn_inserts_before, churn_op, encode_key, expected_memory_content, expected_value, expected_vector,
    graph_entity_kind, graph_entity_label, graph_op, memory_forgets_before, memory_match_expr, memory_op,
    memory_puts_before, memory_record_id, vector_index_params,
};
use basemyai_engine::idx::vector::node;
use basemyai_engine::key::vector_index::node_key;
use basemyai_engine::{
    Engine, PersistentFts, PersistentGraph, PersistentMemoryIndex, PersistentVectorIndex, VectorIndexParams,
};

/// Matches the earlier N1 spike's rigor (20 cycles) — see
/// `docs/benchmarks/n1-storage-engine-spike-2026-07-04.md`. Overridable via
/// `BASEMYAI_CRASH_CYCLES` (§8.3: the PR gate stays at the default 20 —
/// "crash smoke test" — while a nightly job can opt into a longer "crash
/// loops prolongés" run without a second copy of this file). Unset in the
/// PR gate, so its behavior there is exactly unchanged.
fn cycles() -> u32 {
    std::env::var("BASEMYAI_CRASH_CYCLES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20)
}

#[test]
fn kill_reopen_verify_loop() {
    run_cycles(cycles());
}

/// Batch-mode counterpart of [`kill_reopen_verify_loop`]: same spawn/kill
/// machinery, `crash_writer` in `"batch"` mode. See
/// [`run_batch_cycles`] for what "all-or-nothing" means here and how it's
/// checked without trusting the writer's own claims about what landed.
#[test]
fn batch_kill_reopen_verify_loop() {
    run_batch_cycles(cycles(), false);
}

/// Encrypted variant of [`batch_kill_reopen_verify_loop`] (N5.4, ADR-030):
/// the exact same batch-atomicity proof, with the engine opened via
/// `Engine::open_encrypted` under the fixed harness key. Batch mode is the
/// richest coverage per cycle for the encryption layer — every WAL batch
/// record the kill can tear is one AEAD envelope, every flush/compaction it
/// can interrupt writes sealed SSTs, and `crypto.meta` is re-read on every
/// reopen. The assertions are identical: encryption must be transparent to
/// the crash guarantees.
#[test]
fn encrypted_batch_kill_reopen_verify_loop() {
    run_batch_cycles(cycles(), true);
}

/// Vector-index counterpart (N3, ADR-026 §3/§4): same spawn/kill machinery,
/// `crash_writer` in `"vector"` (churn) mode. See [`run_vector_cycles`] for
/// exactly what is asserted after each kill.
#[test]
fn vector_kill_reopen_verify_loop() {
    run_vector_cycles(cycles());
}

/// Graph-index counterpart (N4): same spawn/kill machinery, `crash_writer`
/// in `"graph"` mode. See [`run_graph_cycles`] for exactly what is asserted
/// after each kill.
#[test]
fn graph_kill_reopen_verify_loop() {
    run_graph_cycles(cycles());
}

/// Memory-triplet counterpart (N5.5, ADR-027 §3/ADR-028 §4): same spawn/kill
/// machinery, `crash_writer` in `"memory"` mode — puts and forgets against
/// `PersistentMemoryIndex`, the composed record+vector+FTS atomic write.
/// See [`run_memory_cycles`] for exactly what is asserted after each kill.
#[test]
fn memory_kill_reopen_verify_loop() {
    run_memory_cycles(cycles(), false);
}

/// Encrypted variant (N5.4, ADR-030) of [`memory_kill_reopen_verify_loop`] —
/// the memory triplet is the mode that touches every one of the engine's
/// four logical indexes (vector, memory, FTS — plus WAL/SST underneath) per
/// op, so it is the richest per-cycle coverage for "encryption must be
/// transparent to crash guarantees" left untried by `encrypted_batch_*`.
#[test]
fn encrypted_memory_kill_reopen_verify_loop() {
    run_memory_cycles(cycles(), true);
}

/// Exposed at a small cycle count so this file can also serve as a fast
/// local smoke check; the real gate is `kill_reopen_verify_loop` /
/// `cargo xtask test-crash-consistency`.
fn run_cycles(cycles: u32) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let engine_dir = tmp.path().join("engine");
    let confirm_log = tmp.path().join("confirmed.log");
    let bin = env!("CARGO_BIN_EXE_crash_writer");

    let mut max_confirmed_seen: Option<u64> = None;

    for cycle in 0..cycles {
        let child = spawn_writer(bin, &engine_dir, &confirm_log, "single");
        kill_after_jitter(child, cycle);

        let last_confirmed = read_last_confirmed_single(&confirm_log);
        if let Some(last) = last_confirmed {
            assert!(
                max_confirmed_seen.is_none_or(|prev| last >= prev),
                "cycle {cycle}: confirmed counter regressed ({last} < {prev:?}) — the \
                 confirmation log itself should only ever grow",
                prev = max_confirmed_seen
            );
            max_confirmed_seen = Some(last);

            let engine = Engine::open(&engine_dir)
                .unwrap_or_else(|e| panic!("cycle {cycle}: engine failed to reopen after kill: {e}"));

            for counter in 0..=last {
                let key = encode_key(counter);
                let expected = expected_value(counter);
                let got = engine
                    .get(&key)
                    .unwrap_or_else(|e| panic!("cycle {cycle}: get(counter={counter}) errored: {e}"));
                assert_eq!(
                    got.as_deref(),
                    Some(expected.as_slice()),
                    "cycle {cycle}: key counter={counter} was confirmed durable by the writer \
                     before the kill, but is missing or corrupt after reopen — crash-consistency \
                     violation"
                );
            }
            engine
                .close()
                .unwrap_or_else(|e| panic!("cycle {cycle}: close after verify failed: {e}"));
        }
    }

    assert!(
        max_confirmed_seen.is_some(),
        "no cycle ever confirmed a single key — the harness or writer is broken, not the engine"
    );
}

/// Batch-mode kill loop: same spawn/sleep/kill sequence as [`run_cycles`],
/// against `crash_writer`'s `"batch"` mode instead of `"single"`.
///
/// The proof this needs to deliver is stronger than single-key durability:
/// for *every* batch the writer ever attempted — not just ones it managed to
/// log as confirmed before being killed — either all of that batch's keys
/// are present with the correct value, or none of them are. Two ranges are
/// checked each cycle:
///
/// 1. Every batch up to (and including) the last one the confirm log claims
///    was fully applied: `[0, last_confirmed_end]`. Same as the single-mode
///    check, just batch-shaped.
/// 2. The *next* batch after that — `[last_confirmed_end + 1, +BATCH_SIZE)`
///    — which the writer may have been partway through `apply_batch` for
///    when the kill landed, and which therefore never made it into the
///    confirm log at all. This is the range that actually exercises the
///    atomicity guarantee under a real kill: the driver computes its exact
///    expected keys/values from the counter alone (per
///    `basemyai_engine::harness::BATCH_SIZE`'s alignment invariant — see
///    that constant's doc comment for why this range is always known even
///    though it was never confirmed) and asserts the count of *present* keys
///    in it is either 0 or `BATCH_SIZE`, never in between, and that every
///    present one has the exact expected value.
fn run_batch_cycles(cycles: u32, encrypted: bool) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let engine_dir = tmp.path().join("engine");
    let confirm_log = tmp.path().join("confirmed.log");
    let bin = env!("CARGO_BIN_EXE_crash_writer");

    let mut max_confirmed_seen: Option<u64> = None;
    let mut any_in_flight_batch_observed = false;

    for cycle in 0..cycles {
        let child = if encrypted {
            spawn_writer_encrypted(bin, &engine_dir, &confirm_log, "batch")
        } else {
            spawn_writer(bin, &engine_dir, &confirm_log, "batch")
        };
        kill_after_jitter(child, cycle);

        let last_confirmed_end = read_last_confirmed_batch_end(&confirm_log);

        if let Some(end) = last_confirmed_end {
            assert!(
                max_confirmed_seen.is_none_or(|prev| end >= prev),
                "cycle {cycle}: confirmed batch end regressed ({end} < {prev:?}) — the \
                 confirmation log itself should only ever grow",
                prev = max_confirmed_seen
            );
            assert_eq!(
                (end + 1) % BATCH_SIZE,
                0,
                "cycle {cycle}: confirmed batch end {end} is not batch-aligned — the writer's \
                 batching invariant (every confirmed end is a multiple of BATCH_SIZE minus one) \
                 is broken, which would invalidate this test's ability to compute the next \
                 batch's range"
            );
            max_confirmed_seen = Some(end);

            let engine = if encrypted {
                Engine::open_encrypted(&engine_dir, basemyai_engine::harness::CRYPTO_KEY)
            } else {
                Engine::open(&engine_dir)
            }
            .unwrap_or_else(|e| panic!("cycle {cycle}: engine failed to reopen after kill: {e}"));

            // 1. Every confirmed batch must be fully present.
            for counter in 0..=end {
                assert_key_present_and_correct(&engine, counter, cycle, "confirmed batch");
            }

            // 2. The next (possibly in-flight-at-kill-time, never confirmed)
            //    batch must be all-or-nothing.
            let next_start = end + 1;
            let next_end = next_start + BATCH_SIZE - 1;
            let present_count = (next_start..=next_end)
                .filter(|&counter| {
                    let key = encode_key(counter);
                    engine
                        .get(&key)
                        .unwrap_or_else(|e| panic!("cycle {cycle}: get(counter={counter}) errored: {e}"))
                        .is_some()
                })
                .count();
            assert!(
                present_count == 0 || present_count as u64 == BATCH_SIZE,
                "cycle {cycle}: batch [{next_start}, {next_end}] was torn by the kill — \
                 {present_count} of {BATCH_SIZE} keys present, expected 0 (batch never applied) \
                 or {BATCH_SIZE} (batch fully applied, just not yet confirmed) — crash-consistency \
                 violation: a partial batch survived a real forced kill"
            );
            if present_count as u64 == BATCH_SIZE {
                any_in_flight_batch_observed = true;
                for counter in next_start..=next_end {
                    assert_key_present_and_correct(&engine, counter, cycle, "in-flight (uncommitted-log) batch");
                }
            }

            engine
                .close()
                .unwrap_or_else(|e| panic!("cycle {cycle}: close after verify failed: {e}"));
        }
    }

    assert!(
        max_confirmed_seen.is_some(),
        "no cycle ever confirmed a single batch — the harness or writer is broken, not the engine"
    );
    // Not a correctness assertion (both outcomes are valid depending on
    // exactly when the kill landed) — just visibility into whether this run
    // actually exercised the interesting "fully applied but not yet logged"
    // case, or only ever killed before/after batch boundaries.
    eprintln!(
        "batch_kill_reopen_verify_loop: in-flight-batch-survived-a-kill case observed = {any_in_flight_batch_observed}"
    );
}

/// Vector-churn kill loop: same spawn/sleep/kill sequence as [`run_cycles`],
/// against `crash_writer`'s `"vector"` mode — the deterministic churn
/// schedule of `harness::churn_op` (inserts + tombstone deletes + periodic
/// `consolidate()` passes), so kills land inside deletes and inside
/// consolidations, not just inserts.
///
/// The schedule is a pure function of the step number and the writer
/// confirms one `step <n>` line per completed op, so from the last
/// confirmed step `S` alone the driver recomputes, without trusting the
/// writer: the exact set of confirmed-inserted ids, the exact set of
/// confirmed-deleted ids, and the single op (`S + 1`) that may have been in
/// flight — durable or not, but never partially — when the kill landed.
/// After each kill the driver:
///
/// 1. Reopens the engine AND the persistent vector index, asserting the
///    open is **clean** — `rebuilt_on_open() == false`. Inserts and deletes
///    are single atomic batches; consolidation keeps the metadata
///    consistent at every intermediate step (repairs → meta re-anchor →
///    purge) — so the rebuild escape hatch must never trigger from a crash
///    alone, even one landing mid-consolidation.
/// 2. Asserts the index's live count matches the replayed schedule, ± the
///    possibly-in-flight op's effect.
/// 3. **Confirmed-deleted ids never resurface** — checked exhaustively at
///    the block level: the id's block is either gone (purged by a
///    consolidation) or still present but tombstoned (`deleted == true`,
///    with the original vector byte-exact). Since `search` filters
///    tombstones and skips missing blocks by construction, block-level
///    tombstone/absence *implies* the id can never be returned; a bounded
///    deterministic sample additionally exercises real `search` calls over
///    each sampled deleted id's exact vector and asserts it is absent from
///    the top-10.
/// 4. Every confirmed-live id's block is present, live, and byte-exact
///    (exhaustive); a bounded deterministic sample (the newest
///    [`VECTOR_SEARCH_RECENT`] live ids — nearest the kill window — plus an
///    evenly-strided [`VECTOR_SEARCH_SAMPLE`]-point sweep of the rest) must
///    be findable by `search` over its exact vector.
fn run_vector_cycles(cycles: u32) {
    const VECTOR_SEARCH_RECENT: usize = 20;
    const VECTOR_SEARCH_SAMPLE: usize = 50;
    const VECTOR_DELETED_SEARCH_SAMPLE: usize = 30;

    let tmp = tempfile::tempdir().expect("tempdir");
    let engine_dir = tmp.path().join("engine");
    let confirm_log = tmp.path().join("confirmed.log");
    let bin = env!("CARGO_BIN_EXE_crash_writer");

    let mut max_confirmed_seen: Option<u64> = None;
    let mut kills_inside_consolidate = 0u32;
    let mut kills_inside_delete = 0u32;

    for cycle in 0..cycles {
        let child = spawn_writer(bin, &engine_dir, &confirm_log, "vector");
        kill_after_jitter(child, cycle);

        let Some(last_step) = read_last_confirmed_step(&confirm_log) else {
            continue;
        };
        assert!(
            max_confirmed_seen.is_none_or(|prev| last_step >= prev),
            "cycle {cycle}: confirmed step regressed ({last_step} < {prev:?})",
            prev = max_confirmed_seen
        );
        max_confirmed_seen = Some(last_step);

        // Replay the pure schedule up to (and including) the last confirmed
        // step; the op at `last_step + 1` may or may not have landed.
        let confirmed_inserts = churn_inserts_before(last_step + 1);
        let confirmed_deleted: HashSet<u64> = (0..churn_deletes_before(last_step + 1)).map(|k| 3 * k).collect();
        let in_flight = churn_op(last_step + 1);
        match in_flight {
            ChurnOp::Consolidate => kills_inside_consolidate += 1,
            ChurnOp::Delete { .. } => kills_inside_delete += 1,
            ChurnOp::Insert { .. } => {}
        }

        let mut engine = Engine::open(&engine_dir)
            .unwrap_or_else(|e| panic!("cycle {cycle}: engine failed to reopen after kill: {e}"));
        let index = PersistentVectorIndex::open(&mut engine, vector_index_params())
            .unwrap_or_else(|e| panic!("cycle {cycle}: vector index failed to reopen after kill: {e}"));
        assert!(
            !index.rebuilt_on_open(),
            "cycle {cycle}: index needed a rebuild after a kill (in-flight op: {in_flight:?}) — \
             inserts/deletes are atomic batches and consolidation keeps metadata consistent at \
             every step (ADR-026 §3/§4), the rebuild escape hatch must never trigger from a \
             crash alone"
        );

        // (2) Live count matches the replayed schedule ± the in-flight op.
        let expected_live = confirmed_inserts - confirmed_deleted.len() as u64;
        let tolerance_ok = match in_flight {
            ChurnOp::Insert { .. } => index.len() == expected_live || index.len() == expected_live + 1,
            ChurnOp::Delete { .. } => index.len() == expected_live || index.len() + 1 == expected_live,
            ChurnOp::Consolidate => index.len() == expected_live,
        };
        assert!(
            tolerance_ok,
            "cycle {cycle}: index live count {} does not match the replayed schedule \
             (expected {expected_live} ± the in-flight {in_flight:?})",
            index.len()
        );

        // (3) Confirmed-deleted ids: exhaustively purged-or-tombstoned at
        //     the block level (which by construction implies they can never
        //     come out of `search`)...
        for &id in &confirmed_deleted {
            let block = engine
                .get(node_key(id).as_bytes())
                .unwrap_or_else(|e| panic!("cycle {cycle}: get(deleted node {id}) errored: {e}"));
            if let Some(bytes) = block {
                let decoded = node::decode(&bytes)
                    .unwrap_or_else(|e| panic!("cycle {cycle}: deleted node block {id} corrupt after reopen: {e}"));
                assert!(
                    decoded.deleted,
                    "cycle {cycle}: id {id} was confirmed deleted before the kill but its block \
                     reopened LIVE — a confirmed delete was lost (crash-consistency violation)"
                );
                assert_eq!(
                    decoded.vector,
                    expected_vector(id),
                    "cycle {cycle}: tombstoned block {id} holds a different vector than what was \
                     inserted — corruption"
                );
            } // None = physically purged by a consolidation pass: equally final.
        }
        //     ...plus a bounded sample of real searches over their exact
        //     vectors: never in the top-10.
        let mut deleted_sorted: Vec<u64> = confirmed_deleted.iter().copied().collect();
        deleted_sorted.sort_unstable();
        for &id in sample_ids(&deleted_sorted, 0, VECTOR_DELETED_SEARCH_SAMPLE) {
            let results = index
                .search(&engine, &expected_vector(id), 10)
                .unwrap_or_else(|e| panic!("cycle {cycle}: search(deleted vector {id}) errored: {e}"));
            assert!(
                !results.contains(&id),
                "cycle {cycle}: confirmed-deleted id {id} RESURFACED in search results \
                 {results:?} — crash-consistency violation"
            );
        }

        // (4) Confirmed-live ids: exhaustive block byte-exactness, sampled
        //     searchability. The in-flight op's ids are excluded: an
        //     unconfirmed insert may legitimately be absent, an unconfirmed
        //     delete's target may legitimately already be tombstoned.
        let in_flight_id = match in_flight {
            ChurnOp::Insert { id } | ChurnOp::Delete { id } => Some(id),
            ChurnOp::Consolidate => None,
        };
        let live_ids: Vec<u64> = (0..confirmed_inserts)
            .filter(|id| !confirmed_deleted.contains(id) && Some(*id) != in_flight_id)
            .collect();
        for &id in &live_ids {
            let bytes = engine
                .get(node_key(id).as_bytes())
                .unwrap_or_else(|e| panic!("cycle {cycle}: get(node {id}) errored: {e}"))
                .unwrap_or_else(|| {
                    panic!(
                        "cycle {cycle}: vector {id} was confirmed durable (and never deleted) \
                         before the kill but its node block is missing after reopen — \
                         crash-consistency violation"
                    )
                });
            let decoded = node::decode(&bytes)
                .unwrap_or_else(|e| panic!("cycle {cycle}: node block {id} corrupt after reopen: {e}"));
            assert!(
                !decoded.deleted,
                "cycle {cycle}: live id {id} reopened TOMBSTONED — a delete it never received \
                 was applied (crash-consistency violation)"
            );
            assert_eq!(
                decoded.vector,
                expected_vector(id),
                "cycle {cycle}: node block {id} holds a different vector than what was \
                 confirmed durable — crash-consistency violation"
            );
        }
        for &id in sample_ids(&live_ids, VECTOR_SEARCH_RECENT, VECTOR_SEARCH_SAMPLE) {
            let results = index
                .search(&engine, &expected_vector(id), 10)
                .unwrap_or_else(|e| panic!("cycle {cycle}: search(vector {id}) errored: {e}"));
            assert!(
                results.contains(&id),
                "cycle {cycle}: vector {id} was confirmed durable but search over its exact \
                 vector does not return it (top-10: {results:?}) — the reopened graph lost it"
            );
        }

        engine
            .close()
            .unwrap_or_else(|e| panic!("cycle {cycle}: close after verify failed: {e}"));
    }

    assert!(
        max_confirmed_seen.is_some(),
        "no cycle ever confirmed a single step — the harness or writer is broken, not the engine"
    );
    let last = max_confirmed_seen.unwrap_or(0);
    eprintln!(
        "vector_kill_reopen_verify_loop: {} steps confirmed over {cycles} cycles \
         ({} inserts, {} deletes, {} consolidations); kills landed with a delete in flight \
         {kills_inside_delete}x and a consolidation in flight {kills_inside_consolidate}x",
        last + 1,
        churn_inserts_before(last + 1),
        churn_deletes_before(last + 1),
        basemyai_engine::harness::churn_consolidates_before(last + 1),
    );
}

/// Graph-chain kill loop: same spawn/sleep/kill sequence as [`run_cycles`],
/// against `crash_writer`'s `"graph"` mode — the deterministic chain
/// schedule of `harness::graph_op` (entity upserts interleaved with edge
/// upserts building `0 -> 1 -> 2 -> ...`).
///
/// The schedule is a pure function of the step number and the writer
/// confirms one `step <n>` line per completed op, so from the last
/// confirmed step `S` alone the driver recomputes, without trusting the
/// writer: the highest confirmed entity id, the highest confirmed edge, and
/// the single op (`S + 1`) that may have been in flight — durable or not,
/// but never partially, since every graph mutation is one `Engine::put`
/// (`idx::graph::persistent`'s module doc) — when the kill landed. After
/// each kill the driver:
///
/// 1. Reopens the engine, decodes entity `0` (the seed) and every confirmed
///    entity/edge block directly (`Engine::get`), asserting byte-exact
///    content — the same "never trust the writer's own claim, recompute and
///    compare" discipline as [`assert_key_present_and_correct`].
/// 2. Runs real `PersistentGraph::traverse` calls from entity `0` over a
///    bounded, deterministic sample of the confirmed chain, asserting the
///    expected id appears at its expected depth — proving the persisted
///    adjacency (not just isolated blocks) survives the kill.
fn run_graph_cycles(cycles: u32) {
    const TRAVERSE_SAMPLE: usize = 25;

    let tmp = tempfile::tempdir().expect("tempdir");
    let engine_dir = tmp.path().join("engine");
    let confirm_log = tmp.path().join("confirmed.log");
    let bin = env!("CARGO_BIN_EXE_crash_writer");

    let mut max_confirmed_seen: Option<u64> = None;
    let graph = PersistentGraph::new();

    for cycle in 0..cycles {
        let child = spawn_writer(bin, &engine_dir, &confirm_log, "graph");
        kill_after_jitter(child, cycle);

        let Some(last_step) = read_last_confirmed_step(&confirm_log) else {
            continue;
        };
        assert!(
            max_confirmed_seen.is_none_or(|prev| last_step >= prev),
            "cycle {cycle}: confirmed step regressed ({last_step} < {prev:?})",
            prev = max_confirmed_seen
        );
        max_confirmed_seen = Some(last_step);

        // Replay the pure schedule up to (and including) the last confirmed
        // step to find the highest confirmed entity id and edge.
        let mut max_entity_id: u64 = 0; // the seed, always present
        let mut max_edge_dst: u64 = 0; // highest dst confirmed linked from its src
        for step in 0..=last_step {
            match graph_op(step) {
                GraphOp::UpsertEntity { id } => max_entity_id = max_entity_id.max(id),
                GraphOp::UpsertEdge { dst, .. } => max_edge_dst = max_edge_dst.max(dst),
            }
        }
        let in_flight = graph_op(last_step + 1);

        let engine = Engine::open(&engine_dir)
            .unwrap_or_else(|e| panic!("cycle {cycle}: engine failed to reopen after kill: {e}"));

        // (1) Every confirmed entity, 0..=max_entity_id, byte-exact.
        for id in 0..=max_entity_id {
            let key = basemyai_engine::key::graph_index::entity_key(GRAPH_AGENT, &id.to_string()).expect("key");
            let bytes = engine
                .get(key.as_bytes())
                .unwrap_or_else(|e| panic!("cycle {cycle}: get(entity {id}) errored: {e}"))
                .unwrap_or_else(|| {
                    panic!(
                        "cycle {cycle}: entity {id} was confirmed durable before the kill but is \
                         missing after reopen — crash-consistency violation"
                    )
                });
            let decoded = basemyai_engine::idx::graph::entity::decode(&bytes)
                .unwrap_or_else(|e| panic!("cycle {cycle}: entity block {id} corrupt after reopen: {e}"));
            assert_eq!(decoded.kind, graph_entity_kind());
            assert_eq!(
                decoded.label,
                graph_entity_label(id),
                "cycle {cycle}: entity {id} holds a different label than what was confirmed durable"
            );
        }

        // (1b) Every confirmed edge `k -> k+1` for k in 0..max_edge_dst, byte-exact.
        for dst in 1..=max_edge_dst {
            let src = dst - 1;
            let key =
                basemyai_engine::key::graph_index::edge_key(GRAPH_AGENT, &src.to_string(), "next", &dst.to_string())
                    .expect("key");
            let bytes = engine
                .get(key.as_bytes())
                .unwrap_or_else(|e| panic!("cycle {cycle}: get(edge {src}->{dst}) errored: {e}"))
                .unwrap_or_else(|| {
                    panic!(
                        "cycle {cycle}: edge {src}->{dst} was confirmed durable before the kill but \
                         is missing after reopen — crash-consistency violation"
                    )
                });
            basemyai_engine::idx::graph::edge::decode(&bytes)
                .unwrap_or_else(|e| panic!("cycle {cycle}: edge block {src}->{dst} corrupt after reopen: {e}"));
        }

        // (2) Real traversal over a bounded sample of the confirmed chain,
        //     from the root: entity `k` must be found at depth `k`.
        if max_edge_dst > 0 {
            let now = 0;
            let all_ids: Vec<u64> = (0..=max_edge_dst).collect();
            let sample: Vec<u64> = sample_ids(&all_ids, 5, TRAVERSE_SAMPLE).into_iter().copied().collect();
            let max_probe_depth = *sample.iter().max().unwrap_or(&0);
            let reached = graph
                .traverse(
                    &engine,
                    GRAPH_AGENT,
                    "0",
                    u32::try_from(max_probe_depth).unwrap_or(u32::MAX),
                    now,
                )
                .unwrap_or_else(|e| panic!("cycle {cycle}: traverse errored: {e}"));
            let depth_of: std::collections::HashMap<u64, u32> = reached
                .iter()
                .filter_map(|r| r.id.parse::<u64>().ok().map(|id| (id, r.depth)))
                .collect();
            for &id in &sample {
                if id == 0 {
                    continue; // the start is never in the traversal's own output
                }
                let got = depth_of.get(&id).copied();
                assert_eq!(
                    got,
                    Some(u32::try_from(id).unwrap_or(u32::MAX)),
                    "cycle {cycle}: confirmed chain node {id} not reachable at its expected depth \
                     via traverse (got {got:?}) — the persisted adjacency lost a durable edge"
                );
            }
        }

        engine
            .close()
            .unwrap_or_else(|e| panic!("cycle {cycle}: close after verify failed: {e}"));

        let _ = in_flight; // recomputed for readability/symmetry with the vector loop; not asserted on directly
    }

    assert!(
        max_confirmed_seen.is_some(),
        "no cycle ever confirmed a single step — the harness or writer is broken, not the engine"
    );
    eprintln!(
        "graph_kill_reopen_verify_loop: {} steps confirmed over {cycles} cycles",
        max_confirmed_seen.unwrap_or(0) + 1
    );
}

/// Memory-triplet kill loop (N5.5): same spawn/sleep/kill sequence as
/// [`run_cycles`], against `crash_writer`'s `"memory"` mode — the
/// deterministic put/forget schedule of `harness::memory_op`, which
/// exercises `PersistentMemoryIndex::put`'s composed record+vector+FTS
/// atomic write (ADR-027 §3, ADR-028 §4) under a real kill, not just the
/// vector index alone (`vector` mode) or a synthetic key/value stream
/// (`batch` mode).
///
/// The schedule is a pure function of the step number and the writer
/// confirms one `step <n>` line per completed op, so from the last
/// confirmed step `S` alone the driver recomputes, without trusting the
/// writer: the exact set of confirmed-put ids, the exact set of
/// confirmed-forgotten ids, and the single op (`S + 1`) that may have been
/// in flight — durable or not, but never partially — when the kill landed.
/// After each kill the driver:
///
/// 1. Reopens the engine (encrypted under `harness::CRYPTO_KEY` when
///    `encrypted`) and the persistent vector index, asserting the open is
///    **clean** — `rebuilt_on_open() == false`: every put/forget is a single
///    atomic batch (this test's whole point), so the rebuild escape hatch
///    must never trigger from a crash alone.
/// 2. For every confirmed-put, non-forgotten, non-in-flight id: the memory
///    record is present with the exact expected content and `vec_id`, the
///    reverse mapping resolves, a vector search over its exact vector finds
///    it, and a BM25 search for its unique term finds it — the whole
///    triplet, not just one index.
/// 3. For every confirmed-forgotten id: the memory record is gone, the
///    reverse mapping is gone, it never resurfaces in a vector search over
///    its exact vector, and its BM25 term returns nothing — the delete's
///    companion removals (ADR-027 §3/ADR-028 §4) all landed together.
/// 4. The single possibly-in-flight id (whichever op `S + 1` would have
///    been) is excluded from both checks — its outcome is genuinely
///    ambiguous (may or may not have landed before the kill).
fn run_memory_cycles(cycles: u32, encrypted: bool) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let engine_dir = tmp.path().join("engine");
    let confirm_log = tmp.path().join("confirmed.log");
    let bin = env!("CARGO_BIN_EXE_crash_writer");

    let mut max_confirmed_seen: Option<u64> = None;

    for cycle in 0..cycles {
        let child = if encrypted {
            spawn_writer_encrypted(bin, &engine_dir, &confirm_log, "memory")
        } else {
            spawn_writer(bin, &engine_dir, &confirm_log, "memory")
        };
        kill_after_jitter(child, cycle);

        let Some(last_step) = read_last_confirmed_step(&confirm_log) else {
            continue;
        };
        assert!(
            max_confirmed_seen.is_none_or(|prev| last_step >= prev),
            "cycle {cycle}: confirmed step regressed ({last_step} < {prev:?})",
            prev = max_confirmed_seen
        );
        max_confirmed_seen = Some(last_step);

        let confirmed_puts = memory_puts_before(last_step + 1);
        let confirmed_forgotten: HashSet<u64> = (0..memory_forgets_before(last_step + 1)).map(|k| 2 * k).collect();
        let in_flight = memory_op(last_step + 1);
        let (ambiguous_id, in_flight_is_forget) = match in_flight {
            MemoryOp::Put { id } => (id, false),
            MemoryOp::Forget { id } => (id, true),
        };

        let mut engine = if encrypted {
            Engine::open_encrypted(&engine_dir, basemyai_engine::harness::CRYPTO_KEY)
        } else {
            Engine::open(&engine_dir)
        }
        .unwrap_or_else(|e| panic!("cycle {cycle}: engine failed to reopen after kill: {e}"));
        let vectors = PersistentVectorIndex::open(&mut engine, VectorIndexParams::with_dim(MEMORY_VECTOR_DIM))
            .unwrap_or_else(|e| panic!("cycle {cycle}: vector index failed to reopen after kill: {e}"));
        assert!(
            !vectors.rebuilt_on_open(),
            "cycle {cycle}: vector index needed a rebuild after a kill (in-flight op: {in_flight:?}) — \
             every memory put/forget is one atomic batch, the rebuild escape hatch must never \
             trigger from a crash alone"
        );
        let memory = PersistentMemoryIndex::open(&engine)
            .unwrap_or_else(|e| panic!("cycle {cycle}: memory index failed to reopen after kill: {e}"));
        let fts = PersistentFts::new();

        for id in 0..confirmed_puts {
            if id == ambiguous_id {
                continue; // genuinely ambiguous — may or may not have landed
            }
            let record_id = memory_record_id(id);
            if confirmed_forgotten.contains(&id) {
                assert!(
                    memory
                        .get(&engine, MEMORY_AGENT, &record_id)
                        .unwrap_or_else(|e| panic!("cycle {cycle}: get(forgotten {id}) errored: {e}"))
                        .is_none(),
                    "cycle {cycle}: id {id} was confirmed forgotten before the kill but its record \
                     reopened present — crash-consistency violation"
                );
                assert!(
                    memory
                        .resolve(&engine, id)
                        .unwrap_or_else(|e| panic!("cycle {cycle}: resolve(forgotten {id}) errored: {e}"))
                        .is_none(),
                    "cycle {cycle}: id {id}'s reverse mapping survived a confirmed forget"
                );
                let hits = vectors
                    .search(&engine, &expected_vector(id), 10)
                    .unwrap_or_else(|e| panic!("cycle {cycle}: search(forgotten vector {id}) errored: {e}"));
                assert!(
                    !hits.contains(&id),
                    "cycle {cycle}: confirmed-forgotten id {id} RESURFACED in vector search {hits:?}"
                );
                let bm25 = fts
                    .search_bm25(&engine, MEMORY_AGENT, &memory_match_expr(id), 10)
                    .unwrap_or_else(|e| panic!("cycle {cycle}: bm25(forgotten {id}) errored: {e}"));
                assert!(
                    bm25.is_empty(),
                    "cycle {cycle}: confirmed-forgotten id {id}'s FTS entry survived: {bm25:?}"
                );
            } else {
                let record = memory
                    .get(&engine, MEMORY_AGENT, &record_id)
                    .unwrap_or_else(|e| panic!("cycle {cycle}: get(live {id}) errored: {e}"))
                    .unwrap_or_else(|| {
                        panic!(
                            "cycle {cycle}: id {id} was confirmed put (and never forgotten) before \
                             the kill but its record is missing after reopen — crash-consistency \
                             violation"
                        )
                    });
                assert_eq!(
                    record.content,
                    expected_memory_content(id),
                    "cycle {cycle}: id {id}'s content diverged from what was confirmed durable"
                );
                assert_eq!(record.vec_id, id, "cycle {cycle}: id {id}'s vec_id diverged");
                let mapping = memory
                    .resolve(&engine, id)
                    .unwrap_or_else(|e| panic!("cycle {cycle}: resolve(live {id}) errored: {e}"))
                    .unwrap_or_else(|| panic!("cycle {cycle}: id {id}'s reverse mapping is missing after reopen"));
                assert_eq!(mapping.id, record_id);
                assert_eq!(mapping.agent, MEMORY_AGENT);
                let hits = vectors
                    .search(&engine, &expected_vector(id), 10)
                    .unwrap_or_else(|e| panic!("cycle {cycle}: search(live vector {id}) errored: {e}"));
                assert!(
                    hits.contains(&id),
                    "cycle {cycle}: id {id} was confirmed durable but a vector search over its \
                     exact vector does not return it (top-10: {hits:?})"
                );
                let bm25 = fts
                    .search_bm25(&engine, MEMORY_AGENT, &memory_match_expr(id), 10)
                    .unwrap_or_else(|e| panic!("cycle {cycle}: bm25(live {id}) errored: {e}"));
                assert!(
                    bm25.iter().any(|&(hit_id, _)| hit_id == id),
                    "cycle {cycle}: id {id} was confirmed durable but its unique FTS term does not \
                     find it (hits: {bm25:?})"
                );
            }
        }

        let _ = in_flight_is_forget; // recomputed for readability/symmetry; not asserted on directly
        engine
            .close()
            .unwrap_or_else(|e| panic!("cycle {cycle}: close after verify failed: {e}"));
    }

    assert!(
        max_confirmed_seen.is_some(),
        "no cycle ever confirmed a single step — the harness or writer is broken, not the engine"
    );
    eprintln!(
        "{}memory_kill_reopen_verify_loop: {} steps confirmed over {cycles} cycles",
        if encrypted { "encrypted_" } else { "" },
        max_confirmed_seen.unwrap_or(0) + 1
    );
}

/// Bounded deterministic sample of a sorted id slice: the `recent` newest
/// entries plus an evenly-strided `spread`-point sweep of the older range.
/// Returns references into `ids` (no allocation beyond the output Vec).
fn sample_ids(ids: &[u64], recent: usize, spread: usize) -> Vec<&u64> {
    if ids.is_empty() {
        return Vec::new();
    }
    let recent_from = ids.len().saturating_sub(recent);
    let stride = (recent_from / spread.max(1)).max(1);
    ids[..recent_from]
        .iter()
        .step_by(stride)
        .chain(ids[recent_from..].iter())
        .collect()
}

fn assert_key_present_and_correct(engine: &Engine, counter: u64, cycle: u32, context: &str) {
    let key = encode_key(counter);
    let expected = expected_value(counter);
    let got = engine
        .get(&key)
        .unwrap_or_else(|e| panic!("cycle {cycle}: get(counter={counter}) errored: {e}"));
    assert_eq!(
        got.as_deref(),
        Some(expected.as_slice()),
        "cycle {cycle}: key counter={counter} ({context}) is missing or corrupt after reopen — \
         crash-consistency violation"
    );
}

fn spawn_writer(bin: &str, engine_dir: &Path, confirm_log: &Path, mode: &str) -> Child {
    Command::new(bin)
        .arg(engine_dir)
        .arg(confirm_log)
        .arg(mode)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("failed to spawn crash_writer in {mode} mode: {e}"))
}

/// [`spawn_writer`] with the `--encrypted` flag (N5.4, ADR-030): the writer
/// opens the engine under the fixed harness key.
fn spawn_writer_encrypted(bin: &str, engine_dir: &Path, confirm_log: &Path, mode: &str) -> Child {
    Command::new(bin)
        .arg(engine_dir)
        .arg(confirm_log)
        .arg(mode)
        .arg("--encrypted")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("failed to spawn encrypted crash_writer in {mode} mode: {e}"))
}

fn kill_after_jitter(mut child: Child, cycle: u32) {
    let sleep_ms = 150 + (jitter(cycle) % 400);
    std::thread::sleep(Duration::from_millis(sleep_ms));
    kill_forcefully(child.id());
    let _ = child.wait();
}

/// Deterministic-ish per-cycle jitter (no extra `rand` dependency): mixes
/// the cycle index with the current time so cycles don't all sleep for
/// exactly the same duration, without needing external randomness.
fn jitter(cycle: u32) -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    u64::from(cycle)
        .wrapping_mul(2_654_435_761)
        .wrapping_add(u64::from(nanos))
}

fn read_last_confirmed_single(path: &Path) -> Option<u64> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);
    let mut last = None;
    for line in reader.lines().map_while(Result::ok) {
        if let Ok(n) = line.trim().parse::<u64>() {
            last = Some(n);
        }
    }
    last
}

/// Reads the `end` of the last well-formed `batch <start> <end>` line — same
/// parsing as `crash_writer`'s own `last_confirmed_batch_end`, kept
/// independently here rather than shared: the driver deliberately never
/// trusts the writer's internals, only its output, the same way it never
/// trusts the writer's claim about what value it wrote (see
/// `assert_key_present_and_correct`, which recomputes `expected_value`
/// itself).
fn read_last_confirmed_batch_end(path: &Path) -> Option<u64> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);
    let mut last_end = None;
    for line in reader.lines().map_while(Result::ok) {
        let mut parts = line.split_whitespace();
        if parts.next() != Some("batch") {
            continue;
        }
        let (Some(_start_str), Some(end_str), None) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        if let Ok(end) = end_str.parse::<u64>() {
            last_end = Some(end);
        }
    }
    last_end
}

/// Reads the last well-formed `step <n>` line — kept independent of the
/// writer's own parser for the same trust reasons as
/// [`read_last_confirmed_batch_end`].
fn read_last_confirmed_step(path: &Path) -> Option<u64> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);
    let mut last = None;
    for line in reader.lines().map_while(Result::ok) {
        let mut parts = line.split_whitespace();
        if parts.next() != Some("step") {
            continue;
        }
        let (Some(step_str), None) = (parts.next(), parts.next()) else {
            continue;
        };
        if let Ok(step) = step_str.parse::<u64>() {
            last = Some(step);
        }
    }
    last
}

#[cfg(windows)]
fn kill_forcefully(pid: u32) {
    let _ = Command::new("taskkill")
        .args(["/F", "/PID", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(unix)]
fn kill_forcefully(pid: u32) {
    let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
}

#[cfg(not(any(windows, unix)))]
fn kill_forcefully(_pid: u32) {
    compile_error!("crash-consistency harness needs a forceful kill primitive for this platform");
}
