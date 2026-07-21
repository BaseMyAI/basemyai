# Changelog

All notable changes to this project are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **N13/J3 — immutable version set, S1 snapshots, deferred SST removal
  (ADR-043 §2 as amended for ENG-COR-001).** `Engine` now publishes its live
  SST set as an immutable `Version` (one shared `Arc` handle per file);
  every publication — flush or compaction — is a `VersionEdit { added,
  deleted }` applied to the *current* version at commit time (validated:
  a `deleted` id absent from the current version is the new typed
  `EngineError::VersionEditMissingInput`, and nothing is published). New
  `Engine::snapshot() -> Snapshot`: an S1 snapshot that freezes the *files*,
  not the *view* (the memtable is not captured) — every pinned SST stays on
  disk and readable for the snapshot's lifetime; superseded SSTs are now
  physically removed only when their last referencing version/snapshot
  drops (previously: inline in `compact()`), a removal failure still being
  counted (`compaction_remove_failures`) and the leftover swept as a
  manifest orphan at the next open. New `EngineStats::active_snapshots`
  gauge. Compaction itself still runs under the exclusive writer — J4
  (out-of-lock compaction) is the next milestone and only changes locking,
  not the publication protocol.

- **N10 — scalable maintenance (ADR-041, §7.1→§7.5).**
  - Importance API: `Memory::remember_with_importance` / `Memory::set_importance`
    (NaN/infinite rejected via `MemoryError::InvalidImportance`); `NewMemory`
    and `MemoryStore::put_memory` gain an `importance: f64` field
    (`DEFAULT_IMPORTANCE = 1.0`).
  - Temporal expiry index (`idx/temporal/expiry/`) + `Engine::scan_range`:
    expired-memory GC is now a bounded range query instead of a full
    per-agent scan (no `MemoryRecord` decoding at all).
  - Memory-bounded adaptive forgetting: two paged passes over
    `Engine::scan_range_page`, survivor selection in `O(capacity)` memory.
  - `MemoryStore::forget_many` + engine `PersistentMemoryIndex::forget_many`
    (`ForgetBatchOptions { max_items, max_wal_bytes }`): batched physical
    deletion in bounded atomic chunks (aggregated FTS stats, grouped vector
    tombstones, one WAL record per chunk), idempotent resume between chunks.
    Wired into both eviction paths (GC + adaptive forgetting, CLI and
    event-emitting `Memory` facade — `Forgotten` events still emitted
    post-commit, per existing memory).
  - Agent registry (`meta/agents/`): `NativeMemoryStore::list_agents()` —
    identifiers only, registered on first insert, unregistered by
    `purge_agent` (never by a mere forget). Not retroactive for stores
    written before this change.

### Changed

- **Breaking (trait `MemoryStore`)**: `scan_for_forgetting` is now paginated
  (`after_id`/`limit`, active-only candidates) and `forget_many` is a new
  required method.

### Fixed

- **N13 preflight (ENG-DUR-003, `docs/audits/2026-07-engine-architecture-safety-audit.md`).**
  Every publication `rename` (SST, `store.meta`, `generation.meta`,
  `crypto.meta`) is now followed by an `fsync` of its containing directory
  (`crate::fs_util::sync_dir`, no-op on non-Unix) — POSIX gives no ordering
  guarantee between a rename's directory-entry mutation and any other file's
  own metadata mutation without it.
- **N13 preflight (ENG-DUR-004).** `resolve_active_generation` no longer
  treats a missing `generation.meta` as an unconditional "generation 0" when
  a `gen-N` directory already exists next to it — that combination used to
  make the unconditional post-open GC delete the real generation, a silent
  total data loss. It is now refused as a typed
  `EngineError::CorruptGenerationMeta`.
- **N13 preflight (ENG-DUR-002, minimal correction).** `compact()` no longer
  silently ignores a failed removal of a superseded SST. It retries a few
  times, then counts a persistent failure via the new
  `EngineStats::compaction_remove_failures` instead of swallowing it. Full
  closure of the underlying resurrection risk still depends on the durable
  SST manifest (N13).

## [0.2.0] - 2026-07-10

First native-only release. **Breaking**: no libSQL/V1 compatibility — see
"Changed" below and `docs/adr/ADR-033-native-only.md`.

### Added

- **Adaptive forgetting, ported to the native engine (ADR-037).** Reintroduces
  the capacity-bounded eviction mechanism removed by the native-only
  migration, on top of an applicative scan (`MemoryStore::scan_for_forgetting`)
  instead of a libSQL windowed query. `Memory::adaptive_forget`,
  `AdaptiveForgettingTask` (`MaintenanceWorker` integration), and the CLI
  `forget-adaptive` command (`--capacity`, `--half-life-secs`, `--dry-run`).
  Same hyperbolic retention score as before removal
  (`importance + half_life / (half_life + age)`); scope refined to only
  consider **active** memories (invalidated/expired memories no longer count
  toward capacity — see expired-memory GC below).
- **Expired-memory GC, ported to the native engine (ADR-038).** Reintroduces
  physical deletion of memories whose `valid_until <= now`, paginated by an
  id-based cursor (idempotent, resumable after an interruption).
  `Memory::expired_gc`, `ExpiredMemoryGcTask`, and the CLI `gc` command
  (`--page-size`, `--dry-run`). Disjoint by construction from adaptive
  forgetting (active vs. expired memories never overlap).
- Both `forget-adaptive` and `gc` CLI commands open the raw store
  (`open_engine`), never a full `Memory` — no Candle embedder is loaded, so
  neither needs a provisioned model and both run in CI.
- **NAPI live subscriptions** (`bindings/basemyai-node`) — `Memory.watch(agentId, layer?, callback)`
  resolves to a `WatchHandle`; the JS callback is invoked via `ThreadsafeFunction`
  for every `remember`/`invalidate`/`forget`/`consolidate` event on that agent
  (optionally scoped to one layer), isolated server-side exactly like the
  existing REST/MCP/Python `watch` surfaces. `WatchHandle.close()` stops the
  relay and frees its background task (idempotent; also runs on `Drop`/GC).
- **CUDA/NVML hardware detection** (`basemyai`, feature `cuda-detect`) —
  `detect_hardware()` now reports `HardwareProfile.gpus: Vec<GpuInfo>` (GPU
  count, name, total/free VRAM per device) via `nvml-wrapper`, a pure-Rust
  NVML binding (no CMake). Best-effort: NVML/driver absent (no NVIDIA GPU,
  the common case in CI) never panics, `gpus` is simply empty. Optional and
  outside the default/CI feature set given its cost and the lack of NVIDIA
  hardware to validate against in CI.
- **Docker image for `basemyai-rest`** — multi-stage `Dockerfile`
  (`crates/basemyai-rest/Dockerfile`) and `docker-compose.yml` at the
  workspace root. Builder needs only `build-essential`/`pkg-config` (no
  CMake, no libSQL); runtime is `debian-slim` running as a non-root user.
- **`cargo-dist` packaging for `basemyai-cli`** (`dist-workspace.toml`,
  `.github/workflows/cli-release.yml`) — precompiled binaries for
  Windows/Linux/macOS (x86_64 + aarch64). Uses its own tag namespace
  (`cli-v*`) so it never collides with the existing crates.io/GitHub-Release
  workflow. `basemyai-mcp`/`basemyai-rest` opt out
  (`package.metadata.dist.dist = false`) since they ship as a
  library/transport and a Docker image, respectively, not standalone
  binaries.
- **P1 market benchmark rerun on the native engine**
  (`docs/benchmarks/n6-native-vs-mem0-qdrant-2026-07-10.md`) — the prior
  comparison (2026-06-21) measured BaseMyAI on the now-removed libSQL
  backend. Rerun confirms the core claim (`remember` ~10.8× faster than real
  Mem0, which pays an LLM call per `.add()`) while also disclosing where the
  native engine is currently slower than raw Qdrant (`recall`/`recall_hybrid`
  latency) — reported honestly rather than omitted.

### Changed

- Migrated the active workspace to **native-only** storage: removed libSQL/V1
  compatibility paths from runtime code (`Store`, `LibsqlMemoryStore`,
  SQL-leaky surface, dual-backend feature flags).
- Removed legacy libSQL `crypto` build paths from CI and DX workflows
  (`cargo xtask`, GitHub Actions wheels/prebuilds, binding packaging configs).
- Updated integration tests to open native stores through production APIs (temp
  directories) instead of test-only ephemeral helpers.
- Set `MAX_TEXT_LEN` to `65535` (`u16::MAX`) so the public limit is consistent
  with native FTS docterm encoding.

## [0.1.0] - 2026-06-21

First public release. Published to crates.io (`basemyai-core`, `basemyai`).

### Added

- **Memory engine (Phase 1)** — persistent, temporal, per-agent isolated memory
  on a hardened async libSQL backend with native vector search (`F32_BLOB`,
  `vector_top_k`) and encryption at rest (libSQL `crypto`).
- **Cognition (Phase 2)** — entity/relation graph with cycle-safe recursive
  traversal, RRF hybrid fusion (vector + BM25), adaptive forgetting, and
  episode→fact consolidation with an injected `LlmInference`.
- **`basemyai-core`** — business-agnostic foundation: async `Store`,
  parameterized `Filter`, object-safe `Embedder` (Candle, `all-MiniLM-L6-v2`,
  384d), and an injected maintenance worker.
- **`basemyai`** — memory semantics: four layers, `AgentId` isolation, temporal
  validity, and a `MemoryStore` storage-engine boundary (ADR-020).
- **Surfaces** — MCP server, REST sidecar, CLI (`basemyai`), and PyO3 / NAPI-RS
  bindings.
- **`.bmai`** encrypted container format (ADR-019).
- Hardware-aware, no-silent-download provisioning for embedder and local LLM
  options (ADR-010, ADR-013).
- **Agent-driven consolidation** (ADR-018, supersedes ADR-017) — real E2E
  testing in Claude Code showed MCP sampling is unsupported there (`-32601`)
  and deprecated in the protocol (SEP-2577). The `consolidate` MCP tool now
  uses a tiered policy: sampling *if advertised by the client* → local LLM
  (Ollama / LM Studio / AnythingLLM via `choose_llm`) → otherwise
  `status:"extraction_required"`, where the calling agent extracts with its
  own LLM and persists via the new **`consolidate_apply`** tool. New
  **`consolidate_memory`** MCP prompt drives this flow end to end. In
  `basemyai`, `consolidate()` is split into `consolidation_prompt` /
  `parse_extraction` / `apply_extraction` (public `Extraction` /
  `ExtractedEntity` / `ExtractedRelation` types); MCP tool annotations
  (`read_only_hint` / `destructive_hint` / `idempotent_hint` /
  `open_world_hint`) added on all 8 tools.
- **`basemyai-mcp` production binary** — hardware-aware setup (Candle
  embedder) → encrypted libSQL provider → MCP server over **stdio** (default)
  or local **HTTP** (`BASEMYAI_MCP_TRANSPORT=http`); logs on stderr (stdout is
  the MCP channel in stdio). Env vars: `BASEMYAI_DB_KEY` (required),
  `BASEMYAI_FETCH=1` (model-fetch consent on first run). Install guide:
  `docs/mcp-install.md`.
- **`SamplingBackend`** (ADR-017) — consolidation can borrow the MCP client's
  LLM via `sampling/createMessage`; implements `LlmInference`, lives in
  `basemyai-mcp` (the memory crate stays MCP-agnostic). New
  `McpError::Sampling` variant.
- **`AnythingLlmBackend`** (ADR-016) — LLM backend over AnythingLLM's
  workspace-chat API (`POST /api/v1/workspace/{slug}/chat`, Bearer auth).
  Used by `choose_llm()` as level-2 fallback when
  `BASEMYAI_ANYTHINGLLM_KEY` + `BASEMYAI_ANYTHINGLLM_WORKSPACE` are set and no
  direct backend is available; `LlmProvision.backend` is now
  `Box<dyn LlmInference>`.
- **`OpenAiCompatBackend`** — new name for the inference backend
  (`OllamaBackend` remains an alias): talks to any OpenAI-compatible server
  (Ollama, LM Studio, Jan, vLLM…). Adds an inference timeout (300 s default,
  `with_timeout`) and a 5 s connection timeout so a frozen local server no
  longer blocks consolidation.
- **`Memory::export_jsonl` / `import_jsonl`** — versioned JSONL export/import
  of an agent's full memory (records + graph + validity + importance).
  Embeddings are excluded and **re-computed on import** (batched
  `embed_batch`), making export the embedding-model migration path. Import is
  atomic (single transaction) and idempotent (`INSERT OR IGNORE`, returns an
  `ImportReport`). New `MemoryError::Porting` variant.
- **`Memory::remember_batch` / `remember_batch_with`** — batch ingestion (one
  `embed_batch` pass, one transaction: all or nothing).
- **`Store::begin_write()` → `WriteTxn`** (`basemyai-core`) — serialized write
  transaction (`BEGIN IMMEDIATE` + internal writer lock, automatic rollback on
  drop) making consumers' multi-table writes atomic on the shared connection.
- `remember`, `forget` and `purge_agent` are now **atomic** (the `memory`
  table and its `memory_fts` BM25 mirror are updated in the same transaction).
- Manually triggered E2E test `consolidation_e2e` (`#[ignore]`): 3 episodes →
  `consolidate()` via AnythingLLM → 6 entities + 5 relations extracted by
  `qwen3-vl:4b-instruct` (validated 2026-06-13) — first real run of the
  consolidation→graph pipeline against a physical LLM.

### API stability (0.1.0)

The following types are `#[non_exhaustive]` — new variants can be added in a
minor release without a breaking change:

| Type | Crate | Reason |
| --- | --- | --- |
| `CoreError` | `basemyai-core` | Extensible foundation errors |
| `MemoryError` | `basemyai` | Extensible memory errors |
| `Device` | `basemyai-core` | Future compute devices |
| `MemoryLayer` | `basemyai` | Possible extra layer in V1.1 |
| `Value` | `basemyai-core` | Extensible libSQL SQL types |
| `BackendKind` | `basemyai` | Future local LLM servers |

Stable types (public fields, no `#[non_exhaustive]`):
`Record`, `AgentStats`, `Reached`, `ConsolidationReport`, `Fused`, `Ranking`,
`Neighbor`, `Filter`, `Migration`, `Validity`, `KnownModel`, `LlmOption`.

[Unreleased]: https://github.com/basemyai/basemyai/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/basemyai/basemyai/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/basemyai/basemyai/releases/tag/v0.1.0
