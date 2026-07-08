# M6 — KNN scalability results (2026-07-01/02)

Real, on-hardware Criterion output for `basemyai-core`'s native libSQL `vector_top_k`
KNN path, gathered per the rules in `m6-knn-and-candle-stress.md`. This closes the
`docs/archive/TODO-2026-06.md` M6 row "Résultats KNN 10k/100k/1M" — with 1M **documented, not executed**
(see "1M — not run this session" below).

## Machine specs

- CPU: 13th Gen Intel Core i7-13620H (10 cores / 16 logical processors)
- RAM: ~13.7 GiB
- OS: Windows 11 Home (`Windows_NT` 10.0.26200)
- Storage: SSD (external, "PHILIPS Portable SSD" on the `D:` volume the repo lives on;
  precise sustained-write throughput not determined — "not determined" beyond "SSD, external")
- Rust: 1.95.0 (59807616e, 2026-04-14); cargo 1.95.0 (f2d3ce0bd, 2026-03-21)
- `libsql` crate: 0.9.30 (from `Cargo.lock`)

## Command

```bash
BASEMYAI_KNN_BENCH_SIZES=10000 \
  cargo bench -p basemyai-core --bench knn_scalability
# and separately:
BASEMYAI_KNN_BENCH_SIZES=100000 \
  cargo bench -p basemyai-core --bench knn_scalability
```

Run as two separate single-size invocations rather than one combined
`10000,100000,1000000` sweep — see "Why 1M wasn't run" and "Operational notes" below for
why that mattered in practice on this machine.

## Harness change made for this run: bulk-load-then-index

The original harness (and `Store::ensure_vector_table`) creates the native
`libsql_vector_idx` (a DiskANN-style proximity graph) **before** inserting any rows, so
every insert incrementally maintains the graph. Empirically this does not scale: seeding
100,000 rows that way did not finish within 3+ hours of wall time (disk usage for the
temp DB climbed to ~9.9 GB — about 65x the ~150 MB of raw vector payload — before the run
was killed). Root cause, confirmed by reading `crates/basemyai-core/src/storage/store.rs`:
inserts were already batched correctly (1000 rows per transaction, WAL +
`synchronous=NORMAL`), so this is not a transaction/fsync bug — it's the per-row cost of
incrementally maintaining a proximity-graph index that itself grows with the graph size.

Fix (this run only, **opt-in**, not a change to `Store`'s default behavior): added
`Store::ensure_vector_table_no_index` (table only, no index) and
`Store::create_vector_index` (build the index separately) to
`crates/basemyai-core/src/storage/store.rs`, and changed
`crates/basemyai-core/benches/knn_scalability.rs`'s `seed_store` to bulk-insert all rows
first, then call `create_vector_index` once. `Store::ensure_vector_table` (the path
`Memory` and every other normal consumer uses) is **unchanged** — it still builds the
index up front, which is correct for the library's real incremental-insert usage pattern
where queries must be servable at any time, not just after a bulk load. The two new
methods are documented in-source as bulk-load/benchmark-only.

## Results

### 10,000 rows (post-fix: bulk-load-then-index)

```
knn_scalability/vector_knn_cosine_k10/10000
                        time:   [34.896 ms 48.976 ms 79.357 ms]
                        thrpt:  [126.01 Kelem/s 204.18 Kelem/s 286.57 Kelem/s]
Found 2 outliers among 10 measurements (20.00%)
  2 (20.00%) high severe
```

*(Criterion also printed a `change:` section against a locally cached baseline from an
earlier, unrelated smoke run in `target/criterion/` — not meaningful here and omitted.)*

**Caveat: no pre-fix 10k number exists to compare against.** An original run under the
old create-index-before-insert behavior did reach and start the 10k benchmark, but its
Criterion `time:`/`thrpt:` text was written to a log stream that was lost (the session's
temp/scratch directory was unexpectedly cleared before it could be read back, and a
separate attempt tailed the wrong output stream). Only the post-fix number above was
actually captured as text. Treat this as the only 10k number on record, not as one half of
a before/after comparison.

### 100,000 rows (post-fix: bulk-load-then-index)

```
knn_scalability/vector_knn_cosine_k10/100000
                        time:   [42.259 ms 48.545 ms 57.609 ms]
                        thrpt:  [1.7358 Melem/s 2.0600 Melem/s 2.3663 Melem/s]
Found 2 outliers among 10 measurements (20.00%)
  2 (20.00%) high severe
```

Query latency at 100k (mean 48.5 ms) is essentially the same as at 10k (mean 49.0 ms) —
consistent with `vector_top_k` being a genuine ANN index lookup (sub-linear in row count),
not a scan. The reported `thrpt` figures are Criterion's `rows / time` throughput metric,
not a claim about linear query cost.

### 1M rows — not run this session

Not executed. Extrapolated from the index-build-cost data below: at the observed
build rate this would need on the order of **~22 hours** of uninterrupted index-build
time alone, on top of seeding and querying. That is out of scope for a single working
session and was an explicit, deliberate decision (not an oversight) — see reasoning below.

## Index-build-cost finding (the real headline result)

This is a genuine libSQL native-vector-index scalability characteristic, relevant to
anyone planning a large-scale deployment on this backend — not just a benchmark
inconvenience.

Timestamps used below are either directly observed process/file timestamps, or derived by
subtracting Criterion's own self-reported phase durations (`Warming up for 3.0000 s`,
`Collecting samples in estimated N s`) from a log's final-write timestamp. Where an exact
instant wasn't directly observed, a confirmed bounding checkpoint is used instead of a
guess, and is called out as such.

| Rows | Index-build phase (`CREATE INDEX ... libsql_vector_idx`) duration | Method |
|---|---|---|
| 10,000 | **~13 min** (bounded: still building at check T+12m46s; already built and fully benchmarked by T+13m16s) | Two confirmed manual checkpoints during a live-monitored run (process start 05:54:48 → 06:07:38 "still building" → 06:08:08 "already built + benchmarked", 2026-07-02) |
| 100,000 | **~2h 12min** (≈131.8 min; derived: `err.log` final mtime `18:58:02.98` minus Criterion's reported `Warming up 3.0000s` + `Collecting ... 6.3421s` ≈ index-built at `18:57:53.6`; start bounded by a confirmed checkpoint at `16:46:04` showing seeding already complete and index build already under way, process actually started `16:45:13`) | `target/bench-logs/knn_100k.{out,err}.log` mtimes (`stat`) + a live checkpoint taken during the run, 2026-07-02 |

**Per-row cost is close to constant across this range**: 10k → ~78 ms/row, 100k →
~79 ms/row (both ≈ index-build-duration ÷ row-count). That's a meaningfully better shape
than the pre-fix incremental-insert behavior, where per-row cost appeared to *grow* with
already-indexed size (evidenced by the 65x-raw-data disk amplification observed before
100k even finished incrementally). Building the index once, after a bulk load, scales
roughly **linearly** with row count on this hardware — building it incrementally, one row
at a time into an already-large graph, does not.

**Why 1M wasn't run**: extrapolating the ~78–79 ms/row rate linearly to 1,000,000 rows
gives ≈ 79,000 s ≈ **~22 hours** of index-build time alone. If the true relationship is
even slightly superlinear beyond the 10k–100k range tested here (plausible for a
proximity-graph structure, not disproven by only two data points), it could be
meaningfully worse. Either way, this is not obtainable within a normal working session
without dedicating a machine to an unattended multi-hour-to-multi-day run, so it was
deliberately not attempted. Anyone relying on this backend for a from-scratch bulk load of
~1M+ vectors should plan for an index-build step on this order, run it out-of-band from
query serving, and re-verify the actual rate at their target scale rather than assuming
this extrapolation holds exactly.

## Rules followed (per `m6-knn-and-candle-stress.md`)

- Machine specs recorded above.
- Raw Criterion console output archived above (fenced blocks) and the underlying log
  files are retained locally at `target/bench-logs/knn_100k.{out,err}.log` (gitignored,
  not committed — `target/` is excluded by `.gitignore`).
- These numbers are **not** compared against the Mem0/Qdrant market benchmark
  (`local-memory-vs-mem0-qdrant.md`) — this bench measures internal KNN/index-build
  scalability only.

## Operational notes (why this took multiple attempts)

Recorded for anyone re-running this: launching the bench as a backgrounded process
through this session's shell-tool layer (`Bash`/`PowerShell` `run_in_background`) was
unreliable for a run long enough to include the 100k index build — three separate
launches via different mechanisms died silently partway through the index-build phase
(no panic, no crash-log entry, no non-zero exit text — consistent with the launching
tool's process/job-tracking tearing the child down rather than the benchmark itself
failing). The run that actually completed 100k was launched via PowerShell
`Start-Process -NoNewWindow -RedirectStandardOutput/-RedirectStandardError`, which detaches
the child from the launching call's lifetime; even that needed a couple of retries across
an unattended multi-hour gap before landing a clean run through to completion. If
automating this in CI or a longer-lived box, launch it as a true background service
(scheduled task / systemd unit / detached `nohup`) rather than through an interactive
tool-mediated shell.
