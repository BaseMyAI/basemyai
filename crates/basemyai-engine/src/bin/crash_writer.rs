//! Crash-consistency harness child process (N2,
//! `docs/TODO-NATIVE-ENGINE.md`).
//!
//! Opens an [`Engine`] at the given directory and writes a continuous,
//! deterministic, checksummable stream of key/value pairs (see
//! `basemyai_engine::harness`) until forcefully killed by the driver
//! (`tests/crash_consistency.rs`). Every counter is resumed across restarts
//! from a side-channel confirmation log rather than from the `Engine`
//! itself, since this crate's public API has no key-listing operation.
//!
//! Each successful write is confirmed by appending to that log and fsyncing
//! it — deliberately *not* via stdout: a piped stdout buffer has no
//! durability guarantee at the instant this process is killed, whereas an
//! fsynced file write is visible to the driver regardless of exactly when
//! the kill lands.
//!
//! Usage: `crash_writer <engine_dir> <confirm_log_path> [mode]`
//!
//! `mode` is `single` (default, omit it), `batch`, `vector`, or `graph`:
//! - `single`: one `Engine::put` per counter, confirmed one counter per log
//!   line — proves single-key durability (pre-existing).
//! - `batch`: counters are grouped into fixed-size batches (see
//!   [`basemyai_engine::harness::BATCH_SIZE`]) applied via one
//!   `Engine::apply_batch` each, confirmed one `batch <start> <end>` line per
//!   *whole batch* only after `apply_batch` returns `Ok` — proves batch
//!   all-or-nothing atomicity under a real forced kill, not just single-key
//!   durability.
//! - `vector`: the deterministic vector-index *churn* schedule
//!   ([`basemyai_engine::harness::churn_op`]) — inserts interleaved with
//!   tombstone deletes and periodic full `consolidate()` passes, one
//!   `step <n>` confirm-log line per completed op (N3, ADR-026 §3/§4).
//!   Because the schedule is a pure function of the step number, the driver
//!   can recompute exactly which ids were confirmed inserted/deleted, and
//!   which single op may have been in flight when the kill landed. Resume
//!   is idempotent by construction: an op whose effect landed but whose
//!   confirmation was lost to the kill replays as a no-op — insert comes
//!   back `DuplicateVectorId` (already durable), delete returns `false`
//!   (already tombstoned), consolidate simply re-runs (finds nothing, or
//!   finishes what the kill interrupted).
//! - `graph`: the deterministic graph-chain schedule
//!   ([`basemyai_engine::harness::graph_op`]) — entity upserts interleaved
//!   with edge upserts building a linear chain `0 -> 1 -> 2 -> ...`, one
//!   `step <n>` confirm-log line per completed op (N4). Entity `0` is
//!   seeded once, unconditionally, before the loop starts (idempotent —
//!   always the same content). Every graph mutation is a single
//!   `Engine::put` (see `idx::graph::persistent`'s module doc for why no
//!   `apply_batch` is needed), so resume is trivially idempotent: an op
//!   whose write landed but whose confirmation was lost to the kill just
//!   overwrites the same bytes again on replay.

use std::env;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};

use basemyai_engine::harness::{
    BATCH_SIZE, ChurnOp, GRAPH_AGENT, GraphOp, churn_op, encode_key, expected_value, expected_vector,
    graph_entity_kind, graph_entity_label, graph_op, vector_index_params,
};
use basemyai_engine::{Batch, Engine, EngineError, EngineOptions, PersistentGraph, PersistentVectorIndex};

fn main() {
    let mut args = env::args().skip(1);
    let engine_dir = args
        .next()
        .expect("usage: crash_writer <engine_dir> <confirm_log_path> [mode]");
    let confirm_log_path = args
        .next()
        .expect("usage: crash_writer <engine_dir> <confirm_log_path> [mode]");
    let mode = args.next().unwrap_or_else(|| "single".to_string());

    // Small thresholds: the driver only lets this process run for a few
    // hundred milliseconds per cycle, so flush/compaction must trigger often
    // within that window to actually exercise the flush/rename/truncate
    // ordering under a forced kill, not just plain WAL appends.
    let options = EngineOptions {
        memtable_flush_threshold: 32,
        compaction_sst_threshold: 3,
    };

    let mut engine = Engine::open_with_options(&engine_dir, options).unwrap_or_else(|e| {
        eprintln!("crash_writer: failed to open engine at {engine_dir}: {e}");
        std::process::exit(1);
    });

    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&confirm_log_path)
        .unwrap_or_else(|e| {
            eprintln!("crash_writer: failed to open confirm log at {confirm_log_path}: {e}");
            std::process::exit(1);
        });

    match mode.as_str() {
        "batch" => run_batch_mode(&mut engine, &mut log, &confirm_log_path),
        "single" => run_single_mode(&mut engine, &mut log, &confirm_log_path),
        "vector" => run_vector_mode(&mut engine, &mut log, &confirm_log_path),
        "graph" => run_graph_mode(&mut engine, &mut log, &confirm_log_path),
        other => {
            eprintln!("crash_writer: unknown mode {other:?} (expected \"single\", \"batch\", \"vector\" or \"graph\")");
            std::process::exit(1);
        }
    }
}

/// One `Engine::put` per counter, one confirm-log line per counter.
fn run_single_mode(engine: &mut Engine, log: &mut File, confirm_log_path: &str) -> ! {
    let start = last_confirmed_single(confirm_log_path).map_or(0, |n| n + 1);
    let mut counter = start;
    loop {
        let key = encode_key(counter);
        let value = expected_value(counter);
        if let Err(e) = engine.put(&key, &value) {
            eprintln!("crash_writer: put({counter}) failed: {e}");
            std::process::exit(1);
        }
        confirm_line(log, &counter.to_string());
        counter += 1;
    }
}

/// Fixed-size batches of counters, one `Engine::apply_batch` per batch, one
/// `batch <start> <end>` confirm-log line per *whole batch* — only written
/// after `apply_batch` returns `Ok`, i.e. only after the whole batch is
/// already durable.
fn run_batch_mode(engine: &mut Engine, log: &mut File, confirm_log_path: &str) -> ! {
    let last_confirmed_end = last_confirmed_batch_end(confirm_log_path);
    let mut start = last_confirmed_end.map_or(0, |end| end + 1);
    loop {
        let end = start + BATCH_SIZE - 1;
        let mut batch = Batch::new();
        for counter in start..=end {
            batch.put(&encode_key(counter), &expected_value(counter));
        }
        if let Err(e) = engine.apply_batch(&batch) {
            eprintln!("crash_writer: apply_batch({start}..={end}) failed: {e}");
            std::process::exit(1);
        }
        confirm_line(log, &format!("batch {start} {end}"));
        start = end + 1;
    }
}

/// One churn op per step (insert / tombstone delete / consolidate, per
/// [`churn_op`]'s pure schedule), one `step <n>` confirm-log line per op —
/// only written after the op's `apply_batch`(es) returned `Ok`. See the
/// module doc for why resume is idempotent.
fn run_vector_mode(engine: &mut Engine, log: &mut File, confirm_log_path: &str) -> ! {
    let mut index = PersistentVectorIndex::open(engine, vector_index_params()).unwrap_or_else(|e| {
        eprintln!("crash_writer: failed to open vector index: {e}");
        std::process::exit(1);
    });
    let start = last_confirmed_step(confirm_log_path).map_or(0, |s| s + 1);
    let mut step = start;
    loop {
        match churn_op(step) {
            ChurnOp::Insert { id } => match index.insert(engine, id, expected_vector(id)) {
                Ok(()) => {}
                // The previous run's kill landed between apply_batch
                // (durable) and the confirm-log write: the vector is
                // already live in the index. Confirm and move on — the
                // driver's contract is only "confirmed ⇒ durable".
                Err(EngineError::DuplicateVectorId { .. }) => {}
                Err(e) => {
                    eprintln!("crash_writer: vector insert(step {step}, id {id}) failed: {e}");
                    std::process::exit(1);
                }
            },
            // `Ok(false)` = already tombstoned (same replay reasoning as
            // the duplicate insert above) — both outcomes are confirmable.
            ChurnOp::Delete { id } => {
                if let Err(e) = index.delete(engine, id) {
                    eprintln!("crash_writer: vector delete(step {step}, id {id}) failed: {e}");
                    std::process::exit(1);
                }
            }
            // Idempotent: a re-run after a mid-consolidation kill finishes
            // (or finds nothing left to do).
            ChurnOp::Consolidate => {
                if let Err(e) = index.consolidate(engine) {
                    eprintln!("crash_writer: vector consolidate(step {step}) failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        confirm_line(log, &format!("step {step}"));
        step += 1;
    }
}

/// Deterministic entity content for the graph crash schedule.
fn graph_entity(id: u64) -> basemyai_engine::GraphEntity {
    basemyai_engine::GraphEntity {
        kind: graph_entity_kind(),
        label: graph_entity_label(id),
        valid_from: 0,
        valid_until: None,
    }
}

/// Entity/edge upserts building the linear chain `0 -> 1 -> 2 -> ...`, one
/// `step <n>` confirm-log line per completed op — see the module doc for the
/// schedule and why resume is trivially idempotent here (unlike `vector`
/// mode, there is no delete/consolidate to reason about).
fn run_graph_mode(engine: &mut Engine, log: &mut File, confirm_log_path: &str) -> ! {
    let graph = PersistentGraph::new();
    // Seed entity 0 unconditionally on every run: idempotent (always the
    // same bytes), and it is the chain's root, never produced by `graph_op`
    // itself, so it needs no resume bookkeeping of its own.
    if let Err(e) = graph.upsert_entity(engine, GRAPH_AGENT, "0", graph_entity(0)) {
        eprintln!("crash_writer: failed to seed graph entity 0: {e}");
        std::process::exit(1);
    }

    let start = last_confirmed_step(confirm_log_path).map_or(0, |s| s + 1);
    let mut step = start;
    loop {
        match graph_op(step) {
            GraphOp::UpsertEntity { id } => {
                if let Err(e) = graph.upsert_entity(engine, GRAPH_AGENT, &id.to_string(), graph_entity(id)) {
                    eprintln!("crash_writer: graph upsert_entity(step {step}, id {id}) failed: {e}");
                    std::process::exit(1);
                }
            }
            GraphOp::UpsertEdge { src, dst } => {
                let meta = basemyai_engine::GraphEdgeMeta {
                    weight: 1.0,
                    valid_from: 0,
                    valid_until: None,
                };
                if let Err(e) = graph.upsert_edge(engine, GRAPH_AGENT, &src.to_string(), "next", &dst.to_string(), meta)
                {
                    eprintln!("crash_writer: graph upsert_edge(step {step}, {src}->{dst}) failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        confirm_line(log, &format!("step {step}"));
        step += 1;
    }
}

fn confirm_line(log: &mut File, line: &str) {
    if let Err(e) = writeln!(log, "{line}") {
        eprintln!("crash_writer: confirm log write failed: {e}");
        std::process::exit(1);
    }
    if let Err(e) = log.sync_all() {
        eprintln!("crash_writer: confirm log fsync failed: {e}");
        std::process::exit(1);
    }
}

/// Reads the last well-formed counter line from a single-mode confirmation
/// log, if any. A malformed trailing line (this same process's log write
/// torn by a *previous* kill) is skipped rather than trusted — only a
/// fully-parsed line counts as confirmed.
fn last_confirmed_single(path: &str) -> Option<u64> {
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

/// Reads the last well-formed `step <n>` line from a vector-mode
/// confirmation log, if any. Same torn-trailing-line tolerance as
/// [`last_confirmed_single`].
fn last_confirmed_step(path: &str) -> Option<u64> {
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

/// Reads the `end` of the last well-formed `batch <start> <end>` line from a
/// batch-mode confirmation log, if any. Same torn-trailing-line tolerance as
/// [`last_confirmed_single`].
fn last_confirmed_batch_end(path: &str) -> Option<u64> {
    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);
    let mut last_end = None;
    for line in reader.lines().map_while(Result::ok) {
        let mut parts = line.split_whitespace();
        if parts.next() != Some("batch") {
            continue;
        }
        let (Some(start_str), Some(end_str), None) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        if let (Ok(_start), Ok(end)) = (start_str.parse::<u64>(), end_str.parse::<u64>()) {
            last_end = Some(end);
        }
    }
    last_end
}
