//! In-RAM graph store (N4): the harness/oracle flavor, judged first against
//! the ported scenarios in `tests/graph_parity.rs` — "le harnais d'abord, le
//! moteur ensuite" (`docs/TODO-NATIVE-ENGINE.md`), same discipline N2 and N3
//! already applied. Shares [`super::traverse::run`] with
//! [`super::persistent::PersistentGraph`] — only the storage differs.

use std::collections::HashMap;

use super::edge::GraphEdgeMeta;
use super::entity::GraphEntity;
use super::traverse::{self, GraphProvider, OutEdge, Reached};
use crate::error::Result;

/// Key of the entity/edge-adjacency maps below: `(agent, id)` for entities,
/// `(agent, src)` for edges.
type ScopeKey = (String, String);
/// One node's outgoing edges.
type OutEdges = Vec<OutEdge>;

/// A graph held entirely in RAM, keyed by `(agent, id)` / `(agent, src)` —
/// the same agent-scoping shape the persistent flavor's key layout encodes
/// structurally, so the exact same isolation and reuse-of-ids-across-agents
/// scenarios can be exercised against both flavors identically.
#[derive(Debug, Default)]
pub struct RamGraph {
    entities: HashMap<ScopeKey, GraphEntity>,
    edges: HashMap<ScopeKey, OutEdges>,
}

impl RamGraph {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts or overwrites the entity `(agent, id)`.
    pub fn upsert_entity(&mut self, agent: &str, id: &str, entity: GraphEntity) {
        self.entities.insert((agent.to_string(), id.to_string()), entity);
    }

    /// Inserts or overwrites the directed edge `(agent, src) --relation--> dst`
    /// (matching relation *and* dst identifies the edge to overwrite, as
    /// with the ported SQL's `ON CONFLICT(agent_id, src, dst, relation)`).
    pub fn upsert_edge(&mut self, agent: &str, src: &str, relation: &str, dst: &str, meta: GraphEdgeMeta) {
        let list = self.edges.entry((agent.to_string(), src.to_string())).or_default();
        if let Some(existing) = list.iter_mut().find(|(r, d, _)| r == relation && d == dst) {
            existing.2 = meta;
        } else {
            list.push((relation.to_string(), dst.to_string(), meta));
        }
    }

    /// Breadth-first traversal — see [`traverse::run`] for the exact
    /// semantics. Infallible in practice (a `HashMap`-backed provider never
    /// errors) but returns `Result` for API symmetry with
    /// [`super::persistent::PersistentGraph::traverse`].
    pub fn traverse(&self, agent: &str, start: &str, max_depth: u32, now: i64) -> Result<Vec<Reached>> {
        let mut provider = RamProvider { store: self };
        traverse::run(&mut provider, agent, start, max_depth, now)
    }
}

struct RamProvider<'a> {
    store: &'a RamGraph,
}

impl GraphProvider for RamProvider<'_> {
    fn entity(&mut self, agent: &str, id: &str) -> Result<Option<GraphEntity>> {
        Ok(self.store.entities.get(&(agent.to_string(), id.to_string())).cloned())
    }

    fn out_edges(&mut self, agent: &str, src: &str) -> Result<Vec<OutEdge>> {
        Ok(self
            .store
            .edges
            .get(&(agent.to_string(), src.to_string()))
            .cloned()
            .unwrap_or_default())
    }
}
