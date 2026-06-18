# `.bmai` Format V1

**Status**: Draft, V1 container contract  
**Date**: 2026-06-18

`.bmai` is the public BaseMyAI memory database file format. In V1, a `.bmai`
file is implemented as an encrypted libSQL-compatible database with BaseMyAI
schema and metadata tables. The storage backend is an implementation detail:
applications should treat the file as a BaseMyAI memory database, not as a
general-purpose SQLite database.

## Decision

V1 uses libSQL internally because it already provides the operational properties
BaseMyAI needs now:

- embedded local file;
- transactional writes;
- native vector search;
- FTS5-compatible keyword search;
- recursive SQL for graph traversal;
- optional encryption in `basemyai-core`, mandatory encryption in `basemyai`.

The `.bmai` extension is exposed immediately so SDKs, CLI tools and docs can
stabilize around a BaseMyAI-owned artifact. A future native backend can keep the
same public extension while changing the internal storage engine behind the
`StorageEngine` contract.

## Container Metadata

Every V1 file has a `bmai_meta` table:

```sql
CREATE TABLE IF NOT EXISTS bmai_meta (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
```

Required keys:

| Key | V1 value | Meaning |
|---|---|---|
| `format` | `basemyai-memory` | Identifies the file as a BaseMyAI memory container. |
| `format_version` | `1` | Public `.bmai` container format version. |
| `storage_engine` | `libsql` | Internal V1 backend. |
| `schema_family` | `agent-memory` | Product schema family. |
| `embedding_dim` | `384` | Baseline embedding dimension. |

The metadata is intentionally small. Detailed runtime state, provisioning
choices and migration history live in their dedicated tables.

## Compatibility Rules

- Existing V1 files must remain readable across patch releases.
- Additive metadata keys are allowed.
- Changing the meaning of an existing key requires a new `format_version`.
- The V1 file may be opened by libSQL-compatible tools for diagnostics, but the
  supported API boundary is BaseMyAI.
- SDKs and CLI should prefer `.bmai` in examples and generated paths.

## Non-Goals

V1 does not define:

- a custom page format;
- custom crash recovery;
- a native append-only engine;
- a custom vector index;
- cross-device sync;
- a stable SQL API for external applications.

Those are backend concerns and remain behind the engine contract.
