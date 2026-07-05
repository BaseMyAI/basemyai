//! Native memory index (N5.1, ADR-027): the third logical index on the
//! Layer-1 KV store, next to [`super::vector`] (ADR-026) and
//! [`super::graph`] (N4). It gives memory records a durable home
//! (`MemoryRecord:1`), links them to their vector-index nodes both ways
//! (`vec_id` in the record, `MemoryVecMap:1` back), and owns the monotonic
//! id allocator (`MemoryIndexMeta:1`) — all under the reserved
//! `idx/memory/` keyspace ([`crate::key::memory_index`]).
//!
//! Layer semantics, validity policy and search ranking stay at the consumer
//! (`basemyai`'s `NativeMemoryStore`): this module persists opaque tags and
//! composes crash-safe batches, nothing more. See
//! [`persistent::PersistentMemoryIndex`]'s module doc for the atomicity
//! story (every put/forget rides the vector index's `apply_batch`).

pub mod meta;
pub mod persistent;
pub mod record;
pub mod vecmap;

pub use persistent::{NewMemoryRecord, PersistentMemoryIndex};
pub use record::MemoryRecord;
pub use vecmap::VecMapEntry;
