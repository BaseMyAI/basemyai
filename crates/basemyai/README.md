# basemyai

[![crates.io](https://img.shields.io/crates/v/basemyai?color=dca282)](https://crates.io/crates/basemyai)
[![docs.rs](https://img.shields.io/docsrs/basemyai)](https://docs.rs/basemyai)
[![License](https://img.shields.io/crates/l/basemyai)](https://github.com/basemyai/basemyai/blob/main/LICENSE)

**Local memory engine for AI agents** — four memory layers, temporal RAG, per-agent isolation, knowledge graph, and mandatory encryption at rest.

Built in Rust on top of [`basemyai-core`](https://crates.io/crates/basemyai-core). Everything stays on-device in an encrypted `.bmai` **directory** (WAL, SST, `crypto.meta`) powered by the native BaseMyAI storage engine. Production code opens stores **only** via `Memory::open_native` / `NativeMemoryStore::open_encrypted` — plaintext persistent stores exist behind the `test-util` feature for tests only.

> For the full product overview, bindings (Python, Node, REST), and CLI, see the [main repository](https://github.com/basemyai/basemyai).

## What this crate provides

`basemyai` is the **memory semantics** layer:

| Concept | Module |
|---|---|
| Four memory layers | `memory::MemoryLayer` |
| Per-agent isolation | `memory::AgentId` |
| Temporal validity | `temporal::Validity` |
| Vector + hybrid recall | `memory::Memory` |
| Knowledge graph | `cognition::Graph` |
| Episode → fact consolidation | `cognition::consolidate` |
| Multi-signal fusion (RRF) | `retrieval::rrf_fuse` |
| Hardware-aware provisioning | `provision` |
| Background maintenance | `maintenance` |

`basemyai-core` provides the business-agnostic foundation (storage engine, embeddings, encryption primitives). This crate adds agent memory meaning on top.

## Features

```toml
[dependencies]
basemyai = "0.1"

# Enable Candle in-process embeddings (all-MiniLM-L6-v2)
basemyai = { version = "0.1", features = ["embed"] }
```

| Feature | Description |
|---|---|
| `embed` (recommended) | Candle BERT embedder (`all-MiniLM-L6-v2`, 384d) |
| `test-util` | `HashEmbedder` + `Memory::open_in_memory` for tests only |

The native storage engine (`basemyai-engine`) is always included — it is the sole backend since [ADR-033](https://github.com/basemyai/basemyai/blob/main/docs/adr/ADR-033-native-only.md).

## Quick start

```rust
use basemyai::{AgentId, Memory, MemoryLayer};
use basemyai_core::{CandleEmbedder, Device, EncryptionKey, Embedder};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let key = EncryptionKey::resolve(None)?; // or EncryptionKey::new("…") for explicit override
    let agent = AgentId::new("agent-42").expect("non-empty id");
    let model_path = dirs::home_dir()
        .unwrap()
        .join(".basemyai/models/all-MiniLM-L6-v2");
    let embedder: Box<dyn Embedder> =
        Box::new(CandleEmbedder::load(&model_path, Device::Cpu)?);

    let mem = Memory::open_native("./agent.bmai", &key, embedder, agent).await?;

    mem.remember("User is on the Pro plan.", MemoryLayer::Semantic).await?;

    let hits = mem.recall("billing plan", 5).await?;
    for hit in &hits {
        println!("[{}] {:.3}  {}", hit.layer.table(), hit.score, hit.text);
    }

    Ok(())
}
```

### Memory layers

| Layer | Holds | Lifetime |
|---|---|---|
| `ShortTerm` | Working context for the active session | Expires fast |
| `Episodic` | Events and interactions | Time-bounded |
| `Procedural` | Learned workflows and skills | Long-lived |
| `Semantic` | Facts and knowledge | Until invalidated |

Every record carries `valid_from` / `valid_until` — memory is temporal by construction.

### Knowledge graph

```rust
let graph = mem.graph();
graph.add_entity("alice", "person", "Alice").await?;
graph.add_entity("acme", "org", "Acme Corp").await?;
graph.add_edge("alice", "works_at", "acme", 1.0).await?;

let reached = graph.traverse("alice", 2).await?;
```

### Multi-signal retrieval (RRF)

```rust
use basemyai::{Ranking, rrf_fuse};

let fused = rrf_fuse(&[
    Ranking { signal: "vector".into(), ids: vec![/* ... */] },
    Ranking { signal: "graph".into(),  ids: vec![/* ... */] },
], 10);
```

## Encryption

Encryption at rest is **mandatory**. Supply a passphrase via `EncryptionKey::new(...)` or resolve it with `EncryptionKey::resolve(None)?` ([ADR-034](https://github.com/basemyai/basemyai/blob/main/docs/adr/ADR-034-user-key-resolution.md) — env vars, Docker secrets, `~/.basemyai/key`). The engine never stores or logs the passphrase. Data on disk uses the native envelope scheme ([ADR-030](https://github.com/basemyai/basemyai/blob/main/docs/adr/ADR-030-native-encryption-at-rest.md), XChaCha20-Poly1305).

## Hardware-aware setup

The embedder never auto-downloads. Provision the baseline model explicitly:

```rust
let provision = basemyai::provision(/* consent_to_fetch */ true).await?;
// provision.model_path, provision.device
```

Or use the CLI: `basemyai setup --fetch`.

## Consumption surfaces

The same Rust core is also available via:

| Surface | Package |
|---|---|
| Python SDK | [`basemyai` on PyPI](https://pypi.org/project/basemyai/) |
| Node SDK | [`basemyai` on npm](https://www.npmjs.com/package/basemyai) |
| REST sidecar | `basemyai-rest` (binary, not on crates.io) |
| CLI | `basemyai-cli` (binary, not on crates.io) |

## Documentation

- [docs.rs](https://docs.rs/basemyai)
- [Main README](https://github.com/basemyai/basemyai)
- [Key resolution (ADR-034)](https://github.com/basemyai/basemyai/blob/main/docs/security/key-resolution.md)
- [CLI reference](https://github.com/basemyai/basemyai/blob/main/docs/cli.md)
- [Architecture decisions (ADR)](https://github.com/basemyai/basemyai/blob/main/docs/ADR.md)
- [BaseMyAI is not a vector DB](https://github.com/basemyai/basemyai/blob/main/docs/not-a-vector-db.md)

## License

Source-available under the [Business Source License 1.1](https://github.com/basemyai/basemyai/blob/main/LICENSE) (converts to Apache-2.0 four years after each version's release). See [ADR-031](https://github.com/basemyai/basemyai/blob/main/docs/adr/ADR-031-unified-busl-license.md).
