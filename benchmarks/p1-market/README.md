# P1 Market Benchmark: BaseMyAI Local vs Mem0 + Qdrant Local

This benchmark is a reproducible harness for the public P1 claim:

> BaseMyAI is a local encrypted agent memory database, not an orchestration layer
> over a separate vector database.

It measures the same workload against:

- `basemyai`: local `.bmai` file, in-process bindings, local embedding model.
- `mem0_qdrant`: Mem0 OSS configured with a local Qdrant container.

No benchmark result is checked into the repo until it has been run on a named
machine. The harness writes raw JSON and CSV so results can be audited.

## Workload

- `remember`: insert one memory at a time.
- `recall`: retrieve `k=5` memories.
- `recall_hybrid`: BaseMyAI only, because Mem0's public API does not expose the
  same RRF/BM25 contract as BaseMyAI.

Metrics:

- p50 / p95 / p99 latency in milliseconds.
- total throughput.
- output database size where available.

## Setup

```bash
cd benchmarks/p1-market
python -m venv .venv
. .venv/bin/activate
pip install -r requirements.txt
docker compose -f docker-compose.qdrant.yml up -d
```

BaseMyAI requires a local model directory and an encryption key:

```bash
export BASEMYAI_BENCH_DB=./basemyai-bench.bmai
export BASEMYAI_BENCH_AGENT=bench-agent
export BASEMYAI_BENCH_MODEL_DIR=/path/to/all-MiniLM-L6-v2
export BASEMYAI_BENCH_KEY='dev-benchmark-key'
```

Mem0 + Qdrant requires a provider configuration accepted by Mem0. The default
uses Qdrant on `localhost:6333` for storage and a **local Ollama** for both LLM
(fact extraction) and embeddings — no API key needed, matching the
"local-first" framing of this benchmark:

```bash
ollama pull llama3.2:1b   # fact-extraction LLM mem0 calls on every .add()
ollama pull all-minilm    # embedder, 384d — same dimensionality as basemyai's baseline model
```

If you want an API-backed Mem0 run instead (e.g. to compare against OpenAI),
override the provider env vars before launching:

```bash
export MEM0_LLM_PROVIDER=openai
export MEM0_LLM_MODEL=gpt-4o-mini
export MEM0_EMBEDDER_PROVIDER=openai
export MEM0_EMBEDDER_MODEL=text-embedding-3-small
export MEM0_EMBEDDER_DIMS=1536
export OPENAI_API_KEY=...
```

## Run

The supervised runner is the recommended entry point — it is crash-proof
(checkpoints every item, resumes after a segfault / Docker drop / OOM) and
produces all three measurements:

```bash
bash run_all.sh        # basemyai 500 + mem0 infer=True (100) + mem0 infer=False (500)
```

Or drive `run.py` directly:

```bash
python run.py --target basemyai     --corpus corpus.jsonl --out out/basemyai.json
python run.py --target mem0_qdrant  --corpus corpus.jsonl --out out/mem0-qdrant.json --limit 100 --infer
python run.py --target mem0_qdrant  --corpus corpus.jsonl --out out/mem0-noinfer.json --limit 500 --no-infer
python summarize.py out/*.json > out/summary.md
```

Flags:

- `--limit N` — cap the corpus to the first N items (the `infer=True` latency
  curve is statistically stable well before 500; the tail dominates anyway).
- `--resume` — reload the checkpoint and skip already-done items. The supervisor
  sets this automatically on retry.
- `--infer` / `--no-infer` — Mem0 only. `--infer` (default) runs the LLM
  fact-extraction + reconciliation on every `add` (the real Mem0 experience).
  `--no-infer` stores raw, skipping all LLM calls — this isolates the cost of
  the **vector store** from the cost of the **LLM orchestration layer**, which
  is the whole point of the P1 claim.

## Gotchas (learned the hard way)

- **Use `127.0.0.1`, never `localhost`, for Qdrant on Windows + Docker Desktop.**
  `localhost` resolves to IPv6 `::1` first, fails, then falls back to IPv4 — a
  fixed per-connection penalty (hundreds of ms to *seconds*) paid on every one
  of Mem0's several Qdrant round-trips per `add`. This alone took an `infer=True`
  add from ~25–67 s down to ~4 s. `run.py` defaults to `127.0.0.1`.
- **Pin `numpy<2`.** numpy 2.x segfaults the mem0 + qdrant native stack
  (Python exit 139 / WinError) mid-run.
- **Ollama must keep both models resident.** The embedder (`all-minilm`) and the
  LLM (`llama3.2:1b`) alternate per `add`; with 8 GB VRAM both fit, but verify
  `curl localhost:11434/api/ps` shows them loaded so you do not pay cold-load
  (~6.7 s) repeatedly.

## Notes

Mem0's documented Qdrant configuration uses `Memory.from_config(config)` and
`m.search(query=..., user_id=...)`; see the Mem0/Qdrant docs and Qdrant's Mem0
integration page. This harness keeps that adapter isolated in `run.py` so the
benchmark can be updated if Mem0 changes its Python API.
