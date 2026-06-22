# Changelog

All notable changes to this project are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/basemyai/basemyai/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/basemyai/basemyai/releases/tag/v0.1.0
