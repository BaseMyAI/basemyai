# ADR-019 — Agent Memory Database, `.bmai` V1, and Storage Engine Boundary

**Status**: Accepted  
**Date**: 2026-06-18  
**Context**: supersedes the product framing of "SQLite-backed memory engine" without replacing ADR-011's libSQL V1 backend decision.

## Context

BaseMyAI's product direction is no longer "a local memory library backed by
SQLite". The target category is an **Agent Memory Database**: an embedded,
private, temporal and encrypted database specialized for AI agents.

The research document
`docs/strategy/2026-06-18-agent-memory-database-research.md` concluded that
SQLite/libSQL remains the right V1 backend, but that BaseMyAI must not expose
SQLite as the product identity. Competitors such as Mem0, Zep/Graphiti, Letta,
LangMem, LlamaIndex, Cognee, Supermemory and Hindsight compete at the memory
layer. Vector databases and SQLite vector extensions are infrastructure, not
the product surface.

## Decision

1. BaseMyAI's public file artifact is `.bmai`.
2. In V1, `.bmai` is implemented as an encrypted libSQL-compatible database
   containing BaseMyAI schema and metadata.
3. The V1 backend remains libSQL with native vector search, FTS and recursive
   SQL.
4. A storage engine boundary is introduced incrementally. The first step is a
   small engine capability contract on the existing `Store`; a full backend
   crate extraction is deferred.
5. A native `.bmai` append-only backend is explicitly out of scope for V1.

## Consequences

Positive:

- The product can be documented as a BaseMyAI memory database rather than a
  SQLite wrapper.
- SDKs and CLI can standardize on `.bmai` paths now.
- The backend remains boring and reliable while the memory API matures.
- A future native backend has a clear migration path through the engine
  boundary and container metadata.

Tradeoffs:

- V1 `.bmai` files are still libSQL-compatible internally.
- The public format identity is stronger than the internal implementation
  boundary; documentation must be honest about that.
- A full `basemyai-storage-libsql` crate split remains future work.

## Rejected Alternatives

**Implement a native `.bmai` backend now.** Rejected. It would require crash
recovery, compaction, vector indexing, encryption and migration machinery before
the memory product is proven.

**Keep exposing `.db` paths in examples.** Rejected. It reinforces the wrong
mental model and weakens product positioning.

**Create a generic SQL abstraction.** Rejected. BaseMyAI needs a memory database
engine contract, not a lowest-common-denominator SQL facade.

## Follow-Up Work

- Prefer `.bmai` in README examples and SDK defaults.
- Add CLI commands around `.bmai` inspection and verification.
- Gradually move SQL/libSQL-specific code behind an engine module or future
  `basemyai-storage-libsql` crate.
- Add backend contract tests before any second backend exists.
