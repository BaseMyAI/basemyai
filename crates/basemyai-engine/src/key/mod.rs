//! Keyspace encoding (Layer 1 per `docs/PLAN-NATIVE-ENGINE.md` §3.1).
//!
//! `basemyai-engine` has no notion of "entity" yet — that meaning belongs to
//! whatever consumes this crate later (mirrors the `basemyai-core`
//! agnosticism rule: mechanism here, sense at the caller). [`Key`] is a thin,
//! byte-ordered wrapper so entity-specific encoders (e.g. a future
//! `key::memory::episode(..)` module, once a consumer exists) can share one
//! representation without this crate inventing domain concepts it doesn't
//! need yet.

/// An opaque, byte-comparable key. Ordering is lexicographic on the raw
/// bytes — this is load-bearing: the memtable and SST files both rely on
/// `Key`'s `Ord` impl to keep entries sorted.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Key(Vec<u8>);

impl Key {
    /// Builds a key from raw bytes. No encoding is imposed — callers own
    /// their own key layout until entity-specific encoders exist.
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into())
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}

impl From<&[u8]> for Key {
    fn from(bytes: &[u8]) -> Self {
        Self(bytes.to_vec())
    }
}

impl From<Vec<u8>> for Key {
    fn from(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
}

impl AsRef<[u8]> for Key {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// Keyspace reserved for the native vector index (ADR-026 §Décision 2: the
/// index lives in a dedicated `idx/vector/` prefix of the KV store, never in
/// a sidecar file).
///
/// This is the first *reserved* prefix in the engine's otherwise opaque
/// keyspace: consumers own their key layout, but MUST NOT write keys starting
/// with `idx/vector/` themselves — those belong to
/// [`crate::idx::vector::PersistentVectorIndex`]. The prefix is plain ASCII
/// (human-inspectable in dumps) and does not perturb the ordering of any
/// existing keys — `Key` ordering stays plain lexicographic bytes.
///
/// Layout:
/// - `idx/vector/meta` — the single index-metadata record
///   ([`crate::idx::vector::meta`], `VectorIndexMeta:1` in `format.lock`).
/// - `idx/vector/node/<id: u64 BE>` — one node block per vector
///   ([`crate::idx::vector::node`], `VectorNode:1`). Big-endian ids keep
///   numeric order equal to byte order under [`NODE_PREFIX`], so a prefix
///   scan yields nodes in ascending id order.
///
/// Note `meta` (`m`) sorts before `node/` (`n`), so the whole index occupies
/// one contiguous `idx/vector/` key range.
pub mod vector_index {
    use super::Key;

    /// Prefix every vector-index key starts with (reserved, see module doc).
    pub const INDEX_PREFIX: &[u8] = b"idx/vector/";
    /// Prefix of all node-block keys.
    pub const NODE_PREFIX: &[u8] = b"idx/vector/node/";
    /// The exact key of the index-metadata record.
    pub const META_KEY: &[u8] = b"idx/vector/meta";

    /// Key of the node block for vector `id`.
    #[must_use]
    pub fn node_key(id: u64) -> Key {
        let mut bytes = Vec::with_capacity(NODE_PREFIX.len() + 8);
        bytes.extend_from_slice(NODE_PREFIX);
        bytes.extend_from_slice(&id.to_be_bytes());
        Key::new(bytes)
    }

    /// Key of the index-metadata record.
    #[must_use]
    pub fn meta_key() -> Key {
        Key::new(META_KEY)
    }

    /// Inverse of [`node_key`]: extracts the vector id from a node-block
    /// key's bytes. Returns `None` for keys outside the node keyspace or
    /// with a malformed suffix.
    #[must_use]
    pub fn node_id(key_bytes: &[u8]) -> Option<u64> {
        let suffix = key_bytes.strip_prefix(NODE_PREFIX)?;
        let raw: [u8; 8] = suffix.try_into().ok()?;
        Some(u64::from_be_bytes(raw))
    }
}

/// Keyspace reserved for the native graph index (N4,
/// `docs/TODO-NATIVE-ENGINE.md` — Couche 3 per `docs/PLAN-NATIVE-ENGINE.md`
/// §2). Like [`vector_index`], this is a *reserved* prefix: consumers must
/// not write keys starting with `idx/graph/` themselves.
///
/// Layout:
/// - `idx/graph/entity/<agent_len: u32 BE><agent><id>` — one entity block per
///   `(agent, id)` ([`crate::idx::graph::entity`], `GraphEntity:1`). The
///   `u32` length prefix on `agent` is load-bearing: without it, agent
///   `"ab"` + id `"c"` and agent `"a"` + id `"bc"` would collide on the raw
///   concatenation `"abc"` — the length prefix fixes the agent/id boundary
///   unambiguously regardless of what bytes either string contains. `id` is
///   the unbounded remainder (no further prefix needed: nothing is encoded
///   after it), which also means `entity_agent_prefix(agent)` prefix-scans
///   every entity of that agent.
/// - `idx/graph/edge/<agent_len: u32 BE><agent><src_len: u32 BE><src><relation_len: u32 BE><relation><dst>` —
///   one edge record per `(agent, src, relation, dst)`
///   ([`crate::idx::graph::edge`], `GraphEdge:1`). `dst` and `relation` are
///   folded into the key (not the value) so that
///   `edge_src_prefix(agent, src)` prefix-scans **every outgoing edge of one
///   node** directly — this is the adjacency-list access pattern the BFS
///   traversal (`idx::graph::traverse`) needs on every hop, and it comes for
///   free from key ordering, no secondary index required.
///
/// Isolation by agent is therefore structural, not an applicative filter
/// bolted on after a broader read: every lookup and every scan is already
/// bounded to one agent (and, for edges, one source node) by the key prefix
/// itself.
pub mod graph_index {
    use super::Key;
    use crate::error::{EngineError, Result};

    /// Prefix every graph-index key starts with (reserved, see module doc).
    pub const INDEX_PREFIX: &[u8] = b"idx/graph/";
    /// Prefix of all entity-block keys.
    pub const ENTITY_PREFIX: &[u8] = b"idx/graph/entity/";
    /// Prefix of all edge-record keys.
    pub const EDGE_PREFIX: &[u8] = b"idx/graph/edge/";

    /// Appends a `u32`-length-prefixed field to `buf`. Returns
    /// [`EngineError::GraphKeyTooLong`] instead of silently truncating the
    /// length — a truncated prefix would desynchronize every field encoded
    /// after it, not just misencode this one.
    fn push_len_prefixed(buf: &mut Vec<u8>, field: &'static str, bytes: &[u8]) -> Result<()> {
        let len = u32::try_from(bytes.len()).map_err(|_| EngineError::GraphKeyTooLong {
            field,
            len: bytes.len(),
        })?;
        buf.extend_from_slice(&len.to_be_bytes());
        buf.extend_from_slice(bytes);
        Ok(())
    }

    /// Prefix scanning every entity of `agent`. Not used by the BFS
    /// traversal today (it only ever does point lookups of ids it already
    /// has from edges) — kept for API completeness/symmetry and any future
    /// "list all entities of an agent" consumer.
    pub fn entity_agent_prefix(agent: &str) -> Result<Vec<u8>> {
        let mut buf = Vec::with_capacity(ENTITY_PREFIX.len() + 4 + agent.len());
        buf.extend_from_slice(ENTITY_PREFIX);
        push_len_prefixed(&mut buf, "agent", agent.as_bytes())?;
        Ok(buf)
    }

    /// Key of the entity block for `(agent, id)`.
    pub fn entity_key(agent: &str, id: &str) -> Result<Key> {
        let mut buf = entity_agent_prefix(agent)?;
        buf.extend_from_slice(id.as_bytes());
        Ok(Key::new(buf))
    }

    /// Extracts the entity `id` from a full entity key, given the byte
    /// length of the exact agent prefix it was scanned under (i.e.
    /// `entity_agent_prefix(agent).len()`). Returns `None` for malformed or
    /// foreign keys — same wire-distrust discipline as
    /// [`edge_relation_dst`].
    #[must_use]
    pub fn entity_id(prefix_len: usize, key_bytes: &[u8]) -> Option<String> {
        if !key_bytes.starts_with(ENTITY_PREFIX) || key_bytes.len() < prefix_len {
            return None;
        }
        let suffix = key_bytes.get(prefix_len..)?;
        String::from_utf8(suffix.to_vec()).ok()
    }

    /// Prefix scanning every edge of `agent`, regardless of source node —
    /// the whole-agent read behind `purge_agent` (a per-source
    /// [`edge_src_prefix`] scan can't enumerate sources it doesn't know).
    pub fn edge_agent_prefix(agent: &str) -> Result<Vec<u8>> {
        let mut buf = Vec::with_capacity(EDGE_PREFIX.len() + 4 + agent.len());
        buf.extend_from_slice(EDGE_PREFIX);
        push_len_prefixed(&mut buf, "agent", agent.as_bytes())?;
        Ok(buf)
    }

    /// Prefix scanning every outgoing edge of `(agent, src)`, regardless of
    /// relation or destination — the adjacency-list read the BFS traversal
    /// does on every hop.
    pub fn edge_src_prefix(agent: &str, src: &str) -> Result<Vec<u8>> {
        let mut buf = Vec::with_capacity(EDGE_PREFIX.len() + 8 + agent.len() + src.len());
        buf.extend_from_slice(EDGE_PREFIX);
        push_len_prefixed(&mut buf, "agent", agent.as_bytes())?;
        push_len_prefixed(&mut buf, "src", src.as_bytes())?;
        Ok(buf)
    }

    /// Full key of one edge `(agent, src, relation, dst)`.
    pub fn edge_key(agent: &str, src: &str, relation: &str, dst: &str) -> Result<Key> {
        let mut buf = edge_src_prefix(agent, src)?;
        push_len_prefixed(&mut buf, "relation", relation.as_bytes())?;
        buf.extend_from_slice(dst.as_bytes());
        Ok(Key::new(buf))
    }

    /// Extracts `(relation, dst)` from a full edge key, given the byte
    /// length of the exact `(agent, src)` prefix it was scanned under (i.e.
    /// `edge_src_prefix(agent, src).len()`).
    ///
    /// N2/N3 fuzzing lesson applied here too: `relation_len` is a
    /// wire-controlled count read from the key bytes, bounded against the
    /// actual remaining length before any slicing — a malformed or
    /// truncated key returns `None` rather than panicking. The store's own
    /// writes never produce these, but nothing about reading a `Vec<u8>`
    /// back from the KV layer should be trusted more than any other decode
    /// path.
    #[must_use]
    pub fn edge_relation_dst(prefix_len: usize, key_bytes: &[u8]) -> Option<(String, String)> {
        let suffix = key_bytes.get(prefix_len..)?;
        let len_bytes: [u8; 4] = suffix.get(0..4)?.try_into().ok()?;
        let relation_len = u32::from_be_bytes(len_bytes) as usize;
        let rest = suffix.get(4..)?;
        let relation_bytes = rest.get(..relation_len)?;
        let dst_bytes = rest.get(relation_len..)?;
        let relation = String::from_utf8(relation_bytes.to_vec()).ok()?;
        let dst = String::from_utf8(dst_bytes.to_vec()).ok()?;
        Some((relation, dst))
    }
}

/// Keyspace reserved for the native memory index (N5.1, ADR-027 §2). Like
/// [`vector_index`] and [`graph_index`], this is a *reserved* prefix:
/// consumers must not write keys starting with `idx/memory/` themselves —
/// they go through [`crate::idx::memory::PersistentMemoryIndex`].
///
/// Layout:
/// - `idx/memory/meta` — the single allocator-metadata record
///   ([`crate::idx::memory::meta`], `MemoryIndexMeta:1`): the monotonic
///   `next_vec_id` counter (never reused, ADR-027 §4).
/// - `idx/memory/rec/<agent_len: u32 BE><agent><id>` — one memory record per
///   `(agent, id)` ([`crate::idx::memory::record`], `MemoryRecord:1`). Same
///   length-prefix rationale as `graph_index`: without it, agent `"ab"` +
///   id `"c"` and agent `"a"` + id `"bc"` would collide. `id` is the
///   unbounded remainder, so [`memory_index::record_agent_prefix`]
///   prefix-scans every memory of one agent — isolation is structural.
/// - `idx/memory/vecmap/<vec_id: u64 BE>` — the reverse mapping from a
///   vector-index id back to `(agent, id)`
///   ([`crate::idx::memory::vecmap`], `MemoryVecMap:1`), resolved on every
///   search hit.
pub mod memory_index {
    use super::Key;
    use crate::error::{EngineError, Result};

    /// Prefix every memory-index key starts with (reserved, see module doc).
    pub const INDEX_PREFIX: &[u8] = b"idx/memory/";
    /// Prefix of all memory-record keys.
    pub const RECORD_PREFIX: &[u8] = b"idx/memory/rec/";
    /// Prefix of all vector-id mapping keys.
    pub const VECMAP_PREFIX: &[u8] = b"idx/memory/vecmap/";
    /// The exact key of the allocator-metadata record.
    pub const META_KEY: &[u8] = b"idx/memory/meta";

    /// Appends a `u32`-length-prefixed field to `buf` — same contract as
    /// `graph_index`'s helper, with this index's own error variant.
    fn push_len_prefixed(buf: &mut Vec<u8>, field: &'static str, bytes: &[u8]) -> Result<()> {
        let len = u32::try_from(bytes.len()).map_err(|_| EngineError::MemoryKeyTooLong {
            field,
            len: bytes.len(),
        })?;
        buf.extend_from_slice(&len.to_be_bytes());
        buf.extend_from_slice(bytes);
        Ok(())
    }

    /// Prefix scanning every memory record of `agent` — the structural
    /// isolation read behind `agent_stats`, `recent_episodes`,
    /// `exact_fact_exists` and `purge_agent`.
    pub fn record_agent_prefix(agent: &str) -> Result<Vec<u8>> {
        let mut buf = Vec::with_capacity(RECORD_PREFIX.len() + 4 + agent.len());
        buf.extend_from_slice(RECORD_PREFIX);
        push_len_prefixed(&mut buf, "agent", agent.as_bytes())?;
        Ok(buf)
    }

    /// Key of the memory record for `(agent, id)`.
    pub fn record_key(agent: &str, id: &str) -> Result<Key> {
        let mut buf = record_agent_prefix(agent)?;
        buf.extend_from_slice(id.as_bytes());
        Ok(Key::new(buf))
    }

    /// Extracts the memory `id` from a full record key, given the byte
    /// length of the exact agent prefix it was scanned under (i.e.
    /// `record_agent_prefix(agent).len()`). Returns `None` for malformed or
    /// foreign keys — same wire-distrust discipline as
    /// `graph_index::edge_relation_dst`.
    #[must_use]
    pub fn record_id(prefix_len: usize, key_bytes: &[u8]) -> Option<String> {
        if !key_bytes.starts_with(RECORD_PREFIX) || key_bytes.len() < prefix_len {
            return None;
        }
        let suffix = key_bytes.get(prefix_len..)?;
        String::from_utf8(suffix.to_vec()).ok()
    }

    /// Key of the reverse mapping for vector id `vec_id`. Big-endian keeps
    /// numeric order equal to byte order (a `scan_prefix` yields mappings in
    /// ascending id order — how the allocator heals, ADR-027 §4).
    #[must_use]
    pub fn vecmap_key(vec_id: u64) -> Key {
        let mut bytes = Vec::with_capacity(VECMAP_PREFIX.len() + 8);
        bytes.extend_from_slice(VECMAP_PREFIX);
        bytes.extend_from_slice(&vec_id.to_be_bytes());
        Key::new(bytes)
    }

    /// Inverse of [`vecmap_key`]: extracts the vector id from a mapping
    /// key's bytes, `None` for foreign/malformed keys.
    #[must_use]
    pub fn vecmap_id(key_bytes: &[u8]) -> Option<u64> {
        let suffix = key_bytes.strip_prefix(VECMAP_PREFIX)?;
        let raw: [u8; 8] = suffix.try_into().ok()?;
        Some(u64::from_be_bytes(raw))
    }

    /// Key of the allocator-metadata record.
    #[must_use]
    pub fn meta_key() -> Key {
        Key::new(META_KEY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering_is_lexicographic_on_raw_bytes() {
        let a = Key::from(&b"aa"[..]);
        let b = Key::from(&b"ab"[..]);
        assert!(a < b);
    }

    #[test]
    fn vector_node_key_roundtrips_the_id() {
        let key = vector_index::node_key(0xDEAD_BEEF_0042_1337);
        assert!(key.as_bytes().starts_with(vector_index::NODE_PREFIX));
        assert_eq!(vector_index::node_id(key.as_bytes()), Some(0xDEAD_BEEF_0042_1337));
    }

    #[test]
    fn vector_node_keys_sort_in_id_order() {
        let a = vector_index::node_key(1);
        let b = vector_index::node_key(2);
        let big = vector_index::node_key(u64::from(u32::MAX) + 1);
        assert!(a < b);
        assert!(b < big);
    }

    #[test]
    fn vector_node_id_rejects_foreign_and_malformed_keys() {
        assert_eq!(vector_index::node_id(b"idx/vector/meta"), None);
        assert_eq!(vector_index::node_id(b"idx/vector/node/short"), None);
        assert_eq!(vector_index::node_id(b"unrelated"), None);
    }

    #[test]
    fn vector_meta_key_is_inside_the_reserved_prefix() {
        let meta = vector_index::meta_key();
        assert!(meta.as_bytes().starts_with(vector_index::INDEX_PREFIX));
        // meta sorts before every node key, keeping the index range contiguous.
        assert!(meta < vector_index::node_key(0));
    }

    #[test]
    fn graph_entity_key_roundtrips_and_scopes_by_agent() {
        let key = graph_index::entity_key("agent-a", "alice").expect("encode");
        assert!(key.as_bytes().starts_with(graph_index::ENTITY_PREFIX));
        let prefix = graph_index::entity_agent_prefix("agent-a").expect("prefix");
        assert!(key.as_bytes().starts_with(&prefix[..]));
        // A different agent's prefix must not match, even with a shared
        // textual prefix ("agent-a" vs "agent-ab") — the length prefix on
        // `agent` is exactly what prevents that collision.
        let other_prefix = graph_index::entity_agent_prefix("agent-ab").expect("prefix");
        assert!(!key.as_bytes().starts_with(&other_prefix[..]));
    }

    #[test]
    fn graph_entity_keys_do_not_collide_across_agent_id_boundary() {
        // agent="ab", id="c" vs agent="a", id="bc" — without the length
        // prefix on `agent` both would encode to the same bytes.
        let a = graph_index::entity_key("ab", "c").expect("encode");
        let b = graph_index::entity_key("a", "bc").expect("encode");
        assert_ne!(a.as_bytes(), b.as_bytes());
    }

    #[test]
    fn graph_edge_key_roundtrips_relation_and_dst() {
        let agent = "agent-a";
        let src = "alice";
        let prefix = graph_index::edge_src_prefix(agent, src).expect("prefix");
        let key = graph_index::edge_key(agent, src, "employeur", "acme").expect("encode");
        assert!(key.as_bytes().starts_with(&prefix[..]));
        let (relation, dst) = graph_index::edge_relation_dst(prefix.len(), key.as_bytes()).expect("decode");
        assert_eq!(relation, "employeur");
        assert_eq!(dst, "acme");
    }

    #[test]
    fn graph_edge_src_prefix_scopes_by_source_node() {
        let agent = "agent-a";
        let key_from_alice = graph_index::edge_key(agent, "alice", "rel", "bob").expect("encode");
        let prefix_for_bob = graph_index::edge_src_prefix(agent, "bob").expect("prefix");
        assert!(!key_from_alice.as_bytes().starts_with(&prefix_for_bob[..]));
        let prefix_for_alice = graph_index::edge_src_prefix(agent, "alice").expect("prefix");
        assert!(key_from_alice.as_bytes().starts_with(&prefix_for_alice[..]));
    }

    #[test]
    fn graph_edge_relation_dst_rejects_truncated_and_foreign_keys() {
        let prefix = graph_index::edge_src_prefix("a", "src").expect("prefix");
        assert_eq!(graph_index::edge_relation_dst(prefix.len(), prefix.as_slice()), None);
        assert_eq!(graph_index::edge_relation_dst(prefix.len(), b"short"), None);
        // A relation_len claiming more bytes than actually follow.
        let mut lying = prefix.clone();
        lying.extend_from_slice(&u32::MAX.to_be_bytes());
        lying.extend_from_slice(b"x");
        assert_eq!(graph_index::edge_relation_dst(prefix.len(), &lying), None);
    }

    #[test]
    fn memory_record_key_roundtrips_and_scopes_by_agent() {
        let key = memory_index::record_key("agent-a", "m1").expect("encode");
        assert!(key.as_bytes().starts_with(memory_index::RECORD_PREFIX));
        let prefix = memory_index::record_agent_prefix("agent-a").expect("prefix");
        assert!(key.as_bytes().starts_with(&prefix[..]));
        assert_eq!(
            memory_index::record_id(prefix.len(), key.as_bytes()),
            Some("m1".to_string())
        );
        // Same shared-textual-prefix trap as the graph keys.
        let other_prefix = memory_index::record_agent_prefix("agent-ab").expect("prefix");
        assert!(!key.as_bytes().starts_with(&other_prefix[..]));
    }

    #[test]
    fn memory_record_keys_do_not_collide_across_agent_id_boundary() {
        let a = memory_index::record_key("ab", "c").expect("encode");
        let b = memory_index::record_key("a", "bc").expect("encode");
        assert_ne!(a.as_bytes(), b.as_bytes());
    }

    #[test]
    fn memory_record_id_rejects_foreign_and_undersized_keys() {
        let prefix = memory_index::record_agent_prefix("agent-a").expect("prefix");
        assert_eq!(memory_index::record_id(prefix.len(), b"unrelated"), None);
        assert_eq!(memory_index::record_id(prefix.len(), memory_index::META_KEY), None);
        // A key shorter than the prefix it was allegedly scanned under.
        assert_eq!(memory_index::record_id(prefix.len(), memory_index::RECORD_PREFIX), None);
    }

    #[test]
    fn memory_vecmap_key_roundtrips_the_id_and_sorts_numerically() {
        let key = memory_index::vecmap_key(0xDEAD_BEEF);
        assert!(key.as_bytes().starts_with(memory_index::VECMAP_PREFIX));
        assert_eq!(memory_index::vecmap_id(key.as_bytes()), Some(0xDEAD_BEEF));
        assert!(memory_index::vecmap_key(1) < memory_index::vecmap_key(2));
        assert!(memory_index::vecmap_key(2) < memory_index::vecmap_key(u64::from(u32::MAX) + 1));
        assert_eq!(memory_index::vecmap_id(b"idx/memory/meta"), None);
        assert_eq!(memory_index::vecmap_id(b"idx/memory/vecmap/short"), None);
    }

    #[test]
    fn memory_meta_key_is_inside_the_reserved_prefix() {
        let meta = memory_index::meta_key();
        assert!(meta.as_bytes().starts_with(memory_index::INDEX_PREFIX));
    }
}
