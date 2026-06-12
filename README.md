<div align="center">
  <h1>🧠 BaseMyAI</h1>
  <p><strong>The local memory engine for AI agents.</strong></p>

  <p>
    <a href="#installation">Install</a> ·
    <a href="#quick-start">Quick Start</a> ·
    <a href="ADR.md">Architecture</a> ·
    <a href="PRD.md">PRD</a>
  </p>

  <a href="https://github.com/basemyai/basemyai/actions/workflows/ci.yml">
    <img src="https://github.com/basemyai/basemyai/actions/workflows/ci.yml/badge.svg" alt="CI">
  </a>
  <img src="https://img.shields.io/badge/rust-1.95%2B-orange?logo=rust" alt="Rust 1.95+">
  <img src="https://img.shields.io/badge/license-MIT-blue" alt="MIT License">
  <img src="https://img.shields.io/badge/platforms-Linux%20%7C%20Windows%20%7C%20macOS-lightgrey" alt="Platforms">
  <img src="https://img.shields.io/badge/python-PyO3-3776AB?logo=python" alt="Python via PyO3">
  <img src="https://img.shields.io/badge/node-NAPI--RS-339933?logo=node.js" alt="Node via NAPI-RS">
</div>

---

## The problem

AI agents have no memory. Every session starts from zero. The model that helped you yesterday doesn't remember a thing today.

Worse: the few solutions that *do* add memory ship your conversations, your users' data, and your embeddings to a cloud vector database. For anything sensitive — personal assistants, internal tools, regulated data — that's a non-starter.

And almost none of them handle **time**. A fact that was true last quarter is treated the same as a fact that's true right now. Agents confidently retrieve stale memories with no notion of "valid until".

## What BaseMyAI does

BaseMyAI is a **local memory engine**. It gives your AI agent persistent, temporal, multi-layered memory — embeddings, vector search, and time-aware retrieval — all inside a single local file. Zero cloud, zero data leaks.

```
Without BaseMyAI                With BaseMyAI
─────────────────────          ─────────────────────────────────────────
Agent remembers: nothing       Agent remembers:
(stateless every session)        + short-term working context
                                 + past episodes (what happened, when)
                                 + learned procedures (how to do X)
                                 + semantic facts (what is true, until when)
                               …with per-agent isolation, on-device.
```

It runs the embedding model **in-process** (pure Rust, no Python ML server), stores vectors **inside SQLite**, and never opens a network connection by default.

---

## Architecture: core + semantics

BaseMyAI is **two crates in one Cargo workspace**, publishable independently:

```
┌────────────────────────────────────────────────────────┐
│  basemyai          (the memory semantics)               │
│  4 memory layers · temporal RAG · per-agent isolation   │
│  mandatory encryption · Python / Node / REST surfaces   │
└───────────────────────┬────────────────────────────────┘
                        │ built on top of
┌───────────────────────▼────────────────────────────────┐
│  basemyai-core     (business-agnostic foundation)       │
│  hardened libSQL · native vectors · Candle embeddings   │
│  optional libSQL crypto · MaintenanceWorker             │
└─────────────────────────────────────────────────────────┘
```

- **`basemyai-core`** is **business-agnostic**. It knows pooling, vectors, embeddings, encryption, and a maintenance loop — and *nothing* about agents, time windows, or memory layers. It provides **mechanism**; the consumer provides **meaning**. Its `knn(query, k, filter?)` applies a SQL filter you pass in; its `MaintenanceWorker` runs tasks you register.
- **`basemyai`** is the memory product built on top: the 4 memory layers, temporal RAG, per-agent isolation, and the language bindings.

This split is deliberate: the same core powers both the Python/Node SDKs *and* a native Rust consumer (see [the ecosystem note](#ecosystem-powers-forgemyai)).

---

## The 4 memory layers

| Layer | Holds | Lifetime |
|---|---|---|
| `short_term` | Working context for the current session | Expires fast (TTL) |
| `episodic` | What happened and when (events, interactions) | Time-bounded |
| `procedural` | Learned how-to: steps, workflows, skills | Long-lived |
| `semantic` | Facts and knowledge, vector-searchable | Until invalidated |

Every layer carries `valid_from` / `valid_until` columns — memory is **temporal by construction**.

## Phase 2: Cognition

Beyond storage and vector search, BaseMyAI implements the full five-ingredient memory system from the Vision:

### Knowledge graph

Entities and relations live in the same libSQL file alongside vectors. Multi-hop traversal via recursive SQL CTE (`UNION`, not `UNION ALL` — cycle-safe by construction). Scoped per `agent_id` and depth-bounded.

```rust
graph.add_entity("alice", "person", "Alice")?;
graph.add_entity("acme",  "org",    "Acme Corp")?;
graph.add_edge("alice", "works_at", "acme", 1.0)?;
let reached = graph.traverse("alice", /* max_depth */ 2)?;
```

### Multi-signal retrieval (RRF)

Fuse vector similarity, graph traversal, and any other ranking signal with **Reciprocal Rank Fusion** (k = 60). Each signal contributes a ranked list; RRF scores and merges them deterministically.

```rust
let fused = rrf_fuse(&[
    Ranking { signal: "vector".into(), ids: vec![...] },
    Ranking { signal: "graph".into(),  ids: vec![...] },
], 10);
```

### Adaptive forgetting

A capacity-bounded GC that keeps the most *important and recent* memories. Importance decays with time using a **hyperbolic curve** `H / (H + age)` — not exponential, which underflows to zero at real Unix timestamps.

```rust
let gc = AdaptiveForgetting {
    capacity_per_agent: 10_000,
    recency_half_life_secs: 7 * 24 * 3600,  // 1 week
};
worker.register(gc);
```

### Episode → fact consolidation

`consolidate(memory, llm)` reads recent episodes, extracts entities, relations, and facts via a structured LLM prompt, and promotes them to the knowledge graph and semantic layer — idempotently. Works with any local LLM runner via the `LlmInference` trait.

---

## Temporal RAG

A retrieval that ignores time is a retrieval that lies. BaseMyAI's core query is **hybrid**: cosine similarity (libSQL native vectors, `vector_top_k`) **AND** a time filter.

```
retrieve("what is the user's billing plan?")
  → cosine match over semantic vectors
  → AND valid_until > now()
  → returns only memories that are both relevant AND still true
```

Vectors live **inside** libSQL via its **native vector** support (`F32_BLOB`, `libsql_vector_idx`, `vector_top_k` ANN — no extension to link). There is no second system to sync — no Qdrant, no LanceDB, no external vector DB. One file.

## Encryption at rest

`basemyai` requires encryption at rest via libSQL's built-in **`crypto`** feature: the database is instantiated with an `encryption_key`, and the file on disk is unreadable without it. The key is supplied at open time and never stored. (In `basemyai-core`, encryption is *optional*; `basemyai` makes it *mandatory*. The `crypto` feature requires CMake at build time.)

---

## Installation

```bash
# Python (the primary surface) — precompiled wheel, no compiler needed
pip install basemyai

# Node.js / TypeScript
npm install basemyai

# Rust (native crate)
cargo add basemyai          # full memory product
cargo add basemyai-core     # just the agnostic foundation
```

Wheels and NAPI prebuilds ship compiled — `pip install` / `npm install` never require a C or Rust toolchain on the client.

---

## Setup (hardware-aware)

Before first use, run a one-time setup that inspects your machine and provisions the right model — the way AnythingLLM picks a provider for your hardware.

```bash
basemyai setup
# detects RAM / GPU / VRAM / device (CUDA · Metal · CPU)
# selects the embedding model (baseline: all-MiniLM-L6-v2)
# fetches it explicitly (with checksum) → ~/.basemyai/models/
# persists { model_id, dim, device }
```

There is **no silent download at first run**. The fetch happens only here, with your consent. The embedder then receives an already-resolved model path and device — it never decides or downloads anything itself (see [ADR-010](ADR.md#adr-010)).

---

## Quick Start

```python
from basemyai import Memory

# Open an encrypted, per-agent memory store. The model path comes from
# `basemyai setup` — BaseMyAI never downloads it behind your back.
mem = Memory(
    path="./agent.db",
    agent_id="assistant-42",
    encryption_key="…",
    model_path="~/.basemyai/models/all-MiniLM-L6-v2",
)

# Remember a semantic fact, valid until the user changes plan.
mem.remember(
    "The user is on the Pro plan.",
    layer="semantic",
    valid_until=None,   # true until explicitly invalidated
)

# Recall — temporal RAG: relevant AND still valid, scoped to this agent.
hits = mem.recall("which plan is the user on?", k=5)
for h in hits:
    print(h.text, h.score)
```

The same engine, idiomatically, from Node:

```ts
import { Memory } from "basemyai";

const mem = new Memory({ path: "./agent.db", agentId: "assistant-42", encryptionKey: "…", modelPath: "…" });
await mem.remember("The user prefers dark mode.", { layer: "procedural" });
const hits = await mem.recall("ui preferences", { k: 5 });
```

---

## Consumption surfaces

The same Rust core, four ways to consume it:

| Surface | For | Layer consumed | Tech |
|---|---|---|---|
| Python SDK | Python agent builders (LangChain, LlamaIndex) | `basemyai` | PyO3 + wheel |
| Node SDK | JS/TS agent builders | `basemyai` | NAPI-RS + prebuild |
| REST sidecar | Go, Ruby, any other language | `basemyai` | single self-contained binary (axum) |
| **Native Rust crate** | **Rust programs (e.g. ForgeMyAI)** | **`basemyai-core`** | direct link, no FFI |

---

## Ecosystem: powers ForgeMyAI

`basemyai-core` is the foundation of more than one product. **[ForgeMyAI](../forgemyai-app/)** — the local code-context engine — consumes `basemyai-core` as a **native Rust crate** (the 4th surface above): no FFI, no HTTP, no overhead. It links the core directly for hardened libSQL, native libSQL vector KNN/ANN, and Candle embeddings, and builds its *own* code-specific semantics (symbols, call graph, FTS) on top.

> *BaseMyAI: the local memory engine. ForgeMyAI: the local code context engine, powered by BaseMyAI.*

See [`../ECOSYSTEM_ARCHITECTURE.md`](../ECOSYSTEM_ARCHITECTURE.md) for the full story.

---

## Security

- **100% local** — no data leaves your machine. No telemetry by default.
- **Per-agent isolation** — every query is filtered by `agent_id` at the SQL level. Cross-agent leakage is a security invariant, not a config option.
- **Encrypted at rest** — libSQL `crypto` feature, key never stored.
- **No silent network** — the embedder receives a local model path and never auto-downloads.

See [SECURITY.md](SECURITY.md) for the threat model and vulnerability reporting.

---

## License

MIT — see [LICENSE](LICENSE).
