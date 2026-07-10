# N6 — BaseMyAI (native engine) vs Mem0 + Qdrant, rerun post-ADR-033 (2026-07-10)

**Machine**: same dev workstation as prior benches in this repo (see `docs/benchmarks/m6-knn-results-2026-07-01.md` for hardware notes). Windows, local Ollama (`llama3.2:1b` fact-extraction LLM, `all-minilm` embedder — both confirmed resident via `/api/ps` before each run, no cold-load penalty).

**Why this rerun exists**: `docs/benchmarks/local-memory-vs-mem0-qdrant.md` (2026-06-21) measured BaseMyAI on the **libSQL** backend. ADR-033 removed libSQL entirely; every BaseMyAI number below is the **native engine** (`basemyai-engine`, LSM + LM-DiskANN + native FTS/BM25), same harness (`benchmarks/p1-market/`), same corpus, same protocol as the original bench.

## Protocol deviation — Qdrant embedded local mode, not Docker

The original 2026-06-21 run used a real Qdrant server in Docker (`docker-compose.qdrant.yml`, `127.0.0.1:6333`). **Docker was not available in the environment that produced this rerun** (`docker: command not found`). `qdrant-client` ships an embedded "local mode" (on-disk, in-process, no server/REST hop) that Mem0 accepts via a pre-built `client` object passed into its config — this is still the real Qdrant engine (same HNSW/vector code paths), just without the REST round-trip. `run.py` was extended with `MEM0_QDRANT_LOCAL_PATH` to opt into this mode (see `benchmarks/p1-market/run.py` diff, 2026-07-10).

**This is a real, disclosed protocol change, not a fake/mocked backend** — but it means the Mem0/Qdrant numbers below are **not directly comparable** to the 2026-06-21 Docker-based numbers for Qdrant's own overhead (no REST hop this time, which the 2026-06-21 bench specifically flagged as a several-hundred-ms-to-seconds penalty on Windows via `localhost` DNS fallback — see that doc's "Gotchas"). If a Docker-comparable number is needed later, rerun with `docker compose -f docker-compose.qdrant.yml up -d` and the original `--host`/`--port` path (still supported, `local_path` is opt-in only).

## Results (real, measured — no numbers invented)

### BaseMyAI (native engine), N=500

| Operation | mean | p50 | p95 | p99 |
|---|---|---|---|---|
| `remember` | 317.8 ms | 372.1 ms | 517.8 ms | 607.2 ms |
| `recall` | 357.8 ms | 380.6 ms | 755.2 ms | 1030.1 ms |
| `recall_hybrid` | 350.8 ms | 368.2 ms | 686.0 ms | 1104.3 ms |

Output: `basemyai-bench.bmai`, 3.8 MB for 500 memories (encrypted, native container).

### Mem0 + Qdrant, `infer=True` (real Mem0 experience: LLM fact-extraction on every `add`), N=100

| Operation | mean | p50 | p95 | p99 |
|---|---|---|---|---|
| `remember` (`.add`) | 3420.6 ms | 3554.7 ms | 5779.4 ms | 9579.0 ms |
| `recall` (`.search`) | 95.2 ms | 95.6 ms | 118.3 ms | 131.5 ms |

Qdrant embedded-local storage: 1.3 MB for 100 memories.

### Mem0 + Qdrant, `infer=False` (storage-only, isolates vector-store cost from LLM orchestration), N=500

| Operation | mean | p50 | p95 | p99 |
|---|---|---|---|---|
| `remember` (raw `.add`, no LLM) | 133.7 ms | 91.5 ms | 187.9 ms | 601.9 ms |
| `recall` (`.search`) | 93.7 ms | 94.6 ms | 115.1 ms | 126.8 ms |

Qdrant embedded-local storage: 2.1 MB for 500 memories.

## Reading the numbers honestly

- **`remember`, real Mem0 usage (`infer=True`) vs BaseMyAI**: Mem0 is **~10.8×** slower on mean (3420.6 ms vs 317.8 ms), because every `.add()` pays a synchronous LLM call (`llama3.2:1b`) for fact extraction/reconciliation before the vector write. This is the core P1 claim this harness exists to check, and it holds: BaseMyAI's `remember` has **no LLM in its write path** by design (consolidation is a separate, explicitly-invoked background step, ADR-018), so it never pays this cost.
- **`remember`, storage-only (`infer=False`) vs BaseMyAI**: with the LLM removed from Mem0's path, the gap narrows sharply — Mem0's raw vector-store write (133.7 ms mean) is actually **faster** than BaseMyAI's `remember` (317.8 ms mean) in this run. BaseMyAI's `remember` does strictly more work per call than a bare Qdrant upsert: it embeds in-process (Candle, no network hop, but still CPU-bound inference), writes the vector index (LM-DiskANN), the FTS/BM25 mirror, and the memory record atomically in one transaction, encrypted at rest — Qdrant's embedded-local `infer=False` path here is closer to "vector upsert only" (embedding was still computed via Ollama's `all-minilm`, a separate process, not by Mem0/Qdrant themselves). **This is not a win for BaseMyAI on raw insert throughput** and should not be reported as one; the actual differentiator is the LLM-orchestration cost Mem0 forces onto every real `.add()`, not the storage engine itself.
- **`recall` vs BaseMyAI**: Mem0/Qdrant's `recall` (~93–95 ms mean, both infer modes) is **faster** than BaseMyAI's `recall`/`recall_hybrid` (~351–358 ms mean) in this run. BaseMyAI's recall path does more per query (temporal validity filtering, per-agent isolation checks, and for `recall_hybrid` a second FTS/BM25 pass fused by RRF) — again, not directly comparable to a bare Qdrant ANN search. This is a real, disclosed gap, not something to paper over.
- **Not measured / out of scope for this rerun**: recall@k accuracy (this harness measures latency only, not retrieval quality — see the original 2026-06-21 doc for the same caveat), a Docker-based Qdrant number for direct comparison with the 2026-06-21 run's own Qdrant overhead, and any run beyond N=500 (matches the original protocol's scope, not extended here).

## What to run for a Docker-comparable number later

```bash
cd benchmarks/p1-market
docker compose -f docker-compose.qdrant.yml up -d
# then omit MEM0_QDRANT_LOCAL_PATH — run.py falls back to --host/--port (127.0.0.1:6333)
bash run_all.sh
```
