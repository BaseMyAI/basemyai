# N3 — Native vector index parity bench vs libSQL M6 (2026-07-05)

Closes the `docs/TODO-NATIVE-ENGINE.md` N3 row "Parité bench M6" — the last
open item of the LM-DiskANN vector index decided by
`docs/adr/ADR-026-native-vector-index-lm-diskann.md`. Judges
`crates/basemyai-engine`'s `PersistentVectorIndex` (Vamana/LM-DiskANN on the
Layer-1 KV store) against the reference measured in
`docs/benchmarks/m6-knn-results-2026-07-01.md` (libSQL's native
`vector_top_k`, also LM-DiskANN family) on the **same machine**, same
dimension, same k, same row-count checkpoints (10k / 100k).

## Harness

`crates/basemyai-engine/src/bin/vector_bench.rs` — a manual, non-Criterion
timed scenario (same shape as the M6 script), not wired into `cargo xtask`
(no CI job, like `crash_writer`'s dedicated job instead of living in the
default gate); it does compile clean under `cargo clippy -p basemyai-engine
--bins --all-targets -- -D warnings` (the `--bins` gate already in `xtask
check` covers it).

```bash
cargo build --release -p basemyai-engine --bin vector_bench
./target/release/vector_bench.exe <n>
```

Protocol, matched to M6 wherever the two backends allow a like-for-like
comparison:

- **Dimension**: 384 (`all-MiniLM-L6-v2`), same as M6.
- **Metric**: cosine, same as M6 (`vector_distance_cos` / this crate's
  `cosine_distance`).
- **k**: 10, same as M6.
- **Sizes**: 10 000 and 100 000, same checkpoints as M6.
- **Build path**: **incremental**, one `PersistentVectorIndex::insert` per
  row (the real `remember` path) — **not** bulk-load-then-index. This is
  stricter than M6's own harness, which had to add a bulk-load-then-index
  shortcut (`Store::ensure_vector_table_no_index` +
  `Store::create_vector_index`) because libSQL's incremental build never
  finished at 100k in 3+ hours. The native harness needed no such escape
  hatch — see the results below.
- **Queries**: 100 queries, k=10, mean/p50/p95 latency measured directly
  (M6 used Criterion's sampling; this harness prints its own since there is
  no Criterion dependency here).
- **Recall@10**: measured against an exact brute-force oracle over the same
  vectors and query set.
- **Index parameters** (ADR-026 defaults, `idx/vector/meta.rs`):
  `dim=384, max_degree(R)=32, beam_width(L)=128, alpha(α)=1.2`.
- **Engine tunables**: `EngineOptions::default()`
  (`memtable_flush_threshold=1000, compaction_sst_threshold=4`) — the same
  defaults any other consumer of `basemyai-engine` gets, not hand-tuned for
  this bench.
- **Seed**: `0xBA5E_A126_2026_0705` (vector generator), fixed across both
  sizes.

### Deliberate protocol difference: the vector generator

The harness does **not** reuse libSQL M6's `synthetic_vector` (iid uniform,
L2-normalized). iid-uniform 384d is a documented ANN pathology, not specific
to this implementation: pairwise cosine distances concentrate near 0 in high
dimension, leaving no neighborhood structure for *any* graph-ANN family to
navigate — measured on this exact codebase's own recall harness
(`tests/common/mod.rs`) at recall@10 = 0.664 at N=10 000 under iid uniform
384d, vs. 1.0000 under the low-intrinsic-dimension generator used here and in
the ADR-026 §6 recall gate (`tests/vector_recall.rs`). Real MiniLM sentence
embeddings have low intrinsic dimensionality (a semantic manifold inside the
384d ambient space), so `vector_bench.rs` reuses that same generator
(duplicated from `tests/common::LatentData` since `tests/` isn't visible to
a `src/bin` target): a 16-dimensional seeded latent pushed through a fixed
seeded random linear map into 384d. **This means the recall number here is
not comparable to a hypothetical "libSQL on this exact dataset" run** — M6
never measured libSQL's recall on any dataset (its bench measured latency
and build cost only, not recall) — but it *is* the same recall measurement
ADR-026 §6 gates the index on, which is what this parity bench needs to
confirm holds at the M6 checkpoint sizes too, not just at the smaller sizes
`tests/vector_recall.rs` runs by default.

### Machine (confirmed identical to M6)

- CPU: 13th Gen Intel Core i7-13620H (10 cores / 16 logical processors) —
  matches `m6-knn-results-2026-07-01.md`'s recorded CPU exactly.
- Rust: 1.95.0 (59807616e, 2026-04-14); cargo 1.95.0 (f2d3ce0bd,
  2026-03-21) — same toolchain versions as the M6 record.
- OS: Windows 11 (`Windows_NT` 10.0.26200).
- Storage: same machine as M6; M6 did not pin down sustained-write
  throughput beyond "SSD, external" and this run does not either.

## Results

### 10 000 rows

```
[vector_bench] n=10000 dim=384 k=10 queries=100 params: R=32 L=128 alpha=1.2
[vector_bench]   ...10000/10000 inserted (56.9077694s elapsed, 5.6908 ms/row)
=== vector_bench results: n=10000 ===
build_total=56.9077694s build_ms_per_row=5.6908
query_mean_ms=7.5204 query_p50_ms=7.5367 query_p95_ms=8.9694 (n_queries=100, k=10)
recall_at_10=1.0000
disk_bytes=23336368 disk_mib=22.26
```

### 100 000 rows

```
[vector_bench] n=100000 dim=384 k=10 queries=100 params: R=32 L=128 alpha=1.2
[vector_bench]   ...100000/100000 inserted (1732.4436403s elapsed, 17.3244 ms/row so far)
[vector_bench] querying: 100 x k=10...
=== vector_bench results: n=100000 ===
build_total=1732.4436403s build_ms_per_row=17.3244
query_mean_ms=12.6673 query_p50_ms=12.5524 query_p95_ms=13.7632 (n_queries=100, k=10)
recall_at_10=1.0000
disk_bytes=185886962 disk_mib=177.28
```

Build total wall time: 1732.4 s (≈28 min 52 s) for the full incremental,
one-row-at-a-time build (no bulk-load shortcut used or needed). Per-row
cost is **not flat** — it climbs steadily over the run: cumulative average
4.09 ms/row at row 7 330 (30 s in), 17.32 ms/row average by row 100 000; the
*marginal* rate near the very end (rows 97 976→100 000) is ≈24 ms/row, vs.
≈10 ms/row in the 30–60 s window. See "Limits" below for what this growth
curve does and doesn't mean set against libSQL's own "close to constant
per-row" characterization of its (bulk-load) build.

## Comparative table — native vs libSQL M6

| Metric | ADR-026 §6 threshold | libSQL M6 @10k | **Native @10k** | libSQL M6 @100k | **Native @100k** |
|---|---|---|---|---|---|
| Query latency, mean (k=10) | ≤ ~48–49 ms (parity ceiling) | 48.976 ms | **7.5204 ms** | 48.545 ms | **12.6673 ms** |
| Query latency, p95 | not gated | ~79.4 ms (Criterion max) | **8.9694 ms** | ~57.6 ms (Criterion max) | **13.7632 ms** |
| Build cost, incremental | < 78–79 ms/row | N/A — libSQL never completed a real incremental build; ~78 ms/row is the **bulk-load-then-index** rate, a strictly cheaper regime M6 had to fall back to | **5.6908 ms/row (real incremental)** | N/A — same caveat, and 100k never finished at all in 3h+ incrementally | **17.3244 ms/row (real incremental, full 100k)** |
| Recall@10 | ≥ 0.9 (incl. after churn) | not measured by M6 | **1.0000** | not measured by M6 | **1.0000** |
| Disk footprint | reported, not gated | not reported by M6 (temp `.db` file, size not recorded) | **22.26 MiB** (23 336 368 B) | not reported by M6 | **177.28 MiB** (185 886 962 B) |
| RAM footprint | reported, not gated | not applicable (M6 is a Criterion in-process bench, not isolated) | *see RAM section* | *see RAM section* | *see RAM section* |

## Verdict per ADR-026 §6 threshold

- **Query ≤ parity ceiling (~48–49 ms)**: **HELD at both sizes**, by a wide
  margin — 7.52 ms mean at 10k (≈6.5× under the 48.976 ms libSQL ceiling) and
  12.67 ms mean at 100k (≈3.8× under the 48.545 ms libSQL ceiling). Native
  latency roughly doubles from 10k→100k (a real, mild sub-linear-to-linear
  growth as the graph gets bigger) while libSQL's stays essentially flat
  across the same range — but native starts from such a large margin that it
  stays comfortably under the ceiling either way. p95 is likewise held at
  both sizes (8.97 ms and 13.76 ms vs. libSQL's own Criterion maxima of
  ~79.4 ms and ~57.6 ms).
- **Build < 78–79 ms/row, incremental included**: **HELD at both sizes** —
  5.69 ms/row at 10k, 17.32 ms/row at 100k (full-run average), and unlike
  libSQL both of these numbers *are* the real incremental path (no bulk-load
  shortcut used or needed; libSQL's own 100k incremental build never
  finished in 3+ hours). This is the single most important result of this
  bench: the native engine beats the threshold by ≈13.8× at 10k and ≈4.5× at
  100k while doing the strictly harder workload libSQL couldn't complete at
  all at the larger size. **Caveat, stated plainly**: per-row cost is *not*
  flat like libSQL's own bulk-load rate was (78 ms at 10k, 79 ms at 100k,
  "close to constant") — native's marginal per-row cost keeps climbing
  through the run (≈10 ms/row early in the 100k run, ≈24 ms/row by the end),
  so the *margin* under the threshold is shrinking as N grows even though
  the threshold itself is still held with room to spare at 100k. This
  growth is graph-size-driven (a Vamana graph search widens/costs more to
  navigate as it grows) and is exactly the kind of curve that would need
  watching before promising the same headroom at, say, 1M rows — not
  measured here, out of scope for this bench (see "Limits").
- **Recall@10 ≥ 0.9, incl. after churn**: **HELD at both sizes** (1.0000 at
  10k, 1.0000 at 100k); already established more thoroughly (including under
  insert/delete churn) by `tests/vector_recall.rs` / `tests/vector_churn.rs`
  at N=2 000/10 000 — this bench's numbers are consistent with, not a
  replacement for, that gate. Churn was not re-exercised at 100k by this
  bench (pure insert-only build, matching the M6 protocol shape); the churn
  gate at that scale remains whatever `tests/vector_churn.rs` covers.
- **Disk / RAM reported**: disk **HELD** at both sizes (22.26 MiB at 10k,
  177.28 MiB at 100k — see "Limits" for the scaling read). RAM: see below,
  reported with an honest caveat on measurement completeness.

## RAM footprint

**Intended approach** (stated in the harness's module doc before the runs):
continuous OS-level `WorkingSet64` polling of the `vector_bench` process via
a backgrounded PowerShell job, every 10s for the duration of the run — the
same method already used and documented for the M6 Candle stress run
(`docs/benchmarks/m6-candle-stress-results-2026-07-01.md`'s "Memory
monitoring" section).

**What actually happened**: the `Start-Job` background poller died silently
when its launching PowerShell process exited — the same class of failure the
M6 KNN doc's "Operational notes" section warns about for backgrounded
processes torn down by the launching tool rather than failing on their own.
This was **not caught until after the fact**, so there is no continuous
10s-cadence series for either run. What exists instead is a handful of
**manual, ad hoc snapshots** taken via one-off `Get-Process` calls during the
100k run (none during the 10k run, which finished in under a minute — too
fast to usefully sample this way):

| Elapsed | Rows inserted (approx.) | WorkingSet64 |
|---|---|---|
| early | ~0 (just after seeding) | 267.1 MiB |
| ~300s | ~27600 | 278.4 MiB |
| ~330-420s (offset unlogged) | ~30k-35k | 233.2 MiB |
| ~420-480s (offset unlogged) | ~35k-39k | 250.2 MiB |

**Honest read**: this is a partial, irregularly-spaced sample of the whole
process's working set (generator's full in-memory `Vec<Vec<f32>>` for all
100 000 vectors — 384×4 bytes × 100 000 ≈ 147 MiB by itself — plus the engine,
its memtable, and the index's bounded `CACHE_CAP`-limited block cache), not
an isolated allocator delta attributable to the index alone, and it does
**not** cover the run's actual peak (no sample was taken during or right
after the heaviest late-run inserts, nor during the final query phase). The
observed range (233–278 MiB) is best read as a rough floor on the true peak,
not the peak itself. No RAM sample exists for the 10k run at all. This is a
process gap in this session, not a claim that the index has no memory
profile worth measuring — re-running with a properly detached sampler (a
real scheduled task or a `nohup`-style unattended background service, per
the M6 doc's own recommendation for exactly this failure mode) is the
correct fix, left as follow-up rather than blocking this bench's other
(fully measured) results.

## Limits and honest caveats

1. **Vector generator differs from M6's** (see "Deliberate protocol
   difference" above) — a real, intentional choice, not an oversight; it
   trades exact apples-to-apples data for a dataset the ADR-026 recall gate
   itself is meaningful on. Query latency and build-cost comparisons are
   unaffected (both are shape/count-driven, not data-distribution-driven);
   recall is only comparable to this codebase's own other recall gates, not
   to a libSQL recall number (which M6 never measured).
2. **The libSQL "build cost" column being compared against is not libSQL's
   incremental cost.** M6 explicitly could not measure real incremental
   build at either size (100k never finished in 3+ hours); the 78–79 ms/row
   figure is libSQL's *bulk-load-then-index* rate, a fundamentally easier
   workload than what this bench measures for the native engine (true
   one-row-at-a-time incremental insert, the real `remember` path). The
   comparison is still valid as *the number ADR-026 §6 set as the threshold
   to beat*, but it understates how much harder the native engine's
   workload actually is relative to what it's being compared against.
3. **RAM measurement is incomplete, not just process-level.** The intended
   continuous 10s-cadence sampler died silently (background `Start-Job` torn
   down with its launching process — see the RAM section above); what is
   reported is a handful of ad hoc snapshots covering roughly the first half
   of the 100k run only, with **no** sample from the second half, the final
   query phase, or the 10k run at all. The reported 233–278 MiB range is a
   floor, not a peak, and even a "complete" run of this sampler would still
   only report whole-process working set, not an allocator-attributed
   figure isolating the index's own footprint from the bench harness's own
   `Vec<Vec<f32>>` copy of every vector (kept for the brute-force oracle,
   not something a real `remember` caller would hold).
4. **100k is a single run, not a statistical sample** (matching M6's own
   practice — M6 also ran each size once, not repeated).
5. **Storage medium**: same machine as M6 but M6's own doc does not pin down
   sustained write throughput beyond "SSD, external" — this run inherits
   that same imprecision, not a new one.
6. **Build cost is not flat, unlike libSQL's own reported build-rate
   shape.** libSQL's bulk-load-then-index rate was measured as "close to
   constant" per row (≈78 ms at 10k, ≈79 ms at 100k). The native engine's
   *incremental* per-row cost instead climbs continuously through a run —
   full-run average 5.69 ms/row at 10k vs. 17.32 ms/row at 100k, with the
   100k run's own marginal (not average) rate climbing from ≈10 ms/row early
   to ≈24 ms/row by the end. Both numbers still clear the ADR-026 threshold
   with real margin at the sizes actually tested, but the shape is different
   in kind, not just in magnitude — a graph-search-cost-grows-with-graph-size
   effect that this bench does not extrapolate past 100k (unlike M6, which
   explicitly extrapolated libSQL's flat rate to 1M and reported that
   extrapolation as labeled speculation; this doc does not repeat that
   exercise for the native engine, since a *non-flat* curve would make any
   such extrapolation considerably less trustworthy without more data
   points than two).
7. **Disk footprint scales sub-linearly with N here** (22.26 MiB at 10k →
   177.28 MiB at 100k, a ≈7.97× increase for a 10× row increase), consistent
   with per-vector-plus-neighbor-list cost (384×4 B vector + 32×8 B neighbor
   IDs ≈ 1.8 KiB/node, ≈171 MiB at 100k arithmetic) dominating at scale while
   small fixed overhead (WAL/SST framing) is proportionally larger at 10k.
   Not a claim about asymptotic behavior beyond 100k — not measured.
