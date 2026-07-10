# basemyai — Node.js bindings

[![npm](https://img.shields.io/npm/v/basemyai?color=cb3837&label=npm)](https://www.npmjs.com/package/basemyai)
[![Node](https://img.shields.io/node/v/basemyai)](https://www.npmjs.com/package/basemyai)
[![License](https://img.shields.io/npm/l/basemyai)](https://github.com/basemyai/basemyai/blob/main/LICENSE)

**Local memory engine for AI agents** — Node.js / TypeScript SDK built with [NAPI-RS](https://napi.rs/) and distributed as precompiled native addons (no Rust toolchain required on the client).

BaseMyAI gives agents persistent, temporal, multi-layered memory: vector search, knowledge graph, hybrid retrieval, and per-agent isolation — all in one encrypted local `.bmai` file. Zero cloud. Zero silent downloads.

> This package wraps the Rust [`basemyai`](https://crates.io/crates/basemyai) crate. For the full product overview, architecture, and CLI, see the [main repository](https://github.com/basemyai/basemyai).

## Features

- Four memory layers: `short_term`, `episodic`, `procedural`, `semantic`
- Temporal RAG — only memories that are still valid are returned
- Hybrid recall — vector similarity + BM25 full-text, fused with Reciprocal Rank Fusion
- Knowledge graph — entities, relations, multi-hop traversal
- Per-agent isolation enforced structurally
- Encryption at rest (native envelope, XChaCha20-Poly1305) — passphrase at open time ([ADR-034](https://github.com/basemyai/basemyai/blob/main/docs/adr/ADR-034-user-key-resolution.md))
- Fully async API (`Promise`-based, backed by an internal Tokio runtime)
- TypeScript definitions included (`index.d.ts`)

## Requirements

- **Node.js 18+** (Node-API v9)
- A local embedding model (`all-MiniLM-L6-v2`, 384d) — provisioned once via the CLI or an explicit path

There is **no silent download at first run**. Fetch the model explicitly:

```bash
basemyai setup --fetch
```

## Installation

```bash
npm install basemyai
```

Prebuilds are provided for:

- `x86_64-pc-windows-msvc`
- `x86_64-unknown-linux-gnu`
- `x86_64-apple-darwin`
- `aarch64-apple-darwin`

## Quick start

```bash
# Local dev: create ~/.basemyai/key once (value never printed — back it up)
basemyai config key generate
```

```ts
import { Memory } from "basemyai";

const mem = await Memory.open({
  path: "./agent.bmai",
  agentId: "assistant-42",
  // encryptionKey optional — resolves BASEMYAI_DB_KEY, ~/.basemyai/key, etc.
  modelPath: "~/.basemyai/models/all-MiniLM-L6-v2",
});

// Store a procedural skill
const id = await mem.remember(
  "To deploy: run `make release`, tag, push.",
  "procedural",
);

// Temporal RAG
const hits = await mem.recall("how do I deploy?", 5);
for (const hit of hits) {
  console.log(hit.text, hit.score);
}

// Hybrid recall (vector + full-text)
const hybrid = await mem.recallHybrid("deploy runbook", 10);

// Knowledge graph
await mem.addGraphEntity("alice", "person", "Alice");
await mem.addGraphEntity("acme", "organization", "Acme");
await mem.addGraphEdge("alice", "works_at", "acme");
const reachable = await mem.recallGraph("alice", 2);

// Physical delete (GDPR right to erasure)
await mem.forget(id);

// Live subscriptions (ADR-022): invoke a callback for every remember /
// invalidate / forget / consolidate for this agent (optionally scoped to one
// layer). Isolation is enforced server-side — a mismatched agentId never
// receives anything, no matter what filter is requested here.
const handle = await mem.watch("assistant-42", undefined, (event) => {
  console.log(event.kind, event.layer, event.id); // never the memory's content
});

// Later: stop the relay and free the background task. Also happens
// automatically if the handle is garbage-collected, but calling close()
// explicitly is recommended.
handle.close();
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
| `Memory.open(options)` | Open an encrypted `.bmai` store with a local embedder |
| `remember(text, layer?)` | Store a memory; returns its UUID |
| `recall(query, k?)` | Temporal semantic recall |
| `recallByLayer(query, layer, k?)` | Recall scoped to one layer |
| `recallHybrid(query, k?)` | Vector + BM25 fused with RRF |
| `invalidate(id)` | Soft-delete (sets `valid_until` to now) |
| `forget(id)` | Physical delete |
| `stats()` | Count of valid memories per layer |
| `addGraphEntity` / `addGraphEdge` | Insert graph facts |
| `recallGraph(start, maxDepth?)` | Multi-hop graph traversal |
| `watch(agentId, layer?, callback)` | Live subscription (ADR-022): invokes `callback` with a `MemoryEventPayload` (`agentId`, `kind`, `layer`, `id`) for every memory mutation, isolated server-side by agent/layer. Resolves to a `WatchHandle` |
| `WatchHandle.close()` | Stop a `watch` subscription and free its background task (idempotent; also runs on GC) |

### `Memory.open` options

| Option | Required | Description |
|---|---|---|
| `path` | yes | Path to the `.bmai` container file |
| `agentId` | yes | Tenant identifier (per-agent isolation) |
| `encryptionKey` | no | Passphrase override; if omitted, [ADR-034](https://github.com/basemyai/basemyai/blob/main/docs/security/key-resolution.md) resolution applies |
| `modelPath` | no | Path to `all-MiniLM-L6-v2` model files |
| `allowModelDownload` | no | Allow explicit model download if `modelPath` is omitted (default: `false`) |

## Test-only API

`Memory.openInMemory(agentId)` is compiled **only** with the `test-util` feature. It uses an ephemeral store and a deterministic fake embedder — not part of the production SDK surface. Production code should always use `Memory.open(...)`.

## Related packages

| Package | Surface |
|---|---|
| [`basemyai`](https://crates.io/crates/basemyai) (Rust) | Native crate — full memory semantics |
| [`basemyai`](https://pypi.org/project/basemyai/) (Python) | PyO3 bindings |
| [`basemyai-core`](https://crates.io/crates/basemyai-core) | Business-agnostic foundation (for custom engines) |

## Documentation

- [Main README](https://github.com/basemyai/basemyai)
- [Key resolution (ADR-034)](https://github.com/basemyai/basemyai/blob/main/docs/security/key-resolution.md)
- [CLI reference](https://github.com/basemyai/basemyai/blob/main/docs/cli.md)
- [Architecture decisions (ADR)](https://github.com/basemyai/basemyai/blob/main/docs/ADR.md)

## License

Source-available under the [Business Source License 1.1](https://github.com/basemyai/basemyai/blob/main/LICENSE) (converts to Apache-2.0 four years after each version's release). See [ADR-031](https://github.com/basemyai/basemyai/blob/main/docs/adr/ADR-031-unified-busl-license.md).
