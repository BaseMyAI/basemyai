# basemyai — Python bindings

[![PyPI](https://img.shields.io/pypi/v/basemyai?color=3776ab&label=PyPI)](https://pypi.org/project/basemyai/)
[![Python](https://img.shields.io/pypi/pyversions/basemyai)](https://pypi.org/project/basemyai/)
[![License](https://img.shields.io/pypi/l/basemyai)](https://github.com/basemyai/basemyai/blob/main/LICENSE)

**Local memory engine for AI agents** — Python SDK built with [PyO3](https://pyo3.rs/) and distributed as precompiled wheels (no Rust toolchain required on the client).

BaseMyAI gives agents persistent, temporal, multi-layered memory: vector search, knowledge graph, hybrid retrieval, and per-agent isolation — all in one encrypted local `.bmai` file. Zero cloud. Zero silent downloads.

> This package wraps the Rust [`basemyai`](https://crates.io/crates/basemyai) crate. For the full product overview, architecture, and CLI, see the [main repository](https://github.com/basemyai/basemyai).

## Features

- Four memory layers: `short_term`, `episodic`, `procedural`, `semantic`
- Temporal RAG — only memories that are still valid are returned
- Hybrid recall — vector similarity + BM25 full-text, fused with Reciprocal Rank Fusion
- Bounded context compilation — typed sections, Markdown rendering, citations, and optional exclusion trace
- Knowledge graph — entities, relations, multi-hop traversal
- Per-agent isolation enforced structurally
- Encryption at rest (native envelope, XChaCha20-Poly1305) — passphrase at open time ([ADR-034](https://github.com/basemyai/basemyai/blob/main/docs/adr/ADR-034-user-key-resolution.md))
- Fully async API (`asyncio` coroutines backed by an internal Tokio runtime)

## Requirements

- Python **3.10+**
- A local embedding model (`all-MiniLM-L6-v2`, 384d) — the only piece that needs one explicit gesture, because fetching it is a real network operation

There is **no silent download, ever**. Either fetch it once via the CLI:

```bash
# Install the CLI from the main repo, or use basemyai setup after distribution
basemyai setup --fetch
```

or consent inline, in code, the first time you call `Memory.open(consent_to_fetch=True)` (see below) — it's cached after that. Everything else (`path`, `agent_id`, the encryption key) needs no setup at all.

## Installation

```bash
pip install basemyai
```

Precompiled wheels are provided for Linux, Windows, and macOS (x86_64 and Apple Silicon).

## Quick start

No setup step required — `pip install basemyai` and go:

```python
import asyncio
from basemyai import Memory

async def main():
    # path defaults to "./basemyai.bmai", agent_id to "default", and the
    # encryption key is generated at ~/.basemyai/key on first use if none
    # exists yet (a notice is printed to stderr — back that file up, it's the
    # only copy). consent_to_fetch=True consents to the one real network op —
    # fetching the embedding model once; it's cached after that.
    mem = await Memory.open(consent_to_fetch=True)

    # Running multiple agents, or want everything explicit/scripted? Override
    # any of these, or run `basemyai config set db-path|agent` once so every
    # Memory.open() on this machine picks up the same store/agent by default:
    #   mem = await Memory.open(path="./agent.bmai", agent_id="assistant-42")

    # Store a semantic fact
    memory_id = await mem.remember(
        "The user is on the Pro plan.",
        layer="semantic",
    )

    # Temporal RAG: relevant AND still valid
    hits = await mem.recall("which plan is the user on?", k=5)
    for hit in hits:
        print(hit.text, hit.score)

    # Hybrid recall (vector + full-text)
    hybrid = await mem.recall_hybrid("billing plan", k=10)

    # Bounded prompt context with inspectable provenance
    context = await mem.compile_context(
        "billing plan",
        token_budget=512,
        explain=True,
    )
    print(context.rendered, context.citations)

    # Knowledge graph
    await mem.add_graph_entity("alice", "person", "Alice")
    await mem.add_graph_entity("acme", "organization", "Acme")
    await mem.add_graph_edge("alice", "works_at", "acme")
    reachable = await mem.recall_graph("alice", max_depth=2)

    # Invalidate a fact that is no longer true
    await mem.invalidate(memory_id)

asyncio.run(main())
```

### Memory layers

| Layer | Purpose |
|---|---|
| `short_term` | Working context for the active session |
| `episodic` | Events and interactions (what happened, when) |
| `procedural` | Learned workflows and skills |
| `semantic` | Facts and knowledge (vector-searchable) |

### API overview

| Method | Description |
|---|---|
| `Memory.open(...)` | Open an encrypted `.bmai` store with a local embedder |
| `remember(text, layer)` | Store a memory; returns its UUID |
| `observe(turns)` | Ingest raw conversation turns (`list[tuple[role, content]]`) as episodic memories in one batch; returns their UUIDs |
| `recall(query, k)` | Temporal semantic recall |
| `recall_by_layer(query, layer, k)` | Recall scoped to one layer |
| `recall_hybrid(query, k)` | Vector + BM25 fused with RRF |
| `compile_context(query, token_budget, ...)` | Bounded Markdown context plus typed sections, citations, merges, and optional exclusions |
| `invalidate(id)` | Soft-delete (sets `valid_until` to now) |
| `forget(id)` | Physical delete (GDPR right to erasure) |
| `stats()` | Count of valid memories per layer |
| `add_graph_entity` / `add_graph_edge` | Insert graph facts |
| `recall_graph(start, max_depth)` | Multi-hop graph traversal |

### `Memory.open` parameters

| Parameter | Required | Description |
|---|---|---|
| `path` | no | Path to the `.bmai` container file. Resolution order: explicit → `~/.basemyai/config.toml` / `BASEMYAI_DB_PATH` (`basemyai config set db-path <path>`) → built-in default `./basemyai.bmai` (relative to the process's working directory) |
| `agent_id` | no | Tenant identifier (per-agent isolation). Same resolution order, built-in default `"default"` |
| `encryption_key` | no | Explicit credential. Without it, [ADR-034](https://github.com/basemyai/basemyai/blob/main/docs/security/key-resolution.md) resolution applies (`BASEMYAI_DB_KEY`, `BASEMYAI_DB_KEY_FILE`, `~/.basemyai/key`, …); if no source exists **anywhere**, a key is generated and persisted to `~/.basemyai/key` automatically (stderr notice — back that file up, it's the only copy and losing it makes existing data unrecoverable) |
| `credential_mode` | no | Interpretation of an explicit credential: `raw` (default) or `passphrase` (Argon2id) |
| `model_dir` | no | Path to `all-MiniLM-L6-v2` model files |
| `device` | no | `"auto"`, `"cpu"`, `"cuda"`, or `"metal"` (default: `"auto"`) |
| `consent_to_fetch` | no | Consent to fetch the model if it isn't cached and `model_dir` is omitted (default: `False`) — the one operation this SDK will never do silently, because it's a real network call |

## Test-only API

`Memory.open_in_memory(agent_id)` is compiled **only** with the `test-util` feature. It uses an ephemeral store and a deterministic fake embedder — not part of the production SDK surface. Production code should always use `Memory.open(...)`.

## Error types

```python
from basemyai import (
    BasemyaiError,
    ValidationError,
    StorageError,
    EncryptionError,
    InferenceError,
)
```

## Related packages

| Package | Surface |
|---|---|
| [`basemyai`](https://crates.io/crates/basemyai) (Rust) | Native crate — full memory semantics |
| [`basemyai-node`](https://www.npmjs.com/package/basemyai) | Node.js / TypeScript bindings (NAPI-RS) |
| [`basemyai-core`](https://crates.io/crates/basemyai-core) | Business-agnostic foundation (for custom engines) |

## Documentation

- [Main README](https://github.com/basemyai/basemyai)
- [Key resolution (ADR-034)](https://github.com/basemyai/basemyai/blob/main/docs/security/key-resolution.md)
- [CLI reference](https://github.com/basemyai/basemyai/blob/main/docs/cli.md)
- [Architecture decisions (ADR)](https://github.com/basemyai/basemyai/blob/main/docs/ADR.md)

## License

Source-available under the [Business Source License 1.1](https://github.com/basemyai/basemyai/blob/main/LICENSE) (converts to Apache-2.0 four years after each version's release). See [ADR-031](https://github.com/basemyai/basemyai/blob/main/docs/adr/ADR-031-unified-busl-license.md).
