# Local Memory Benchmark: BaseMyAI vs Mem0 + Qdrant

Status: first real run recorded below (small corpus, single machine — not yet
a statistically robust public claim). Raw JSON in
[benchmarks/p1-market/out](../../benchmarks/p1-market/out/).

The benchmark lives in [benchmarks/p1-market](../../benchmarks/p1-market/README.md).
It compares:

- BaseMyAI as an in-process local encrypted memory database.
- Mem0 OSS configured with local Qdrant as the vector store.

The benchmark intentionally reports raw latency distributions instead of a
single headline number.

## Run record (2026-06-20)

| | |
|---|---|
| CPU | 13th Gen Intel Core i7-13620H (16 logical) |
| RAM | ~13.7 GiB available to the environment |
| OS | Windows 11, Git Bash shell |
| BaseMyAI commit | `4d8d091892dc1f29df2f03ebd1f298f4efe75949` (branch `dev`) |
| Mem0 package | `mem0ai==2.0.7` |
| Qdrant image | `qdrant/qdrant:latest` @ `sha256:75eab8c4ba42096724fdcfde8b4de0b5713d529dde32f285a1f86fdcb2c9e50c` |
| BaseMyAI embedder | local `sentence-transformers/all-MiniLM-L6-v2` via Candle (CPU), 384d |
| Mem0 LLM | local Ollama `llama3.2:1b` (fact extraction on every `.add()`) |
| Mem0 embedder | local Ollama `all-minilm`, 384d |
| Run type | cold (fresh `.bmai` file / fresh Qdrant collection each run) |
| Corpus | 10 items (`corpus.jsonl`) |

| Target | Operation | Count | Mean ms | p50 ms | p95 ms | p99 ms |
|---|---:|---:|---:|---:|---:|---:|
| basemyai | remember | 10 | 452.74 | 434.53 | 550.04 | 550.04 |
| basemyai | recall | 10 | 495.02 | 478.41 | 649.39 | 649.39 |
| basemyai | recall_hybrid | 10 | 425.97 | 401.40 | 581.33 | 581.33 |
| mem0_qdrant | remember | 10 | 12072.25 | 10018.82 | 19485.86 | 19485.86 |
| mem0_qdrant | recall | 10 | 6402.47 | 5450.44 | 8759.85 | 8759.85 |

### Reading these numbers honestly

- **Not apples-to-apples on `remember`.** Mem0's `.add()` runs an LLM call
  (fact extraction) on every insert by default — that's most of its 12s mean,
  not vector-store overhead. BaseMyAI's `remember` only embeds + writes; it
  does no LLM-based extraction at write time (that's a separate, explicit
  `consolidate()` step in BaseMyAI, not exercised here). The gap is real but
  is "embed+store vs. embed+LLM-extract+store", not "libSQL vs. Qdrant".
- **`recall` is closer to apples-to-apples** (embed query + vector search on
  both sides) and still shows BaseMyAI roughly 13x faster — consistent with
  in-process libSQL native vector search vs. Mem0's HTTP round-trip to a
  separate Qdrant container plus its own LLM-assisted result filtering.
- **n=10 is a smoke-sized corpus.** p95/p99 collapse to the same value because
  there are only 10 samples — these are *real* numbers from a real run, not a
  statistically powered benchmark. Don't quote p95/p99 publicly off this
  corpus; widen `corpus.jsonl` first.
- Mem0's own `.search()` does an additional LLM-assisted relevance step in
  some configurations; this run used the default (no `infer`/rerank
  override), matching what a typical out-of-the-box Mem0 deployment pays.

## Publish Criteria

Before publishing results as a public marketing claim (vs. this internal
record), additionally:

- Re-run on a corpus large enough for stable p95/p99 (≥500 items).
- Confirm the comparison framing above ships with the numbers — never the
  headline latency ratio alone.
- Re-confirm CPU/RAM/OS/disk type on the publishing machine if different from
  the run above.

## Commands

```bash
cd benchmarks/p1-market
docker compose -f docker-compose.qdrant.yml up -d
python run.py --target basemyai --corpus corpus.jsonl --out out/basemyai.json
python run.py --target mem0_qdrant --corpus corpus.jsonl --out out/mem0-qdrant.json
python summarize.py out/*.json > out/summary.md
```

No checked-in result should be edited by hand. Keep raw JSON with the summary.
