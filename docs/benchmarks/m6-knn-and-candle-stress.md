# M6 KNN Scalability and Candle Stress

**Status**: harnesses present; full-scale numbers must be generated on target
hardware before any public scale claim.

This page covers the M6 proof gap from `docs/TODO.md`: native libSQL KNN at
larger cardinalities and long-running Candle embedding stability. The harnesses
are intentionally opt-in because they can allocate large databases and run for a
long time.

## KNN Scalability

Default smoke run:

```bash
cargo bench -p basemyai-core --bench knn_scalability
```

Full M6 run:

```bash
BASEMYAI_KNN_BENCH_SIZES=10000,100000,1000000 \
  cargo bench -p basemyai-core --bench knn_scalability
```

The benchmark seeds a temporary libSQL database with deterministic 384d vectors,
then measures `Store::vector_knn(..., k=10)` through the real native
`vector_top_k` path. Seeding is outside the measured Criterion loop.

Rules before publishing numbers:

- Record CPU, RAM, storage type, OS, Rust version and libSQL version.
- Commit or archive Criterion output/raw summaries alongside the claim.
- Do not compare these numbers with the Mem0/Qdrant market benchmark; this bench
  measures internal KNN scalability only.

## Candle Stress

Short local validation:

```bash
BASEMYAI_MODEL_DIR=/path/to/all-MiniLM-L6-v2 \
BASEMYAI_CANDLE_STRESS_SECS=60 \
  cargo test -p basemyai-core --features embed --test candle_stress -- --ignored --nocapture
```

Full M6 stress run:

```bash
BASEMYAI_MODEL_DIR=/path/to/all-MiniLM-L6-v2 \
BASEMYAI_CANDLE_STRESS_SECS=3600 \
BASEMYAI_CANDLE_STRESS_BATCH=16 \
  cargo test -p basemyai-core --features embed --test candle_stress -- --ignored --nocapture
```

The test loads the local `CandleEmbedder`, repeatedly runs `embed_batch`, and
checks the baseline contract on every loop: model id, dimension, row count and
L2-normalized vectors. It never downloads a model.

For leak evidence, run the full test under an OS-level memory monitor or Linux
tooling such as DHAT/Valgrind and keep the raw output with the benchmark record.
