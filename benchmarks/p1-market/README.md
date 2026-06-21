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

```bash
python run.py --target basemyai --corpus corpus.jsonl --out out/basemyai.json
python run.py --target mem0_qdrant --corpus corpus.jsonl --out out/mem0-qdrant.json
python summarize.py out/*.json > out/summary.md
```

## Notes

Mem0's documented Qdrant configuration uses `Memory.from_config(config)` and
`m.search(query=..., user_id=...)`; see the Mem0/Qdrant docs and Qdrant's Mem0
integration page. This harness keeps that adapter isolated in `run.py` so the
benchmark can be updated if Mem0 changes its Python API.
