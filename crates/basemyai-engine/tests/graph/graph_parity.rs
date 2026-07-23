//! Graph-index parity harness (N4, `docs/TODO-NATIVE-ENGINE.md`): the exact
//! scenarios of `crates/basemyai/tests/graph.rs` (multi-hop traversal,
//! agent isolation, id reuse across agents, temporal exclusion, cycle
//! termination), ported faithfully — same graphs, same depths, same
//! assertions on ids/depth/ordering/exclusion — and run first against the
//! in-RAM [`RamGraph`] harness/oracle, then against the KV-persisted
//! [`PersistentGraph`] (including a close/reopen round-trip), per the
//! "harnais d'abord, le moteur ensuite" discipline N2/N3 already applied.
//!
//! Each scenario is written once as a small closure taking a `&mut Backend`
//! trait object, then run against both flavors — this is what makes "ported
//! 1:1, not reinvented, and verified equal on both flavors" checkable rather
//! than just asserted in prose.

use basemyai_engine::idx::graph::{GraphEdgeMeta, GraphEntity, Reached};
use basemyai_engine::{Engine, PersistentGraph, RamGraph};
use tempfile::tempdir;

/// A tiny backend-agnostic seam so every scenario below is written exactly
/// once and run against both graph flavors.
trait Backend {
    fn upsert_entity(
        &mut self,
        agent: &str,
        id: &str,
        kind: &str,
        label: &str,
        valid_from: i64,
        valid_until: Option<i64>,
    );
    fn upsert_edge(&mut self, agent: &str, src: &str, relation: &str, dst: &str, weight: f64, now: i64);
    fn traverse(&mut self, agent: &str, start: &str, max_depth: u32, now: i64) -> Vec<Reached>;
}

impl Backend for RamGraph {
    fn upsert_entity(
        &mut self,
        agent: &str,
        id: &str,
        kind: &str,
        label: &str,
        valid_from: i64,
        valid_until: Option<i64>,
    ) {
        RamGraph::upsert_entity(
            self,
            agent,
            id,
            GraphEntity {
                kind: kind.to_string(),
                label: label.to_string(),
                valid_from,
                valid_until,
            },
        );
    }
    fn upsert_edge(&mut self, agent: &str, src: &str, relation: &str, dst: &str, weight: f64, now: i64) {
        RamGraph::upsert_edge(
            self,
            agent,
            src,
            relation,
            dst,
            GraphEdgeMeta {
                weight,
                valid_from: now,
                valid_until: None,
            },
        );
    }
    fn traverse(&mut self, agent: &str, start: &str, max_depth: u32, now: i64) -> Vec<Reached> {
        RamGraph::traverse(self, agent, start, max_depth, now).expect("RAM traverse never errors")
    }
}

/// Wraps a `PersistentGraph` handle together with the `Engine` it reads and
/// writes through, so it can implement the same `&mut self` shaped
/// [`Backend`] trait as [`RamGraph`].
struct PersistentBackend {
    engine: Engine,
    graph: PersistentGraph,
}

impl PersistentBackend {
    fn open(dir: &std::path::Path) -> Self {
        Self {
            engine: Engine::open(dir).expect("open engine"),
            graph: PersistentGraph::new(),
        }
    }

    /// Closes and reopens the underlying engine — proves the graph survives
    /// a close/reopen round-trip untouched (no in-RAM-only state to lose,
    /// per the module's "no metadata" design).
    fn reopen(self, dir: &std::path::Path) -> Self {
        self.engine.close().expect("close engine");
        Self::open(dir)
    }
}

impl Backend for PersistentBackend {
    fn upsert_entity(
        &mut self,
        agent: &str,
        id: &str,
        kind: &str,
        label: &str,
        valid_from: i64,
        valid_until: Option<i64>,
    ) {
        self.graph
            .upsert_entity(
                &mut self.engine,
                agent,
                id,
                GraphEntity {
                    kind: kind.to_string(),
                    label: label.to_string(),
                    valid_from,
                    valid_until,
                },
            )
            .expect("upsert_entity");
    }
    fn upsert_edge(&mut self, agent: &str, src: &str, relation: &str, dst: &str, weight: f64, now: i64) {
        self.graph
            .upsert_edge(
                &mut self.engine,
                agent,
                src,
                relation,
                dst,
                GraphEdgeMeta {
                    weight,
                    valid_from: now,
                    valid_until: None,
                },
            )
            .expect("upsert_edge");
    }
    fn traverse(&mut self, agent: &str, start: &str, max_depth: u32, now: i64) -> Vec<Reached> {
        self.graph
            .traverse(&self.engine, agent, start, max_depth, now)
            .expect("traverse")
    }
}

fn add_entity(b: &mut (impl Backend + ?Sized), agent: &str, id: &str, kind: &str, label: &str, now: i64) {
    b.upsert_entity(agent, id, kind, label, now, None);
}

// ── Ported scenarios (crates/basemyai/tests/graph.rs) ───────────────────────

/// Ports `traverses_multiple_hops`.
fn scenario_traverses_multiple_hops(b: &mut (impl Backend + ?Sized)) {
    let now = 1_000;
    add_entity(b, "a", "alice", "person", "Alice", now);
    add_entity(b, "a", "acme", "company", "Acme", now);
    add_entity(b, "a", "beta", "company", "Beta", now);
    b.upsert_edge("a", "alice", "employeur", "acme", 1.0, now);
    b.upsert_edge("a", "acme", "a_racheté", "beta", 1.0, now);

    let d1 = b.traverse("a", "alice", 1, now);
    assert_eq!(d1.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["acme"]);
    assert_eq!(d1[0].depth, 1);

    let d2 = b.traverse("a", "alice", 2, now);
    let ids: Vec<_> = d2.iter().map(|r| (r.id.as_str(), r.depth)).collect();
    assert_eq!(ids, [("acme", 1), ("beta", 2)]);
}

/// Ports `isolation_hides_other_agents_edges`.
fn scenario_isolation_hides_other_agents_edges(b: &mut (impl Backend + ?Sized)) {
    let now = 1_000;
    add_entity(b, "A", "x", "thing", "X", now);
    add_entity(b, "A", "y", "thing", "Y", now);
    b.upsert_edge("A", "x", "rel", "y", 1.0, now);

    let seen_by_b = b.traverse("B", "x", 3, now);
    assert!(seen_by_b.is_empty(), "B must not see any entity/edge of A");
}

/// Ports `agents_can_reuse_same_graph_ids_without_conflict`.
fn scenario_agents_can_reuse_same_graph_ids_without_conflict(b: &mut (impl Backend + ?Sized)) {
    let now = 1_000;
    add_entity(b, "A", "alice", "person", "Alice A", now);
    add_entity(b, "A", "acme", "company", "Acme A", now);
    b.upsert_edge("A", "alice", "works_at", "acme", 1.0, now);

    add_entity(b, "B", "alice", "person", "Alice B", now);
    add_entity(b, "B", "acme", "company", "Acme B", now);
    b.upsert_edge("B", "alice", "works_at", "acme", 1.0, now);

    let seen_by_a = b.traverse("A", "alice", 1, now);
    let seen_by_b = b.traverse("B", "alice", 1, now);
    assert_eq!(seen_by_a[0].label, "Acme A");
    assert_eq!(seen_by_b[0].label, "Acme B");
}

/// Ports `excludes_expired_entities_and_edges`.
fn scenario_excludes_expired_entities_and_edges(b: &mut (impl Backend + ?Sized)) {
    let now = 1_000;
    add_entity(b, "a", "root", "thing", "Root", now);
    add_entity(b, "a", "live", "thing", "Live", now);
    b.upsert_edge("a", "root", "rel", "live", 1.0, now);
    // Stale target: valid window already closed before `now`.
    b.upsert_entity("a", "stale", "thing", "Stale", now - 100, Some(now - 10));
    b.upsert_edge("a", "root", "rel", "stale", 1.0, now);

    let reached = b.traverse("a", "root", 2, now);
    let ids: Vec<_> = reached.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["live"], "the expired entity must not appear");
}

/// Ports `terminates_on_cycle`.
fn scenario_terminates_on_cycle(b: &mut (impl Backend + ?Sized)) {
    let now = 1_000;
    add_entity(b, "a", "a1", "thing", "A1", now);
    add_entity(b, "a", "b1", "thing", "B1", now);
    b.upsert_edge("a", "a1", "rel", "b1", 1.0, now);
    b.upsert_edge("a", "b1", "rel", "a1", 1.0, now);

    let reached = b.traverse("a", "a1", 5, now);
    assert_eq!(reached.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["b1"]);
    assert_eq!(reached[0].depth, 1);
}

/// One named scenario function, run once against a fresh backend each.
type Scenario = (&'static str, fn(&mut dyn Backend));

/// Every ported scenario, run once against a fresh backend each.
const SCENARIOS: &[Scenario] = &[
    ("traverses_multiple_hops", |b| scenario_traverses_multiple_hops(b)),
    ("isolation_hides_other_agents_edges", |b| {
        scenario_isolation_hides_other_agents_edges(b)
    }),
    ("agents_can_reuse_same_graph_ids_without_conflict", |b| {
        scenario_agents_can_reuse_same_graph_ids_without_conflict(b)
    }),
    ("excludes_expired_entities_and_edges", |b| {
        scenario_excludes_expired_entities_and_edges(b)
    }),
    ("terminates_on_cycle", |b| scenario_terminates_on_cycle(b)),
];

// ── RAM flavor: the harness/oracle, judged first ─────────────────────────────

#[test]
fn ram_graph_matches_every_ported_scenario() {
    for (name, scenario) in SCENARIOS {
        let mut backend = RamGraph::new();
        scenario(&mut backend as &mut dyn Backend);
        eprintln!("ram: {name} ok");
    }
}

// ── Persistent flavor: same scenarios, then a close/reopen round-trip ───────

#[test]
fn persistent_graph_matches_every_ported_scenario() {
    for (name, scenario) in SCENARIOS {
        let dir = tempdir().expect("tempdir");
        let mut backend = PersistentBackend::open(dir.path());
        scenario(&mut backend as &mut dyn Backend);
        eprintln!("persistent: {name} ok");
    }
}

/// The one behavior RAM structurally cannot exercise: does the traversal
/// still see the same graph after a real close + reopen (WAL replay or
/// flushed SST, whichever the engine picked)? Re-runs the multi-hop and
/// temporal-exclusion scenarios' *assertions* against a backend that was
/// closed and reopened between every write and the final traversal.
#[test]
fn persistent_graph_survives_close_reopen_round_trip() {
    let dir = tempdir().expect("tempdir");
    let now = 1_000;

    let mut backend = PersistentBackend::open(dir.path());
    add_entity(&mut backend, "a", "alice", "person", "Alice", now);
    add_entity(&mut backend, "a", "acme", "company", "Acme", now);
    add_entity(&mut backend, "a", "beta", "company", "Beta", now);
    backend.upsert_edge("a", "alice", "employeur", "acme", 1.0, now);
    backend.upsert_edge("a", "acme", "a_racheté", "beta", 1.0, now);
    backend.upsert_entity("a", "stale", "thing", "Stale", now - 100, Some(now - 10));
    backend.upsert_edge("a", "alice", "rel_stale", "stale", 1.0, now);

    let mut backend = backend.reopen(dir.path());

    let d2 = backend.traverse("a", "alice", 2, now);
    let ids: Vec<_> = d2.iter().map(|r| (r.id.as_str(), r.depth)).collect();
    assert_eq!(
        ids,
        [("acme", 1), ("beta", 2)],
        "multi-hop traversal must survive a close/reopen round-trip"
    );
    assert!(
        d2.iter().all(|r| r.id != "stale"),
        "the expired entity must stay excluded after reopen"
    );
}

/// Sanity check that a `GraphEntity`/edge round-trips through the actual
/// `Engine::get` path used by traversal, not just the encode/decode unit
/// tests in `idx::graph::{entity,edge}` — belt-and-suspenders given this
/// file is the parity gate.
#[test]
fn persistent_entity_is_byte_identical_after_reopen() {
    let dir = tempdir().expect("tempdir");
    let mut backend = PersistentBackend::open(dir.path());
    backend.upsert_entity("a", "alice", "person", "Ålice — 京都", 42, Some(999));
    let backend = backend.reopen(dir.path());

    let key = basemyai_engine::key::graph_index::entity_key("a", "alice").expect("key");
    let bytes = backend.engine.get(key.as_bytes()).expect("get").expect("present");
    let decoded: GraphEntity = basemyai_engine::idx::graph::entity::decode(&bytes).expect("decode");
    assert_eq!(decoded.kind, "person");
    assert_eq!(decoded.label, "Ålice — 京都");
    assert_eq!(decoded.valid_from, 42);
    assert_eq!(decoded.valid_until, Some(999));
}
