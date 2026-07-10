<p align="center">
  <img height="140" src="./basemyai-branding/logo/logo-svg/logo-gradient.svg" alt="BaseMyAI">
</p>

<h3 align="center">The local memory engine for AI agents.</h3>
<p align="center"><em>Persistent · Temporal · Encrypted · 100 % local · Built in Rust</em></p>

<p align="center">
  <a href="https://github.com/basemyai/basemyai/actions/workflows/ci.yml">
    <img src="https://img.shields.io/github/actions/workflow/status/basemyai/basemyai/ci.yml?style=flat-square&branch=main" alt="CI">
  </a>
  &nbsp;
  <a href="https://github.com/basemyai/basemyai">
    <img src="https://img.shields.io/badge/built_with-Rust-dca282.svg?style=flat-square" alt="Built with Rust">
  </a>
  &nbsp;
  <a href="https://github.com/basemyai/basemyai">
    <img src="https://img.shields.io/badge/edition-2024-bde800.svg?style=flat-square" alt="Rust Edition 2024">
  </a>
  &nbsp;
  <a href="LICENSE">
    <img src="https://img.shields.io/badge/license-BUSL--1.1-00a88a.svg?style=flat-square" alt="Business Source License 1.1">
  </a>
</p>

<p align="center">
  <a href="https://crates.io/crates/basemyai">
    <img src="https://img.shields.io/crates/d/basemyai?color=dca282&label=rust&style=flat-square" alt="Crates.io">
  </a>
  &nbsp;
  <a href="https://pypi.org/project/basemyai/">
    <img src="https://img.shields.io/pypi/dm/basemyai?color=3776ab&label=python&style=flat-square" alt="PyPI">
  </a>
  &nbsp;
  <img src="https://img.shields.io/badge/platforms-Linux%20%7C%20Windows%20%7C%20macOS-lightgrey?style=flat-square" alt="Platforms">
</p>

<p align="center">
  <a href="https://discord.gg/basemyai">
    <img src="https://img.shields.io/badge/discord-join-5865f2.svg?style=flat-square" alt="Discord">
  </a>
  &nbsp;
  <a href="https://x.com/basemyai">
    <img src="https://img.shields.io/badge/x-follow_us-222222.svg?style=flat-square" alt="X">
  </a>
  &nbsp;
  <a href="https://dev.to/basemyai">
    <img src="https://img.shields.io/badge/dev-join_us-86f7b7.svg?style=flat-square" alt="Dev">
  </a>
  &nbsp;
  <a href="https://www.linkedin.com/company/basemyai/">
    <img src="https://img.shields.io/badge/linkedin-connect_with_us-0a66c2.svg?style=flat-square" alt="LinkedIn">
  </a>
  &nbsp;
  <a href="https://www.youtube.com/@basemyai">
    <img src="https://img.shields.io/badge/youtube-subscribe-fc1c1c.svg?style=flat-square" alt="YouTube">
  </a>
</p>

<p align="center">
  <a href="https://basemyai.com/blog"><img height="25" src="./basemyai-branding/social/blog.svg" alt="Blog"></a>
  &nbsp;
  <a href="https://github.com/basemyai/basemyai"><img height="25" src="./basemyai-branding/social/github.svg" alt="GitHub"></a>
  &nbsp;
  <a href="https://www.linkedin.com/company/basemyai/"><img height="25" src="./basemyai-branding/social/linkedin.svg" alt="LinkedIn"></a>
  &nbsp;
  <a href="https://x.com/basemyai"><img height="25" src="./basemyai-branding/social/x.svg" alt="X"></a>
  &nbsp;
  <a href="https://www.youtube.com/@basemyai"><img height="25" src="./basemyai-branding/social/youtube.svg" alt="YouTube"></a>
  &nbsp;
  <a href="https://dev.to/basemyai"><img height="25" src="./basemyai-branding/social/dev.svg" alt="Dev"></a>
  &nbsp;
  <a href="https://discord.gg/GQAxwkzyuU"><img height="25" src="./basemyai-branding/social/discord.svg" alt="Discord"></a>
  &nbsp;
  <a href="https://stackoverflow.com/questions/tagged/basemyai"><img height="25" src="./basemyai-branding/social/stack-overflow.svg" alt="Stack Overflow"></a>
</p>

<br>

<h2><img height="20" src="./basemyai-branding/icons/documentation.svg">&nbsp;&nbsp;What is BaseMyAI?</h2>

BaseMyAI is a **local memory engine** built in Rust that gives AI agents persistent, temporal, multi-layered memory — vector search, knowledge graph, and time-aware retrieval — inside a single encrypted `.bmai` container. Zero cloud. Zero data leaks. Zero silent downloads.

AI agents have no memory by default. Every session starts from zero. Worse — the few solutions that _do_ add memory route your conversations and embeddings to a cloud vector database. For anything sensitive — personal assistants, internal tools, regulated industries — that is a non-starter. And almost none of them handle **time**: a fact that was true last quarter is treated identically to a fact that is true right now.

BaseMyAI solves all three problems in a single Rust binary:

- **Privacy-first** — everything stays on-device, in one encrypted `.bmai` container (native engine, XChaCha20-Poly1305 at rest — no CMake, no cloud)
- **Temporal** — every memory carries `valid_from` / `valid_until`; retrieval returns only what is _currently_ true
- **Multi-signal** — vector similarity + knowledge graph + Reciprocal Rank Fusion in one query

BaseMyAI uses vectors, but it is **not another vector database**. It is an
embedded memory database for agents: isolation, temporal truth, layers, graph,
forgetting, and encryption are part of the product contract. See
[BaseMyAI is not a vector DB](docs/not-a-vector-db.md).

<h2><img height="20" src="./basemyai-branding/icons/contents.svg">&nbsp;&nbsp;Contents</h2>

- [What is BaseMyAI?](#what-is-basemyai)
- [Features](#features)
- [Architecture: core + engine + semantics](#architecture-core--engine--semantics)
- [The 4 memory layers](#the-4-memory-layers)
- [Phase 2 — Cognition](#phase-2--cognition)
- [Temporal RAG](#temporal-rag)
- [Encryption at rest](#encryption-at-rest)
- [P1 public proofs](#p1-public-proofs)
- [Getting started](#getting-started)
- [Installation](#installation)
- [Quick look](#quick-look)
- [Consumption surfaces](#consumption-surfaces)
- [Community](#community)
- [Contributing](#contributing)
- [Security](#security)
- [License](#license)

<h2><img height="20" src="./basemyai-branding/icons/features.svg">&nbsp;&nbsp;Features</h2>

- [x] 100 % local — no data leaves your machine, no telemetry by default
- [x] Pure Rust native storage engine (`basemyai-engine`) — LSM WAL+SST, no external database
- [x] Four memory layers: short-term, episodic, procedural, semantic
- [x] Temporal RAG — retrieval filtered by `valid_until`, never returns stale facts
- [x] Native vector search (LM-DiskANN / Vamana ANN, recall@10 = 1.0 at 10k/100k)
- [x] Knowledge graph — entities, relations, multi-hop BFS traversal
- [x] Hybrid search — vector similarity **+** native BM25 full-text, fused with Reciprocal Rank Fusion
- [x] Multi-signal retrieval with Reciprocal Rank Fusion (vector + graph, k = 60)
- [x] Adaptive forgetting — hyperbolic importance × recency, capacity-bounded GC
- [x] Episode-to-fact consolidation via injected LLM (any local runner, no hard dependency)
- [x] Hardware-aware provisioning — no silent model downloads, explicit setup command
- [x] Encryption at rest via native envelope (ADR-030); centralized passphrase resolution (ADR-034)
- [x] Per-agent isolation enforced structurally by key layout — cross-agent leakage is a security invariant
- [x] MCP server (stdio + HTTP), CLI (`basemyai`), REST sidecar (axum), Python SDK (PyO3), Node SDK (NAPI-RS), native Rust crate

<h2><img height="20" src="./basemyai-branding/icons/tick.svg">&nbsp;&nbsp;P1 Public Proofs</h2>

- [Benchmark harness: BaseMyAI local vs Mem0 + Qdrant local](docs/benchmarks/local-memory-vs-mem0-qdrant.md)
- [Adversarial isolation test](crates/basemyai/tests/p1_isolation_adversarial.rs)
- [Temporal replacement demo](crates/basemyai/examples/temporal_replacement.rs)
- [Zero network after setup](docs/zero-network-after-setup.md)
- [BaseMyAI is not a vector DB](docs/not-a-vector-db.md)

<img width="100%" src="./basemyai-branding/img/basemyai-memory-engine.png.png" alt="BaseMyAI memory engine" />

<h2><img height="20" src="./basemyai-branding/icons/documentation.svg">&nbsp;&nbsp;Architecture: core + engine + semantics</h2>

BaseMyAI is a **Cargo workspace** with two publishable crates (`basemyai-core`, `basemyai`) and an internal native engine (`basemyai-engine`, ADR-024/032):

```
┌────────────────────────────────────────────────────────┐
│  basemyai          (the memory semantics)               │
│  4 memory layers · temporal RAG · per-agent isolation   │
│  adaptive forgetting · cognition · MCP/CLI/REST/SDKs    │
└───────────────────────┬────────────────────────────────┘
                        │ built on top of
┌───────────────────────▼────────────────────────────────┐
│  basemyai-core     (business-agnostic foundation)       │
│  StorageEngine trait · Candle embedder · maintenance    │
└───────────────────────┬────────────────────────────────┘
                        │ backed by
┌───────────────────────▼────────────────────────────────┐
│  basemyai-engine   (native storage engine)            │
│  LSM (WAL+SST) · ANN vector index · native FTS/BM25   │
│  knowledge graph · encryption envelope (ADR-030)      │
└────────────────────────────────────────────────────────┘
```

**`basemyai-core`** is deliberately business-agnostic. It exposes engine capabilities, Candle in-process embeddings, encryption primitives, and a background maintenance loop — nothing about agents, time windows, or memory layers. It provides **mechanism**; the consumer provides **meaning**.

**`basemyai-engine`** is the durable storage layer: crash-consistent LSM, LM-DiskANN vector index, inverted FTS index, graph traversal, and at-rest encryption — all in pure Rust, no libSQL/SQLite dependency.

**`basemyai`** is the memory product built on top: the four layers, temporal RAG, per-agent isolation enforced structurally in the key layout, and all language binding surfaces.

Since **[ADR-032](docs/adr/ADR-032-native-only.md)** (2026-07-08), the native engine is the **only** active backend. libSQL/V1 compatibility paths have been removed from the workspace.

`basemyai-core` is also designed for third-party Rust consumers that build their own semantics on the same foundation (no FFI, no HTTP). See [`../ECOSYSTEM_ARCHITECTURE.md`](../ECOSYSTEM_ARCHITECTURE.md) for the wider ecosystem.

<h2><img height="20" src="./basemyai-branding/icons/features.svg">&nbsp;&nbsp;The 4 memory layers</h2>

| Layer        | Holds                                         | Lifetime                     |
| ------------ | --------------------------------------------- | ---------------------------- |
| `short_term` | Working context for the active session        | Expires fast (TTL)           |
| `episodic`   | What happened and when — events, interactions | Time-bounded                 |
| `procedural` | Learned how-to: steps, workflows, skills      | Long-lived                   |
| `semantic`   | Facts and knowledge, vector-searchable        | Until explicitly invalidated |

Every layer carries `valid_from` / `valid_until` — memory is **temporal by construction**, not as an afterthought.

<img width="100%" src="./basemyai-branding/img/basemyai-branch-your-agent.png" alt="Branch your agent" />

<h2><img height="20" src="./basemyai-branding/icons/features.svg">&nbsp;&nbsp;Phase 2 — Cognition</h2>

Beyond storage and vector search, BaseMyAI implements a full five-ingredient memory system.

### Knowledge graph

Entities and relations live in the same `.bmai` container alongside vectors. Multi-hop traversal via native BFS in `basemyai-engine` (cycle-safe, depth-bounded). Scoped per `agent_id` through structural key isolation.

```rust
graph.add_entity("alice", "person", "Alice")?;
graph.add_entity("acme",  "org",    "Acme Corp")?;
graph.add_edge("alice", "works_at", "acme", 1.0)?;

let reached = graph.traverse("alice", /* max_depth */ 2)?;
```

### Multi-signal retrieval (RRF)

Fuse vector similarity, graph traversal, and any other ranking signal with **Reciprocal Rank Fusion** (k = 60). Each signal contributes a ranked list; RRF scores and merges them deterministically — no tunable weights to overfit.

```rust
let fused = rrf_fuse(&[
    Ranking { signal: "vector".into(), ids: vec![...] },
    Ranking { signal: "graph".into(),  ids: vec![...] },
], /* top_k */ 10);
```

### Adaptive forgetting

A capacity-bounded GC that keeps the most _important and recent_ **active** memories. Importance decays over time using a **hyperbolic curve** `H / (H + age)` — stable at real Unix timestamps without floating-point underflow. Invalidated/expired memories are never counted toward capacity — that's the separate expired-memory GC below.

```rust
let policy = AdaptiveForgettingPolicy {
    capacity: 10_000,
    recency_half_life_secs: 7 * 24 * 3600,  // 1 week
};
worker.register(Duration::from_secs(3600), Arc::new(AdaptiveForgettingTask::new(memory.clone(), policy)));
```

### Expired-memory GC

A second, disjoint mechanism: physically deletes memories whose `valid_until` has already passed (invalidated explicitly, or expired by their validity window) — paginated by cursor, idempotent, resumable after an interruption.

```rust
let report = memory.expired_gc(/* page_size */ 1_000).await?;
// or as a background task, alongside AdaptiveForgettingTask:
worker.register(Duration::from_secs(3600), Arc::new(ExpiredMemoryGcTask::new(memory.clone(), 1_000)));
```

### Episode → fact consolidation

`consolidate(memory, llm)` reads recent episodes, extracts entities, relations, and facts via a structured LLM prompt, and promotes them to the knowledge graph and semantic layer — idempotently, via any local LLM runner through the injected `LlmInference` trait.

```rust
consolidate(&memory, &llm_backend).await?;
// episodes → (entity, relation, fact) → knowledge graph + semantic layer
```

<h2><img height="20" src="./basemyai-branding/icons/security.svg">&nbsp;&nbsp;Temporal RAG</h2>

A retrieval that ignores time is a retrieval that lies. BaseMyAI's core query is **hybrid**: native vector similarity **AND** temporal validity filtering in the same retrieval pipeline.

```
retrieve("what is the user's billing plan?")
  → ANN cosine match (native index)
  → AND valid_until > now()
  → returns only memories that are both relevant AND still true
```

Vectors, graph edges, and FTS all live in BaseMyAI's **native engine** (`basemyai-engine`): no external vector database, no SQL surface, no sync layer.

A `.bmai` file is a native engine container (WAL + SST + metadata). It is **not** a SQLite/libSQL database and is not interchangeable with V1 libSQL artifacts.

<h2><img height="20" src="./basemyai-branding/icons/security.svg">&nbsp;&nbsp;Encryption at rest</h2>

`basemyai` requires encryption at rest via the native envelope scheme (ADR-030, XChaCha20-Poly1305). The passphrase is supplied at open time and **never** stored by the engine, never logged, never written to `config.toml`.

**User key resolution (ADR-034)** — CLI, REST, MCP, and SDK bindings resolve the passphrase from the same ordered sources:

| Priority | Source                                                      |
| -------- | ----------------------------------------------------------- |
| 1        | Explicit argument (`encryption_key` / `EncryptionKey::new`) |
| 2        | `BASEMYAI_DB_KEY`                                           |
| 3        | `BASEMYAI_ENCRYPTION_KEY` (legacy alias)                    |
| 4        | `BASEMYAI_DB_KEY_FILE`                                      |
| 5        | `/run/secrets/basemyai_db_key` (Docker secrets)             |
| 6        | `~/.basemyai/key` (`basemyai config key generate`)          |

Full operator guide: [`docs/security/key-resolution.md`](docs/security/key-resolution.md).

<p align="center">
  <img width="75%" src="./basemyai-branding/img/basemyai-database-plugin.png" alt="Database plugin architecture" />
</p>

<h2><img height="20" src="./basemyai-branding/icons/gettingstarted.svg">&nbsp;&nbsp;Getting started</h2>

Getting started with BaseMyAI takes three steps: provision a local encryption passphrase, run `basemyai setup` once for the embedding model, then open a `Memory` from your language of choice.

```bash
basemyai config key generate   # creates ~/.basemyai/key — value never printed; back it up
basemyai config key check      # verify a passphrase source is available
basemyai setup --fetch         # download + verify the baseline embedder (explicit consent)
```

**Python**

```python
from basemyai import Memory

mem = await Memory.open(
    path="./agent.bmai",
    agent_id="assistant-42",
    # encryption_key optional — ADR-034 resolves env / ~/.basemyai/key / BASEMYAI_DB_KEY_FILE
    model_dir="~/.basemyai/models/all-MiniLM-L6-v2",
)

# Store a semantic fact, valid until explicitly invalidated.
await mem.remember(
    "The user is on the Pro plan.",
    layer="semantic",
)

await mem.add_graph_entity("alice", "person", "Alice")
await mem.add_graph_entity("acme", "organization", "Acme")
await mem.add_graph_edge("alice", "works_at", "acme")
graph_hits = await mem.recall_graph("alice", max_depth=2)

# Temporal RAG: relevant AND still valid, scoped to this agent.
hits = await mem.recall("which plan is the user on?", k=5)
for h in hits:
    print(h.text, h.score)
```

**Node / TypeScript**

```ts
import { Memory } from "basemyai";

const mem = await Memory.open({
  path: "./agent.bmai",
  agentId: "assistant-42",
  // encryptionKey optional — ADR-034 resolves env / ~/.basemyai/key / BASEMYAI_DB_KEY_FILE
  modelPath: "~/.basemyai/models/all-MiniLM-L6-v2",
});

await mem.remember("The user prefers dark mode.", "procedural");
await mem.addGraphEntity("alice", "person", "Alice");
await mem.addGraphEntity("acme", "organization", "Acme");
await mem.addGraphEdge("alice", "works_at", "acme");
const graphHits = await mem.recallGraph("alice", 2);

const hits = await mem.recall("ui preferences", 5);
```

`open_in_memory` (Python) and `openInMemory` (Node) intentionally stay
test-only. They are compiled only with the `test-util` feature, use an
ephemeral native temp store plus a deterministic fake embedder, and are not
part of the documented production SDK surface. Production code should use
`Memory.open(...)` with an encrypted file store and a local model path.

**Rust (native)**

```rust
use basemyai::{AgentId, Memory, MemoryLayer};
use basemyai_core::{CandleEmbedder, Device, EncryptionKey, Embedder};

let key = EncryptionKey::resolve(None)?; // or EncryptionKey::new("…") to pass explicitly
let agent = AgentId::new("agent-42").unwrap();
let model_path = dirs::home_dir().unwrap().join(".basemyai/models/all-MiniLM-L6-v2");
let embedder: Box<dyn Embedder> =
    Box::new(CandleEmbedder::load(&model_path, Device::Cpu)?);

let mem = Memory::open_native("./agent.bmai", &key, embedder, agent).await?;
mem.remember("User is on Pro plan.", MemoryLayer::Semantic).await?;
let hits = mem.recall("billing plan", 5).await?;
```

<h2><img height="20" src="./basemyai-branding/icons/installation.svg">&nbsp;&nbsp;Installation</h2>

BaseMyAI is designed to be simple to install. Precompiled wheels (Python) and NAPI prebuilds (Node) are the packaging target so client machines do not need a C or Rust toolchain.

<h4><img width="20" src="./basemyai-branding/icons/apple.svg">&nbsp;&nbsp;Python (all platforms)</h4>

```bash
pip install basemyai
```

<h4><img width="20" src="./basemyai-branding/icons/cloud.svg">&nbsp;&nbsp;Node / TypeScript</h4>

The public npm package is not live yet: `npm view basemyai` returned `404` from
the public registry as of 2026-07-08. Use this command only after the npm release
workflow has published and verified `basemyai`.

```bash
npm install basemyai
```

<h4><img width="20" src="./basemyai-branding/icons/tick.svg">&nbsp;&nbsp;Rust (native crate)</h4>

```toml
# Full memory product
basemyai = "0.1"

# Business-agnostic foundation only (custom Rust consumers)
basemyai-core = "0.1"
```

<h4><img width="20" src="./basemyai-branding/icons/docker.svg">&nbsp;&nbsp;Docker (REST sidecar)</h4>

Prefer a **Docker secret file** over inline env vars (ADR-034). See
[`docs/security/key-resolution.md`](docs/security/key-resolution.md).

```yaml
# docker-compose.yml (excerpt)
services:
  basemyai-rest:
    image: basemyai/basemyai-rest:latest
    secrets:
      - basemyai_db_key
    environment:
      BASEMYAI_DB_KEY_FILE: /run/secrets/basemyai_db_key
      BASEMYAI_REST_DB_PATH: /data/memory.bmai
    volumes:
      - ./data:/data
    ports:
      - "7743:7743"

secrets:
  basemyai_db_key:
    file: ./secrets/basemyai_db_key.txt
```

```bash
mkdir -p secrets && chmod 700 secrets
openssl rand -hex 32 > secrets/basemyai_db_key.txt
chmod 600 secrets/basemyai_db_key.txt
docker compose up basemyai-rest
```

<h4><img width="20" src="./basemyai-branding/icons/linux.svg">&nbsp;&nbsp;Install on Linux</h4>

```bash
curl --proto '=https' --tlsv1.2 -sSf https://install.basemyai.com | sh
```

<h4><img width="20" src="./basemyai-branding/icons/windows.svg">&nbsp;&nbsp;Install on Windows</h4>

```ps1
iwr https://windows.basemyai.com -useb | iex
```

<h4><img width="20" src="./basemyai-branding/icons/apple.svg">&nbsp;&nbsp;Install on macOS</h4>

```bash
brew install basemyai/tap/basemyai
```

<h4>Hardware-aware setup (run once)</h4>

```bash
basemyai setup
# Detecting hardware…  GPU: CUDA 12.3 · 8 GB VRAM
# Selected model: all-MiniLM-L6-v2 (384d, ~90 MB)
# Fetching model… ████████████████ 100%  SHA-256 ✓
# Saved to ~/.basemyai/models/
```

There is **no silent download at first run**. The fetch happens only here, with your explicit consent. The embedder then receives an already-resolved model path and device — it never decides or downloads anything itself.

Copy [`.env.example`](.env.example) to `.env` for local env vars (placeholders only — never commit real secrets).

<h4>Developer CLI</h4>

The `basemyai` binary wraps the engine for scripting and inspection. Every
command that opens a `.bmai` container needs an encryption passphrase (ADR-034).
There is no CLI flag for the key and no plaintext open.

```bash
basemyai config key generate  # local dev: ~/.basemyai/key (chmod 600 on Unix)
basemyai config key check     # verify resolution before scripting
basemyai setup --fetch        # provision the baseline embedder (explicit consent)
basemyai status               # detected hardware + persisted provisioning config
basemyai init ./agent.bmai    # create an encrypted .bmai container
basemyai inspect              # container metadata + memory count
basemyai verify               # validate container + expected format version

basemyai remember "User is on Pro plan." --layer semantic
basemyai recall "billing plan" -k 5 --hybrid
basemyai list --layer semantic
basemyai stats
basemyai invalidate <id>      # soft-delete: valid_until = now
basemyai forget <id>          # physical delete — GDPR right to erasure
basemyai export --out backup.jsonl
basemyai import --file backup.jsonl

basemyai graph add-entity alice person "Alice"
basemyai graph add-edge alice works_at acme
basemyai graph traverse alice --depth 2

basemyai gc                                 # delete memories past their valid_until
basemyai forget-adaptive --capacity 10000   # evict least-retained active memories beyond capacity
basemyai consolidate          # episodes → facts + graph (requires local LLM)

basemyai llm detect           # discover local LLM servers + best model
basemyai llm suggest          # installable models matched to your hardware
```

Set `BASEMYAI_DB_PATH` and `BASEMYAI_AGENT` (or use `--db` / `--agent` flags).
For the passphrase, prefer `basemyai config key generate` locally, or set
`BASEMYAI_DB_KEY` / `BASEMYAI_DB_KEY_FILE` in CI and Docker — see
[`docs/security/key-resolution.md`](docs/security/key-resolution.md).
Full CLI reference: [docs/cli.md](docs/cli.md).

<h2><img height="20" src="./basemyai-branding/icons/features.svg">&nbsp;&nbsp;Quick look</h2>

Store an episodic memory — what happened and when.

```python
await mem.remember(
    "User asked to refactor the auth module at 14:32.",
    layer="episodic",
)
```

Store a procedural skill the agent learned.

```python
await mem.remember(
    "To deploy: run `make release`, tag the commit, push to origin.",
    layer="procedural",
)
```

Hybrid recall — fuses vector similarity and full-text search with RRF.

```python
hits = await mem.recall_hybrid("how do I deploy?", k=10)
```

Invalidate a fact that is no longer true.

```python
await mem.invalidate("semantic:abc123")
# valid_until is set to now() — future recalls skip this record
```

Traverse graph entities that have already been consolidated.

```python
reachable = await mem.recall_graph("alice", max_depth=3)
```

Insert graph facts directly from SDKs when the caller already knows the
entities and relation.

```python
await mem.add_graph_entity("alice", "person", "Alice")
await mem.add_graph_entity("acme", "organization", "Acme")
await mem.add_graph_edge("alice", "works_at", "acme")
```

```ts
await mem.addGraphEntity("alice", "person", "Alice");
await mem.addGraphEntity("acme", "organization", "Acme");
await mem.addGraphEdge("alice", "works_at", "acme");
```

Recall within one memory layer.

```python
procedures = await mem.recall_by_layer("how do I deploy?", "procedural", k=5)
```

<h2><img height="20" src="./basemyai-branding/icons/features.svg">&nbsp;&nbsp;Consumption surfaces</h2>

The same Rust core, six ways to consume it:

| Surface               | For                                                                | Crate consumed  | Tech                                                                 |
| --------------------- | ------------------------------------------------------------------ | --------------- | -------------------------------------------------------------------- |
| **MCP server**        | AI agents (Claude Code, Cursor, custom MCP clients)                | `basemyai`      | stdio + HTTP, 8 tools — see [MCP install guide](docs/mcp-install.md) |
| **Python SDK**        | Python agent builders (LangChain, LlamaIndex, custom)              | `basemyai`      | PyO3 + precompiled wheel                                             |
| **Node SDK**          | JS / TS agent builders                                             | `basemyai`      | NAPI-RS + prebuild                                                   |
| **REST sidecar**      | Go, Ruby, any HTTP client                                          | `basemyai`      | axum, `/v1` routes + SSE live subscriptions                          |
| **CLI** (`basemyai`)  | Scripting, ops, ad-hoc inspection, agent-as-tool (`--format json`) | `basemyai`      | Single binary (clap) — see [CLI reference](docs/cli.md)              |
| **Native Rust crate** | Rust programs on the agnostic core                                 | `basemyai-core` | Direct link, zero FFI overhead                                       |

<h2><img height="20" src="./basemyai-branding/icons/community.svg">&nbsp;&nbsp;Community</h2>

Join our growing community around the world, for help, ideas, and discussions regarding BaseMyAI.

- View our official [Blog](https://basemyai.com/blog)
- Chat live with us on [Discord](https://discord.gg/basemyai)
- Follow us on [X](https://x.com/basemyai)
- Connect with us on [LinkedIn](https://www.linkedin.com/company/basemyai/)
- Visit us on [YouTube](https://www.youtube.com/@basemyai)
- Join our [Dev community](https://dev.to/basemyai)
- Questions tagged #basemyai on [Stack Overflow](https://stackoverflow.com/questions/tagged/basemyai)

<h2><img height="20" src="./basemyai-branding/icons/contributing.svg">&nbsp;&nbsp;Contributing</h2>

We would love for you to get involved with BaseMyAI development! If you wish to help, you can learn more about how you can contribute to this project in the [contribution guide](CONTRIBUTING.md).

Architecture decisions are documented in [docs/ADR.md](docs/ADR.md) (index) with each decision in its own file under [docs/adr/](docs/adr/). A decision that changes always produces a **new ADR** — existing ADRs are never edited. Read the ADR before touching cross-cutting concerns. Implementation status: [docs/status.md](docs/status.md).

Rust gate before every commit — `cargo xtask ci` (fmt + per-crate clippy/test with the exact feature combinations CI uses; plain `cargo clippy --workspace`/`cargo test --workspace` do **not** reproduce CI):

```bash
cargo xtask ci
```

<h2><img height="20" src="./basemyai-branding/icons/security.svg">&nbsp;&nbsp;Security</h2>

For security issues, kindly email us at [security@basemyai.com](mailto:security@basemyai.com) instead of posting a public issue on GitHub.

- **100 % local** — no data leaves your machine, no telemetry by default
- **Per-agent isolation** — every access is scoped structurally by `agent_id`; cross-agent leakage is a security invariant, not a config option
- **Encrypted at rest** — native envelope (ADR-030); passphrase resolved per ADR-034, never stored in config
- **No silent network** — the embedder receives a local model path and never auto-downloads

See [SECURITY.md](SECURITY.md) and [docs/security/key-resolution.md](docs/security/key-resolution.md) for the threat model, key custody, and vulnerability reporting.

<h2><img height="20" src="./basemyai-branding/icons/license.svg">&nbsp;&nbsp;License</h2>

BaseMyAI (every crate in this repository, plus the Python/Node bindings) is
source-available under the **Business Source License 1.1**, converting
automatically to Apache-2.0 four years after each version's release (see
[ADR-031](docs/adr/ADR-031-unified-busl-license.md)).

In plain terms: you're free to depend on BaseMyAI inside your own product —
including a commercial one — use it internally, for research, or evaluation.
What's restricted is republishing BaseMyAI itself (or a fork of it, under any
name) as a competing memory/vector/graph/code-context engine, or reselling it
as a hosted service. See the Additional Use Grant in [LICENSE](LICENSE) for
the exact terms.

The "BaseMyAI" and "ForgeMyAI" names and logos are trademarks, governed
independently of the code license — see
[TRADEMARK_POLICY.md](TRADEMARK_POLICY.md) for what's freely permitted and
what requires permission.

For more information, see [LICENSE](LICENSE) and
[ADR-031](docs/adr/ADR-031-unified-busl-license.md).
