# `.bmai` Format Specification

**Status**: Active (native engine, ADR-033) — **format stability: experimental**  
**Date**: 2026-07-08 (rewritten for native-only; supersedes the 2026-06-18 libSQL draft)

`.bmai` is the public BaseMyAI agent memory database artifact. Since ADR-033,
it is implemented as an **encrypted native engine directory** (`basemyai-engine`),
not as a SQLite/libSQL file. Applications must open it through BaseMyAI APIs
(`Memory::open_native`, CLI, MCP, REST, bindings) — not as a generic database
file.

## Format stability

BaseMyAI is not yet used publicly in production on the native engine, so the
on-disk `.bmai` format (WAL/SST layout, block/codec versions, `crypto.meta`)
is **experimental**, not a frozen contract:

- no backward compatibility between internal format revisions is guaranteed;
- a wire-format change can drop an old codec or on-disk layout outright
  (`format.lock` is bumped deliberately, not migrated);
- development stores created with an old format version are recreated, not
  migrated — see `PLAN-NATIVE-ENGINE.md` §"Politique de remplacement du
  format" for the concrete rule engine changes (e.g. the block-based SST
  format landed by ADR-039/N8) must follow;
- a migration path is only built when it is actually useful to the project —
  never speculatively.

The format-stability contract begins only once an explicit decision (a new
ADR) freezes it — expected at the earliest around or before BaseMyAI `1.0`.
Until then, treat every `.bmai` directory produced by a development build as
disposable.

## Product identity

- **Extension**: `.bmai` (unchanged from ADR-019).
- **Physical layout**: a **directory** named e.g. `agent.bmai/` containing the
  engine's WAL, SST files, and `crypto.meta` (see [On-disk layout](#on-disk-layout)).
- **Logical identity**: `format=basemyai-memory`, `storage_engine=native`.
- **Encryption**: mandatory at the product layer (`basemyai`, ADR-007/ADR-030).
  Production paths use `NativeMemoryStore::open_encrypted` / `BASEMYAI_DB_KEY`.
  No CMake, no SQLCipher — XChaCha20-Poly1305 pure Rust (ADR-030).

## Retired: libSQL V1 (`format_version=1`, `storage_engine=libsql`)

The first public release (`0.1.0`, 2026-06) used a **single encrypted libSQL
file** as `.bmai`. That implementation is **removed** from the active workspace
(ADR-033). There is no automatic in-place upgrade:

1. Open the old file with BaseMyAI `0.1.0` (or export while libSQL support still
   exists in your checkout).
2. `Memory::export_jsonl` / `basemyai export` for each agent.
3. Create a new native `.bmai` directory and `import` / `Memory::import_jsonl`.

Do not point BaseMyAI `0.2.0+` at a libSQL-era `.bmai` file and expect recovery.

## Container metadata

Metadata is stored as UTF-8 key/value pairs in the native KV store under the
prefix `meta/bmai/` (semantic equivalent of the historical `bmai_meta` SQL
table). Keys are seeded idempotently on every open (`ensure_container_meta`).

Required keys at creation:

| Key | Current value | Meaning |
|---|---|---|
| `format` | `basemyai-memory` | Identifies the file as a BaseMyAI memory container. |
| `format_version` | `2` | Public container format version (`BMAI_FORMAT_VERSION`). |
| `storage_engine` | `native` | Active backend (`basemyai-engine`). |
| `schema_family` | `agent-memory` | Product schema family (unchanged). |
| `embedding_dim` | `384` | Baseline embedding dimension (`all-MiniLM-L6-v2`). |

Optional keys written on first open with an embedder:

| Key | Meaning |
|---|---|
| `embedding_model_id` | Baseline model id from provisioning. Reopening with a different model or dimension is rejected — use JSONL export/import to re-embed. |

Read metadata via `NativeMemoryStore::container_metadata()`, CLI `inspect`, or
`verify`.

## On-disk layout

A production `.bmai` directory typically contains:

| Path | Role |
|---|---|
| `store.meta` | Store-generation marker (ADR-039 §7). Absent with other artifacts present ⇒ an incompatible/pre-ADR-039 store, rejected at open. |
| `crypto.meta` | DEK/KEK envelope (ADR-030). Presence means the store is encrypted. |
| `wal.log` | Write-ahead log (per-record envelopes when encrypted). |
| `*.sst` | Block-based sorted string tables (ADR-039): header (plaintext) + data blocks + block index + bloom filter + footer; every section but the header sealed individually when encrypted (`EncryptedSstBlock`, AEAD bound to `sst_id`/section/section number). |

Index data (vector LM-DiskANN, graph, memory records, FTS postings) lives in
the engine KV space under versioned key layouts governed by
`crates/basemyai-engine/format.lock` — not in a separate SQL schema.

Wire formats (`WalRecord`, `SstHeader`, `SstDataBlock`, `SstBlockIndex`,
`SstBloomFilter`, `SstFooter`, `EncryptedSstBlock`, `StoreMeta`, `CryptoMeta`,
index record types) are versioned in `format.lock`; CI fails if they drift
without a deliberate bump.

## Engine boundary

Applications depend on:

- `basemyai::storage::MemoryStore` — semantic operations (`put_memory`,
  `recall_vector`, `graph_traverse`, …).
- `basemyai::Memory` — embedding, temporal validity, agent isolation, hybrid
  recall, consolidation.

They must **not** depend on:

- SQL, libSQL, or SQLite tools;
- raw KV key layouts (engine-internal, may evolve within `format.lock` bumps);
- direct `basemyai-engine` APIs unless building a fork of the storage layer.

Zones intentionally outside the portable `MemoryStore` contract (documented in
ADR-020): JSONL porting (`memory/porting.rs`) and background maintenance tasks
beyond `ConsolidationTask` (GC / adaptive forgetting removed with libSQL,
ADR-033).

## Compatibility rules

- `format_version` and `storage_engine` must match what `basemyai verify` expects.
- Additive metadata keys under `meta/bmai/` are allowed within the same
  `format_version`.
- Changing the meaning of an existing key or the on-disk engine layout requires
  a new `format_version` and an ADR.
- Patch releases must read containers written by the same `format_version`.

## Verification

```bash
export BASEMYAI_DB_KEY='your-key'
basemyai verify --db ./agent.bmai
basemyai inspect --db ./agent.bmai
```

`verify` checks `format`, `format_version`, and `storage_engine`. `inspect`
lists all `meta/bmai/*` entries plus aggregate memory counts.

## Non-goals

This spec does **not** define:

- a stable raw KV API for third parties;
- cross-device sync (VISION §5.6 / TODO N6);
- multi-model embedding catalogs (V2);
- opening `.bmai` with off-the-shelf SQLite tools.

Those remain product or engine follow-ups, not part of the public container
contract.

## References

- ADR-019 — product framing and `.bmai` identity
- ADR-033 — native-only migration (retires libSQL V1)
- ADR-030 — encryption at rest
- `docs/adr/ADR-025-native-engine-storage-foundation.md` — LSM foundation
- `crates/basemyai/tests/format.rs` — metadata contract test
