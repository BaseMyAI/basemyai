# basemyai-core

[![crates.io](https://img.shields.io/crates/v/basemyai-core?color=dca282)](https://crates.io/crates/basemyai-core)
[![docs.rs](https://img.shields.io/docsrs/basemyai-core)](https://docs.rs/basemyai-core)
[![License](https://img.shields.io/crates/l/basemyai-core)](https://github.com/basemyai/basemyai/blob/main/LICENSE)

**Business-agnostic embedded foundation** for the BaseMyAI ecosystem — native storage engine, vector search, full-text search, graph primitives, optional Candle embeddings, encryption at rest, and an async maintenance worker.

This crate provides **mechanism**; consumers provide **meaning**. It knows nothing about `agent_id`, memory layers, temporal validity, or code symbols.

> For the full agent memory product, use [`basemyai`](https://crates.io/crates/basemyai). For ecosystem context, see the [main repository](https://github.com/basemyai/basemyai#architecture-core--semantics).

## Design principle

```
┌──────────────────────────────────────────────┐
│  basemyai          memory semantics           │
│  layers · temporal RAG · agent isolation      │
└────────────────────┬─────────────────────────┘
                     │ built on
┌────────────────────▼─────────────────────────┐
│  basemyai-core     business-agnostic core     │
│  storage · vectors · FTS · graph · embed      │
└──────────────────────────────────────────────┘
```

**Agnosticity invariant (ADR-001):** `basemyai-core` must not contain `agent_id`, `valid_from`/`valid_until`, memory layers, or code-domain types such as `Symbol`/`Edge`. A `grep` over `src/` for these concepts must return zero matches.

Primary consumer:

- [`basemyai`](https://crates.io/crates/basemyai) — agent memory semantics (this repo)

Third-party Rust crates may also depend on `basemyai-core` directly and supply their own semantics (never `basemyai`).

## What this crate provides

| Primitive | Type / trait |
|---|---|
| Storage engine | `StorageEngine`, `NativeEngine` |
| Engine capabilities | `EngineCapabilities`, `EngineKind` |
| Encryption key | `EncryptionKey` |
| Distance metric | `Metric` |
| Embeddings | `Embedder` trait, `CandleEmbedder` (feature `embed`) |
| Device selection | `Device` |
| Background tasks | `MaintenanceWorker`, `MaintenanceTask` |
| Errors | `CoreError` |

Semantic memory operations (`remember`, `recall`, layers, isolation) live in `basemyai::storage::NativeMemoryStore` — not here.

## Features

```toml
[dependencies]
basemyai-core = "0.1"

# Enable Candle in-process embeddings (all-MiniLM-L6-v2)
basemyai-core = { version = "0.1", features = ["embed"] }
```

| Feature | Description |
|---|---|
| *(default)* | Storage engine + maintenance loop (no ML) |
| `embed` | Candle BERT embedder (`all-MiniLM-L6-v2`, 384d) + tokenizers |

You can implement your own `Embedder` without enabling `embed` — useful for custom models or test fakes.

## Quick start

### Open the native engine

```rust
use basemyai_core::{EncryptionKey, NativeEngine};
use std::path::Path;

let engine = NativeEngine::open(Path::new("./data.bmai"))?;
// Or encrypted:
let key = EncryptionKey::new("your-secret-key");
let engine = NativeEngine::open_encrypted(Path::new("./data.bmai"), key.expose().as_bytes())?;
```

### Custom embedder (no Candle)

```rust
use basemyai_core::Embedder;

struct MyEmbedder;

impl Embedder for MyEmbedder {
    fn embed(&self, text: &str) -> basemyai_core::Result<Vec<f32>> {
        // Your embedding logic
        Ok(vec![0.0; 384])
    }
    fn embed_batch(&self, texts: &[String]) -> basemyai_core::Result<Vec<Vec<f32>>> {
        texts.iter().map(|t| self.embed(t)).collect()
    }
    fn model_id(&self) -> &str { "my-model-384" }
    fn dim(&self) -> usize { 384 }
}
```

### Maintenance worker

```rust
use basemyai_core::{MaintenanceWorker, MaintenanceTask};
use std::sync::Arc;

struct MyTask;
impl MaintenanceTask for MyTask {
    fn name(&self) -> &str { "my-task" }
    async fn run(&self) -> basemyai_core::Result<()> {
        // Periodic work injected by the consumer
        Ok(())
    }
}

let worker = MaintenanceWorker::new();
worker.register(Arc::new(MyTask));
// worker.spawn() — runs registered tasks on a schedule
```

## Native engine

The storage backend is the home-grown BaseMyAI engine ([ADR-024](https://github.com/basemyai/basemyai/blob/main/docs/adr/ADR-024-native-engine.md)/[025](https://github.com/basemyai/basemyai/blob/main/docs/adr/ADR-025-native-engine-storage-foundation.md)):

- LSM-tree with WAL + SSTables
- Native vector index (LM-DiskANN / Vamana)
- Native full-text search (BM25)
- Native graph storage (prefix-scoped KV layout)
- Encryption at rest ([ADR-030](https://github.com/basemyai/basemyai/blob/main/docs/adr/ADR-030-native-encryption-at-rest.md))

`basemyai-engine` is an internal, unpublished crate. You interact with it through `NativeEngine` and `StorageEngine` in this crate.

## Embedder policy

The `Embedder` **never downloads** and **never detects hardware**. It receives a resolved model path and `Device` from the consumer's setup layer (`basemyai::provision` or your own). Zero network after setup.

## Documentation

- [docs.rs](https://docs.rs/basemyai-core)
- [Main README](https://github.com/basemyai/basemyai)
- [Architecture decisions (ADR)](https://github.com/basemyai/basemyai/blob/main/docs/ADR.md)
- [ADR-001 — Two-crate split](https://github.com/basemyai/basemyai/blob/main/docs/adr/ADR-001-two-crates-split.md)

## License

Source-available under the [Business Source License 1.1](https://github.com/basemyai/basemyai/blob/main/LICENSE) (converts to Apache-2.0 four years after each version's release). See [ADR-031](https://github.com/basemyai/basemyai/blob/main/docs/adr/ADR-031-unified-busl-license.md).
