# BaseMyAI Is Not A Vector Database

BaseMyAI uses vector search, but it is not a vector database product.

A vector database stores embeddings and metadata filters. That is useful, but it
leaves the hardest agent-memory work to the application: temporal validity,
per-agent isolation, memory layers, graph links, forgetting, consolidation,
encryption, and model provisioning.

BaseMyAI is an embedded agent memory database. The vector index is one mechanism
inside the engine, not the product boundary.

| Capability | Vector DB | BaseMyAI |
|---|---|---|
| Stores embeddings | Yes | Yes, inside local libSQL |
| Metadata filters | Yes | Yes, but scoped through memory semantics |
| Agent memory layers | No | short-term, episodic, procedural, semantic |
| Temporal truth | Usually app code | `valid_from` / `valid_until` in the schema |
| Per-agent isolation | Usually metadata convention | SQL-level invariant |
| Local encrypted file | Not the default product shape | `.bmai` local encrypted container |
| Graph context | Usually separate system | Entities and relations in the same file |
| Forgetting / GC | App code | Native maintenance tasks |
| Silent network avoided | Depends on deployment | Embedder never downloads; setup is explicit |

## Why This Matters

With a vector database, a developer still has to design the memory model:

```sql
WHERE agent_id = ?
  AND valid_from <= now()
  AND (valid_until IS NULL OR valid_until > now())
```

Then they must remember to apply that filter to every vector query, keyword
query, graph traversal, delete, export, and maintenance job.

BaseMyAI makes those rules part of the product API:

```python
await memory.remember("The user is on the Pro plan.", layer="semantic")
hits = await memory.recall("current billing plan", k=5)
```

The application asks a memory question. BaseMyAI enforces the memory contract.

## Positioning

- Qdrant, Chroma, LanceDB, pgvector, and FAISS are vector retrieval systems.
- Mem0 and LangMem are memory orchestration layers over pluggable stores.
- Graphiti models temporal knowledge graphs, usually with a graph backend.
- BaseMyAI is the local encrypted database file for private agent memory.

The short version:

> BaseMyAI is SQLite for private AI-agent memory, not another vector DB.
