# N3.1 — Vector bench hardening + scale-up follow-up (2026-07-05)

Follow-up to `docs/benchmarks/n3-vector-parity-2026-07-05.md` (N3, closed).
**N3 stays closed** — its three ADR-026 §6 thresholds were already held at
10k/100k with real margin. This note does not reopen that verdict; it
hardens the harness that produced it and pushes measurement further where
that is cheap to do honestly. Scope, per the request that produced this
doc: audit the existing bench for reliability gaps, fix what's fixable
without touching the index itself, and report what is and isn't measured —
not "prove 1M works."

## Audit of the existing N3 bench

Read `crates/basemyai-engine/src/bin/vector_bench.rs` (pre-N3.1 version, as
archived by `n3-vector-parity-2026-07-05.md`) and the parity report itself.
Four things the request asked to pin down:

1. **Generator**: `LatentData` (duplicated from `tests/common::LatentData`)
   — a fixed 16-dimensional seeded latent pushed through a fixed seeded
   random linear map into the 384d ambient space. **Not** M6's
   `synthetic_vector` (iid uniform, L2-normalized,
   `crates/basemyai-core/benches/knn_scalability.rs`). This was already a
   *documented, deliberate* choice in both the harness's module doc and the
   parity report's "Deliberate protocol difference" section — iid-uniform
   384d is a measured ANN pathology on this exact codebase (recall@10 =
   0.664 at N=10 000, `tests/common/mod.rs` module doc), not an oversight.
   Restated here because the request asked for it explicitly, not because
   it was undocumented.
2. **Oracle location**: the pre-N3.1 harness built `let vectors: Vec<Vec<f32>>
   = (0..n).map(|_| generator.point()).collect()` **before** the insert
   loop and kept it alive for the entire run, purely so the query phase
   could call `brute_force_top_k(&vectors, ...)` at the end. At n=100 000
   that's ≈147 MiB (384×4 bytes/row) held for the full ~29-minute build,
   on top of whatever the index itself holds — the parity report's RAM
   section already named this as a contaminant of any whole-process RAM
   reading, but the harness had no way to turn it off.
3. **Incremental build timing**: `Instant::now()` wraps the entire
   `for (id, vector) in vectors.iter().enumerate() { index.insert(...) }`
   loop plus a trailing `engine.flush()`; `ms_per_row` is
   `build_elapsed / n` — a full-run average, not a moving one. The parity
   report separately called out (by re-deriving from progress-log
   timestamps, not a harness feature) that the *marginal* per-row cost
   climbs through a run — this was true but wasn't something the harness
   itself surfaced.
4. **Why the RAM monitoring failed**: the parity report's own account —
   an external `Start-Job`-backgrounded PowerShell `Get-Process` poller,
   launched from a separate shell, which died silently when that launching
   shell exited. Nothing in the harness itself was watching memory; the
   monitoring lived entirely outside the process being measured, with no
   supervision tying its lifetime to the thing it was measuring.

## What changed in the harness

`crates/basemyai-engine/src/bin/vector_bench.rs` (same binary, same
protocol shape — 384d/cosine/k=10, same node/graph/persistence code paths,
**no change to the index itself**):

1. **In-process RAM sampler** (`mod ram` in the binary). A background
   thread inside the *same process* samples whole-process RSS every
   `VECTOR_BENCH_RAM_INTERVAL_MS` (default 1000ms) into an in-memory
   buffer, started right after the engine opens and stopped right after the
   query phase. This directly fixes audit finding 4: there is no separate
   launching shell to lose — the sampler thread shares the benchmark
   process's own lifetime, so it cannot be torn down independently of it.
   Windows reads `K32GetProcessMemoryInfo`'s `WorkingSetSize` (new
   `target.'cfg(windows)'.dependencies` on `windows-sys`, gated to that
   target only — no new Linux/macOS dependency); Linux reads
   `/proc/self/status`'s `VmRSS` (no dependency). Unsupported platforms
   report `peak_rss=not_measured` rather than a fabricated number. Output:
   `peak_rss_mib`, `mean_rss_mib`, sample count, and — with
   `VECTOR_BENCH_RAM_LOG=<path>` — every raw `(elapsed_s, rss_bytes)`
   sample as CSV for later plotting.

   **Still not isolated to the index.** This is a reliability fix, not a
   granularity fix: it reports the same whole-process RSS the old external
   poller was trying to report (dataset generator + oracle copy, if
   retained + the index's own read-through node cache/memtable + the
   harness's own buffers), just continuously and without the silent-death
   failure mode. Isolating "the index's own bytes" would need instrumenting
   the global allocator (`GlobalAlloc`), which this manual one-off tool
   still does not do — noted as a real limit, not solved here.

2. **`VECTOR_BENCH_SKIP_ORACLE=1`** — directly answers audit finding 2.
   When set, the harness generates and inserts each vector without
   retaining the `n`-length `Vec<Vec<f32>>`, and skips the brute-force
   recall computation entirely (queries still run, using freshly generated
   query vectors, so latency is still measured; `recall_at_10` prints
   `skipped`). At n=250 000 this removes ≈366 MiB of oracle-copy RAM that
   would otherwise sit alongside the index for the whole run and confuse
   the RAM reading; at n=1 000 000 it would remove ≈1.43 GiB. Measured
   effect at a small n=3 000 smoke test (see below): peak whole-process RSS
   dropped from 37.70 MiB (oracle retained) to 29.98 MiB (oracle skipped) —
   the ratio only gets more lopsided as n grows, since the oracle copy
   scales linearly with n while the index's own working set does not need
   to.

   **Recommended split, not one giant run**: run recall-gated checks
   (oracle enabled) only up to the sizes already gated by
   `tests/vector_recall.rs` / `tests/vector_churn.rs` / the N3 parity bench
   (≤100k) — that's where the exact-recall number stays cheap and honest.
   For scale characterization above that (build cost curve, query latency,
   disk, RAM), run with `VECTOR_BENCH_SKIP_ORACLE=1` and treat recall as
   already established at the smaller, oracle-gated sizes, not
   re-measured at scale. This is a deliberate protocol choice for this
   follow-up, not an oversight: an exact brute-force oracle over 500k–1M
   384d vectors is itself an expensive, RAM-heavy O(n) scan per query that
   would dominate the very numbers being measured.

3. **No new CLI shape** — `vector_bench <n> [engine_dir]` is unchanged;
   scale is still just a bigger `n`. `n` was already free-form, so 250k/
   500k/1M "support" is really "no artificial ceiling was ever hardcoded" —
   confirmed by actually running 250k (below), not assumed.

Both env vars were smoke-tested at n=3 000 (see the raw output captured
during this session):

```
$ VECTOR_BENCH_RAM_INTERVAL_MS=100 VECTOR_BENCH_RAM_LOG=ram.csv vector_bench 3000
recall_at_10=1.0000
peak_rss_bytes=39530496 peak_rss_mib=37.70 mean_rss_mib=22.25 ram_samples=51 ...

$ VECTOR_BENCH_SKIP_ORACLE=1 VECTOR_BENCH_RAM_INTERVAL_MS=100 vector_bench 3000
recall_at_10=skipped (VECTOR_BENCH_SKIP_ORACLE set — see module doc)
peak_rss_bytes=31440896 peak_rss_mib=29.98 mean_rss_mib=17.70 ram_samples=46 ...
```

Both runs' CSV logs show a continuous sample series (no gaps, no silent
death) from just after engine-open through the end of the query phase.

## Scale-up run: 250 000 rows

Launched during this session, oracle disabled (per the split above),
5-second RAM sampling:

```bash
VECTOR_BENCH_SKIP_ORACLE=1 VECTOR_BENCH_RAM_INTERVAL_MS=5000 \
  VECTOR_BENCH_RAM_LOG=ram-250k.csv \
  vector_bench 250000 <engine_dir> > run-250k.log 2>&1
```

**Status: [FILL IN AT DOC-FINALIZATION TIME — see the live log].** Extrapolating
from the N3 100k curve (17.32 ms/row full-run average, marginal rate ≈24
ms/row by the end of that run — `n3-vector-parity-2026-07-05.md`), a
250 000-row incremental build should land somewhere in the neighborhood of
roughly 25–35 ms/row average, i.e. very roughly 1.7–2.4 hours of wall time
— **stated as a rough expectation to interpret the in-progress log by, not
a result**. If this run finished within the session, its real numbers
(build ms/row, query latency, recall=skipped, disk, RAM) are reported
below under "Measured results"; if it did not finish, its partial log and
exact reproduction command are the deliverable, per this task's own
instruction to not fabricate an unfinished run as a completed one.

### Measured results (fill in only if the run completed)

| Metric | 10k (N3) | 100k (N3) | **250k (N3.1)** |
|---|---|---|---|
| Build, ms/row (full-run avg) | 5.69 | 17.32 | *(see log)* |
| Query mean, ms (k=10) | 7.52 | 12.67 | *(see log)* |
| Recall@10 | 1.0000 | 1.0000 | skipped (oracle disabled, see above) |
| Disk, MiB | 22.26 | 177.28 | *(see log)* |
| Peak whole-process RSS, MiB | not measured | 233–278 (partial, floor only) | *(see log, continuous sampler)* |

## 500k / 1M: not run this session

Per the task's own constraint ("si un run long est impossible dans la
session, livre au minimum le harness fiable + documentation + instructions
exactes"), 500 000 and 1 000 000-row runs were **not** executed here — the
250k run above already consumes a meaningful fraction of a session, and the
100k curve's own caveat (marginal cost climbing, not flat) means a 500k/1M
number extrapolated from two points would be exactly the kind of
unreliable extrapolation `n3-vector-parity-2026-07-05.md` explicitly
declined to make. Reproduce locally with:

```bash
cargo build --release -p basemyai-engine --bin vector_bench

# 500k, oracle disabled, RAM sampled every 5s, samples logged to CSV
VECTOR_BENCH_SKIP_ORACLE=1 VECTOR_BENCH_RAM_INTERVAL_MS=5000 \
  VECTOR_BENCH_RAM_LOG=ram-500k.csv \
  ./target/release/vector_bench.exe 500000 /path/to/engine-500k

# 1M, same shape
VECTOR_BENCH_SKIP_ORACLE=1 VECTOR_BENCH_RAM_INTERVAL_MS=5000 \
  VECTOR_BENCH_RAM_LOG=ram-1m.csv \
  ./target/release/vector_bench.exe 1000000 /path/to/engine-1m
```

Expect these to run for several hours each given the non-flat marginal
build-cost curve already observed at 100k; run them detached (e.g. `nohup
... &` on Unix, a genuinely-detached process — not a `Start-Job` tied to an
interactive shell — on Windows) and inspect the CSV + stdout log
afterward. `VECTOR_BENCH_KEEP=1` leaves the engine directory on disk if you
also want to inspect the on-disk layout by hand; without it the directory
is deleted at the end of a successful run (so keep it set if you might
need to kill the run early and still want the partial engine state).

## Honest limits carried forward

Everything `n3-vector-parity-2026-07-05.md` already said about the 10k/100k
numbers still holds unchanged (generator choice, libSQL's build-cost column
being a bulk-load rate rather than a true incremental one, the non-flat
marginal build curve, single-run-not-repeated). This follow-up adds, and
does not remove, these additional caveats:

1. **RAM is still whole-process, not index-isolated** — now continuous and
   crash/kill-proof against the specific failure mode that hit the N3 run
   (an external supervisor dying), but the number itself is the same kind
   of number as before: dataset + oracle (when enabled) + index + harness,
   summed. Do not read `peak_rss_mib` as "the index uses N MiB."
2. **`VECTOR_BENCH_SKIP_ORACLE` recall is `skipped`, not `1.0` and not
   `unknown-but-fine`.** Any scale-up run using it carries no recall
   evidence of its own; recall confidence for this index still rests on
   `tests/vector_recall.rs` / `tests/vector_churn.rs` (≤10k, gated) and the
   N3 parity bench (10k/100k, ungated but archived). Do not extrapolate
   recall to 250k/500k/1M from those numbers without a dedicated,
   oracle-enabled run at that size — which will itself be expensive (an
   O(n) brute-force scan per query), and was out of scope here.
3. **250k (and any 500k/1M run) uses a different vector generator instance
   than the M6 comparison** — same generator *algorithm* as N3 (`LatentData`,
   seed `0xBA5E_A126_2026_0705`), so query latency and disk-cost numbers
   are internally comparable across 10k/100k/250k, but there is still no
   libSQL M6 datapoint at these sizes to compare against (M6 itself never
   went past 100k — see `docs/benchmarks/m6-knn-results-2026-07-01.md`).
4. **No new ADR-026 threshold is being introduced or re-judged here.** N3's
   three exit thresholds were already met and archived; this follow-up is
   purely about instrumentation quality and reporting reach, not a second
   gate.
