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
- Knowledge graph — entities, relations, multi-hop traversal
- Per-agent isolation enforced structurally
- Encryption at rest (native envelope, XChaCha20-Poly1305) — passphrase at open time ([ADR-034](https://github.com/basemyai/basemyai/blob/main/docs/adr/ADR-034-user-key-resolution.md))
- Fully async API (`asyncio` coroutines backed by an internal Tokio runtime)

## Requirements

- Python **3.10+**
- A local embedding model (`all-MiniLM-L6-v2`, 384d) — provisioned once via the CLI or an explicit path

There is **no silent download at first run**. Fetch the model explicitly:

```bash
# Install the CLI from the main repo, or use basemyai setup after distribution
basemyai setup --fetch
```

## Installation

```bash
pip install basemyai
```

Precompiled wheels are provided for Linux, Windows, and macOS (x86_64 and Apple Silicon).

## Quick start

```bash
# Local dev: create ~/.basemyai/key once (value never printed — back it up)
basemyai config key generate
```

```python
import asyncio
from basemyai import Memory

async def main():
    mem = await Memory.open(
        path="./agent.bmai",
        agent_id="assistant-42",
        # encryption_key optional — resolves BASEMYAI_DB_KEY, ~/.basemyai/key, etc.
        model_dir="~/.basemyai/models/all-MiniLM-L6-v2",
    )

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
| `recall(query, k)` | Temporal semantic recall |
| `recall_by_layer(query, layer, k)` | Recall scoped to one layer |
| `recall_hybrid(query, k)` | Vector + BM25 fused with RRF |
| `invalidate(id)` | Soft-delete (sets `valid_until` to now) |
| `forget(id)` | Physical delete (GDPR right to erasure) |
| `stats()` | Count of valid memories per layer |
| `add_graph_entity` / `add_graph_edge` | Insert graph facts |
| `recall_graph(start, max_depth)` | Multi-hop graph traversal |

### `Memory.open` parameters

| Parameter | Required | Description |
|---|---|---|
| `path` | yes | Path to the `.bmai` container file |
| `agent_id` | yes | Tenant identifier (per-agent isolation) |
| `encryption_key` | no | Passphrase override; if omitted, [ADR-034](https://github.com/basemyai/basemyai/blob/main/docs/security/key-resolution.md) resolution applies (`BASEMYAI_DB_KEY`, `BASEMYAI_DB_KEY_FILE`, `~/.basemyai/key`, …) |
| `model_dir` | no | Path to `all-MiniLM-L6-v2` model files |
| `device` | no | `"auto"`, `"cpu"`, `"cuda"`, or `"metal"` (default: `"auto"`) |
| `consent_to_fetch` | no | If `model_dir` is omitted, allow explicit model download (default: `false`) |

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
