# Local Memory Benchmark: BaseMyAI vs Mem0 + Qdrant

Status: authoritative run recorded 2026-06-21 (≥500-item corpus, single machine).
The earlier 2026-06-20 smoke run (n=10) is preserved at the bottom as a
**superseded record** — its `recall` numbers were a measurement artifact; see
[The 2026-06-20 correction](#the-2026-06-20-correction). Raw JSON in
[benchmarks/p1-market/out](../../benchmarks/p1-market/out/). Harness in
[benchmarks/p1-market](../../benchmarks/p1-market/README.md).

It compares:

- **BaseMyAI** as an in-process local encrypted memory database.
- **Mem0 OSS** configured with a local Qdrant container as its vector store,
  measured in **two modes**: `infer=True` (default — an LLM extracts facts on
  every write) and `infer=False` (raw store, no LLM).

The benchmark reports raw latency distributions, never a single headline number.

## TL;DR — the decisive finding

> **BaseMyAI is a memory database, not an orchestration layer over a vector DB.**
> The benchmark proves this by isolating the two cost centres.

On the write path (`remember` / `.add()`), at p50:

| | p50 write | vs BaseMyAI |
|---|---:|---:|
| BaseMyAI (embed + store) | **76 ms** | 1.0× |
| Mem0 `infer=False` (embed + store, no LLM) | **82 ms** | 1.08× |
| Mem0 `infer=True` (embed + **LLM extract** + store) | **4 714 ms** | **62×** |

Read it this way: **strip the LLM and Mem0+Qdrant writes are within ~8 % of
BaseMyAI.** The 62× gap is *not* "libSQL native vectors vs Qdrant" — the two
storage engines are comparable. The gap is the **synchronous LLM
fact-extraction Mem0 runs on every `.add()`**, which accounts for ~98 % of its
write latency (4 714 ms → 82 ms when `infer` is off). BaseMyAI does no LLM work
at write time; its extraction is a separate, explicit, *backgroundable*
`consolidate()` step (ADR-018), so the cost never lands on the hot write path.

## Run record (2026-06-21)

| | |
|---|---|
| CPU | 13th Gen Intel Core i7-13620H (16 logical) |
| GPU | NVIDIA GeForce RTX 4060 Laptop, 8 GiB (Ollama offloads all layers) |
| RAM | ~13.7 GiB available to the environment |
| OS | Windows 11, Git Bash shell, Docker Desktop (linux engine) |
| BaseMyAI commit | `f437f348d474a45c033a50ff23ed671cc46f7c6c` (branch `dev`) |
| Mem0 package | `mem0ai==2.0.7` |
| Qdrant image | `qdrant/qdrant:latest` @ `sha256:75eab8c4ba42096724fdcfde8b4de0b5713d529dde32f285a1f86fdcb2c9e50c` |
| numpy | `1.26.4` (numpy 2.x segfaults the native stack — see methodology) |
| BaseMyAI embedder | local `sentence-transformers/all-MiniLM-L6-v2` via Candle, 384d |
| Mem0 LLM | local Ollama `llama3.2:1b` (fact extraction, `infer=True` only) |
| Mem0 embedder | local Ollama `all-minilm`, 384d |
| Qdrant transport | `127.0.0.1` REST (not `localhost` — see methodology) |
| Run type | cold (fresh `.bmai` / fresh Qdrant collection per target) |
| `k` (recall) | 5 |
| Corpus | 500 items (`infer=True` capped to 100 — the latency tail is stable well below 500) |

| Target | Operation | Count | Mean ms | p50 ms | p95 ms | p99 ms |
|---|---:|---:|---:|---:|---:|---:|
| basemyai | remember | 500 | 95.28 | 75.91 | 188.94 | 321.76 |
| basemyai | recall | 500 | 157.35 | 168.08 | 200.60 | 235.65 |
| basemyai | recall_hybrid | 500 | 163.36 | 175.00 | 211.36 | 249.83 |
| mem0_qdrant (`infer=True`) | remember | 100 | 4152.50 | 4713.52 | 5813.57 | 6392.14 |
| mem0_qdrant (`infer=True`) | recall | 100 | 123.38 | 112.54 | 187.28 | 244.70 |
| mem0_qdrant (`infer=False`) | remember | 500 | 82.94 | 82.09 | 106.91 | 118.67 |
| mem0_qdrant (`infer=False`) | recall | 500 | 92.58 | 91.43 | 121.01 | 137.01 |

BaseMyAI output DB after 500 inserts: **42.2 MB** (encrypted at rest).

## Reading these numbers honestly

- **`remember` is not apples-to-apples by *function*, but is apples-to-apples by
  *API call*.** A developer who wants to save a memory calls `remember()` /
  `.add()`. BaseMyAI returns in 76 ms; default Mem0 blocks for 4.7 s because it
  runs an LLM inline. The `infer=False` row proves the difference is the LLM, not
  the store. Quote the 62× as *"write-call latency, default configuration"*, and
  always ship the `infer=False` row next to it so the reason is visible.
- **`recall` does *not* favour BaseMyAI — be upfront about this.** BaseMyAI
  recall p50 is **168 ms** vs Mem0 **113 ms** (`infer=True`) / **91 ms**
  (`infer=False`). On reads, Mem0+Qdrant is *faster*. BaseMyAI's recall embeds
  the query with Candle and runs KNN every call; that embed step dominates its
  read latency. This is a real, honest finding — the BaseMyAI story is about
  **write-path simplicity and no LLM tax**, not about beating Qdrant on read
  throughput.
- **`recall_hybrid` (175 ms) is BaseMyAI-only.** Mem0's public API does not
  expose an equivalent RRF/BM25 contract, so there is no fair Mem0 column.
- **Latency growth.** Mem0 `infer=True` write latency grows as the collection
  fills (reconciliation prompt grows): first-quartile mean 3 686 ms →
  last-quartile mean 4 309 ms (**×1.17** over 100 items). `infer=False` is flat
  (×0.98) and BaseMyAI is near-flat (×1.24 over 500, dominated by embed jitter,
  not collection size). Expect the `infer=True` ratio to keep climbing past 100.
- **Single machine, single run.** Real numbers from a real run — not a
  multi-trial, multi-machine study. See Publish Criteria before quoting publicly.

## Methodology & reproducibility

The harness ([run_all.sh](../../benchmarks/p1-market/run_all.sh)) is
crash-proof: it checkpoints every item and resumes after a crash, so a segfault
or a Docker/Qdrant drop costs one item, never the whole run. Three lessons were
baked into it after they cost hours:

1. **Qdrant over `127.0.0.1`, never `localhost`.** On Windows + Docker Desktop,
   `localhost` resolves to IPv6 `::1` first, fails, then falls back to IPv4 — a
   fixed per-connection penalty (hundreds of ms to *seconds*) paid on every one
   of Mem0's several Qdrant round-trips per `.add()`. Fixing this alone took an
   `infer=True` add from ~25–67 s down to ~4 s. **This is the single biggest
   methodology error to avoid, and the cause of the superseded 2026-06-20
   recall numbers.**
2. **Pin `numpy<2`.** numpy 2.x segfaults the mem0 + qdrant native stack
   (Python exit 139 / `WinError`) part-way through a run.
3. **Keep both Ollama models resident.** The embedder (`all-minilm`) and LLM
   (`llama3.2:1b`) alternate per add; with 8 GB VRAM both fit. Verify with
   `curl 127.0.0.1:11434/api/ps` so cold-load (~6.7 s) is not paid repeatedly.

The `infer=False` mode is the methodological centrepiece: it is the control that
separates *storage cost* from *LLM-orchestration cost*. Without it the 62× write
gap is ambiguous ("is Qdrant slow, or is it the LLM?"); with it the answer is
unambiguous.

## The 2026-06-20 correction

The first smoke run (n=10, table below) reported BaseMyAI `recall` as ~13×
faster than Mem0 (478 ms vs 6 402 ms). **That conclusion was wrong.** That run
talked to Qdrant over `localhost`, so every Mem0 read paid the IPv6-fallback
penalty described above. With `127.0.0.1`, Mem0 `recall` is **113 ms** — i.e.
*faster* than BaseMyAI, not 13× slower. The storage engines were never that far
apart on reads; the old number measured a Windows networking artifact, not
Qdrant. Treat any pre-2026-06-21 Mem0 latency from this harness as suspect.

<details>
<summary>Superseded run record (2026-06-20, n=10) — artifact, do not quote</summary>

| Target | Operation | Count | Mean ms | p50 ms | p95 ms | p99 ms |
|---|---:|---:|---:|---:|---:|---:|
| basemyai | remember | 10 | 452.74 | 434.53 | 550.04 | 550.04 |
| basemyai | recall | 10 | 495.02 | 478.41 | 649.39 | 649.39 |
| basemyai | recall_hybrid | 10 | 425.97 | 401.40 | 581.33 | 581.33 |
| mem0_qdrant | remember | 10 | 12072.25 | 10018.82 | 19485.86 | 19485.86 |
| mem0_qdrant | recall | 10 | 6402.47 | 5450.44 | 8759.85 | 8759.85 |

n=10 (p95/p99 collapse to a single sample), `localhost` transport, CPU-only
embedder note. Both the inflated Mem0 `recall` and part of the `remember` cost
are now attributed to the `localhost`/IPv6 penalty.
</details>

## Publish criteria

Before quoting these as a public marketing claim (vs. this internal record):

- [x] Corpus ≥500 for stable p95/p99 (met for BaseMyAI and Mem0 `infer=False`;
      Mem0 `infer=True` at n=100 — adequate for the tail, widen if quoting p99).
- [x] Comparison framing ships with the numbers (the `infer=False` control and
      the honest `recall` caveat must travel with any headline ratio).
- [ ] Re-run on the actual publishing machine if its CPU/GPU/RAM/disk differ.
- [ ] At least one independent re-run on a second machine (Linux native, to rule
      out remaining Docker-Desktop-on-Windows transport effects).

Never quote the 62× write ratio without the `infer=False` row, and never imply
BaseMyAI wins on read latency — it does not.

## Commands

```bash
cd benchmarks/p1-market
docker compose -f docker-compose.qdrant.yml up -d
bash run_all.sh            # crash-proof; runs all three targets + summary
# or drive run.py directly — see the harness README for flags
```

No checked-in result should be edited by hand. Keep raw JSON with the summary.
