# Changelog

All notable changes to this project are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/basemyai/basemyai/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/basemyai/basemyai/releases/tag/v0.1.0
