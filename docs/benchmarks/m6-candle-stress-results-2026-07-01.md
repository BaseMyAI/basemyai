# M6 Candle Stress — Full 1h Run Results (2026-07-01/02)

Archived raw output for the open M6 TODO line "Exécution stress 1h + mémoire".
Harness: `crates/basemyai-core/tests/candle_stress.rs` (see
`docs/benchmarks/m6-knn-and-candle-stress.md` for the harness description and
short-validation instructions).

## Environment

- OS: Windows 11 Famille
- CPU: 13th Gen Intel(R) Core(TM) i7-13620H
- RAM: 13.7 GB total (reported by `Win32_ComputerSystem.TotalPhysicalMemory`)
- Rust: `rustc 1.95.0 (59807616e 2026-04-14)`
- Cargo: `cargo 1.95.0 (f2d3ce0bd 2026-03-21)`
- Candle: `candle-core`/`candle-nn`/`candle-transformers` = `0.10` (workspace `Cargo.toml`)
- Model: local `all-MiniLM-L6-v2` at
  `C:\Users\Noluc\AppData\Local\basemyai\models\all-MiniLM-L6-v2`
  (`config.json`, `tokenizer.json`, `model.safetensors` present, verified before
  the run; no network access, no download).
- Memory monitoring: OS-level (no DHAT/Valgrind on Windows). PowerShell
  `Get-Process -Name candle_stress*` polled every ~30s for `WorkingSet64`
  (bytes), covering the full run.

## Exact command

```bash
export BASEMYAI_MODEL_DIR="C:/Users/Noluc/AppData/Local/basemyai/models/all-MiniLM-L6-v2"
export BASEMYAI_CANDLE_STRESS_SECS="3300"
export BASEMYAI_CANDLE_STRESS_BATCH="16"
cargo test -p basemyai-core --features embed --test candle_stress -- --ignored --nocapture
```

Note on duration: the task target was `BASEMYAI_CANDLE_STRESS_SECS=3600` (1h).
Several earlier attempts on this run (see "Attempt history" below) were killed
by the execution environment (background-task/session interruptions) before
completing a full 3600s loop, even though memory sampling itself showed no
leak signature in the data that did survive. To guarantee one clean, fully
captured run with a confirmed `test result: ok` and an uninterrupted memory
series, the archived run below used `BASEMYAI_CANDLE_STRESS_SECS=3300` (55
minutes of active `embed_batch` looping, plus compile/model-load/teardown
overhead — 3319s/~55.3min wall clock end to end). This is a continuous
single-process run close to but not exactly the nominal "1h" duration; re-running
with `3600` on a machine/session without background-task interruptions would
be a straightforward follow-up if an exact 1h number is required.

## Full raw test output (run 5 — the archived, completed run)

```
    Finished `test` profile [unoptimized + debuginfo] target(s) in 3.34s
     Running tests\candle_stress.rs (target\debug\deps\candle_stress-dd950b0b1b6c2e88.exe)

running 1 test
test candle_embed_batch_stress_keeps_baseline_contract ... candle stress: iterations=100, elapsed=1739.4198996s, batch=16
ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3307.40s
```

Wrapper timing: process start `2026-07-02T14:45:47Z`, `TEST_DONE exit_code=0
elapsed_total=3319s`, end `2026-07-02T15:41:06Z`.

Only one `iterations=100` checkpoint printed (the harness prints every 100
iterations): at ~17.4s/iteration (embed_batch of 16 texts, 384d, CPU), the run
completed roughly 190 iterations total before the configured 3300s duration
elapsed, so iteration 200 (~3480s) was never reached. This is expected given
the print cadence, not an anomaly.

## Memory sample series (WorkingSet64, ~30s cadence, full run)

102 samples collected from t=30s to t=3278s. Summary:

| Stat | Value |
|---|---|
| Samples | 102 |
| Min | 61.2 MB |
| Max | 193.1 MB |
| Mean | 88.6 MB |
| First sample (t=30s) | 123.3 MB |
| Last sample (t=3278s) | 79.3 MB |
| Avg of first 10 samples (t=30s–315s, incl. model load) | 128.5 MB |
| Avg of last 10 samples (t=2951s–3278s) | 79.9 MB |

Full series (`timestamp,pid,working_set_bytes,elapsed_sec`):

```csv
timestamp,pid,working_set_bytes,elapsed_sec
2026-07-02T14:46:19Z,27160,129265664,30
2026-07-02T14:46:51Z,27160,176930816,62
2026-07-02T14:47:23Z,27160,139509760,95
2026-07-02T14:47:55Z,27160,202510336,127
2026-07-02T14:48:26Z,27160,130326528,158
2026-07-02T14:48:58Z,27160,143106048,190
2026-07-02T14:49:29Z,27160,106205184,221
2026-07-02T14:50:00Z,27160,93016064,252
2026-07-02T14:50:32Z,27160,105459712,283
2026-07-02T14:51:03Z,27160,120950784,315
2026-07-02T14:51:34Z,27160,123449344,346
2026-07-02T14:52:05Z,27160,122486784,377
2026-07-02T14:52:36Z,27160,94781440,409
2026-07-02T14:53:07Z,27160,102457344,439
2026-07-02T14:53:39Z,27160,89464832,471
2026-07-02T14:54:10Z,27160,75313152,502
2026-07-02T14:54:42Z,27160,85356544,533
2026-07-02T14:55:13Z,27160,75358208,565
2026-07-02T14:55:45Z,27160,72744960,597
2026-07-02T14:56:17Z,27160,84836352,628
2026-07-02T14:56:49Z,27160,84688896,660
2026-07-02T14:57:20Z,27160,68984832,692
2026-07-02T14:57:53Z,27160,68993024,724
2026-07-02T14:58:24Z,27160,81604608,756
2026-07-02T14:58:56Z,27160,81629184,787
2026-07-02T14:59:28Z,27160,83091456,819
2026-07-02T15:00:00Z,27160,74149888,851
2026-07-02T15:00:31Z,27160,105705472,883
2026-07-02T15:01:02Z,27160,92073984,914
2026-07-02T15:01:33Z,27160,83910656,945
2026-07-02T15:02:05Z,27160,89649152,977
2026-07-02T15:02:36Z,27160,89124864,1008
2026-07-02T15:03:07Z,27160,98721792,1039
2026-07-02T15:03:38Z,27160,91365376,1071
2026-07-02T15:04:10Z,27160,84074496,1102
2026-07-02T15:04:42Z,27160,87756800,1133
2026-07-02T15:05:13Z,27160,85225472,1165
2026-07-02T15:05:45Z,27160,86212608,1197
2026-07-02T15:06:17Z,27160,81027072,1228
2026-07-02T15:06:48Z,27160,106106880,1260
2026-07-02T15:07:19Z,27160,88137728,1291
2026-07-02T15:07:50Z,27160,121184256,1322
2026-07-02T15:08:21Z,27160,94830592,1354
2026-07-02T15:08:53Z,27160,67272704,1385
2026-07-02T15:09:25Z,27160,71610368,1416
2026-07-02T15:09:56Z,27160,88051712,1448
2026-07-02T15:10:28Z,27160,70836224,1479
2026-07-02T15:10:59Z,27160,124198912,1511
2026-07-02T15:11:30Z,27160,90284032,1542
2026-07-02T15:12:02Z,27160,99545088,1574
2026-07-02T15:12:33Z,27160,84254720,1605
2026-07-02T15:13:05Z,27160,84250624,1636
2026-07-02T15:13:37Z,27160,81825792,1668
2026-07-02T15:14:08Z,27160,96358400,1700
2026-07-02T15:14:40Z,27160,81469440,1731
2026-07-02T15:15:12Z,27160,94679040,1764
2026-07-02T15:15:43Z,27160,84353024,1795
2026-07-02T15:16:15Z,27160,81203200,1827
2026-07-02T15:16:47Z,27160,74625024,1859
2026-07-02T15:17:19Z,27160,87609344,1891
2026-07-02T15:17:51Z,27160,89501696,1923
2026-07-02T15:18:23Z,27160,80986112,1954
2026-07-02T15:18:55Z,27160,79499264,1986
2026-07-02T15:19:27Z,27160,81178624,2018
2026-07-02T15:19:59Z,27160,84140032,2050
2026-07-02T15:20:30Z,27160,84766720,2082
2026-07-02T15:21:01Z,27160,102469632,2113
2026-07-02T15:21:32Z,27160,122105856,2144
2026-07-02T15:22:05Z,27160,83558400,2175
2026-07-02T15:22:36Z,27160,86855680,2208
2026-07-02T15:23:08Z,27160,84430848,2240
2026-07-02T15:23:39Z,27160,81670144,2271
2026-07-02T15:24:10Z,27160,83922944,2302
2026-07-02T15:24:41Z,27160,95997952,2334
2026-07-02T15:25:12Z,27160,97378304,2364
2026-07-02T15:25:44Z,27160,78094336,2395
2026-07-02T15:26:17Z,27160,67493888,2427
2026-07-02T15:27:13Z,27160,125501440,2462
2026-07-02T15:27:46Z,27160,90431488,2516
2026-07-02T15:28:32Z,27160,100134912,2552
2026-07-02T15:29:04Z,27160,107200512,2595
2026-07-02T15:29:37Z,27160,66863104,2628
2026-07-02T15:30:08Z,27160,108060672,2660
2026-07-02T15:30:42Z,27160,79921152,2692
2026-07-02T15:31:14Z,27160,108277760,2726
2026-07-02T15:31:46Z,27160,64204800,2757
2026-07-02T15:32:18Z,27160,83095552,2789
2026-07-02T15:32:51Z,27160,86503424,2822
2026-07-02T15:33:22Z,27160,128217088,2854
2026-07-02T15:33:55Z,27160,97173504,2886
2026-07-02T15:34:27Z,27160,79765504,2918
2026-07-02T15:35:01Z,27160,65241088,2951
2026-07-02T15:35:35Z,27160,82956288,2985
2026-07-02T15:36:07Z,27160,101277696,3019
2026-07-02T15:36:40Z,27160,67231744,3050
2026-07-02T15:37:12Z,27160,101044224,3084
2026-07-02T15:37:43Z,27160,73609216,3115
2026-07-02T15:38:15Z,27160,69918720,3147
2026-07-02T15:38:49Z,27160,64225280,3179
2026-07-02T15:39:24Z,27160,87519232,3213
2026-07-02T15:39:55Z,27160,106778624,3247
2026-07-02T15:40:34Z,27160,83107840,3278
```

### Plot-as-text (working set, MB, 30s buckets, ~every 3rd sample for readability)

```
t=30s    123 |###########################
t=190s   137 |##############################
t=471s    85 |###################
t=787s    78 |##################
t=1102s   80 |##################
t=1416s    68 |###############
t=1731s    78 |##################
t=2050s    80 |##################
t=2364s    93 |####################
t=2660s   103 |#######################
t=2951s    62 |##############
t=3278s    79 |##################
```

Reads as an early bump during model load / first embedding calls (~120–200 MB
in the first few minutes), then a settle to a ~65–110 MB steady-state band
that holds for the rest of the run with no monotonic climb.

## Verdict

**Stable — no leak observed.** `test result: ok` (exit 0), memory oscillates
in a bounded ~61–193 MB band for the full 3300s (55 min) of continuous
`embed_batch` calls with no upward trend (last-10-sample average 79.9 MB is
*lower* than first-10-sample average 128.5 MB, which itself includes the
one-time model-load spike). No DHAT/Valgrind fine-grained allocation tracking
was available on this Windows machine, so this is OS-level `WorkingSet64`
evidence only, not a full leak-detector proof — sufficient to clear this M6
gap per the task's own acceptance bar (OS-level monitoring), not a substitute
for Linux DHAT/Valgrind tooling if that level of rigor is later required.

## Attempt history (for transparency)

Four earlier attempts on this task were interrupted by the execution
environment before producing a complete, archivable result:

1. **Run 1** (`SECS=3600`): background task killed at ~3618s wall clock,
   right as the stress loop should have been finishing; stdout was lost to
   buffering before flush, but ~59.6 minutes of memory samples were captured
   (t=5s–3579s) showing the same bounded, non-growing pattern seen in run 5.
2. **Run 2**: superseded before completion while switching to a quieter
   logging/notification strategy.
3. **Run 3** (`SECS=3300`): completed successfully (`exit_code=0,
   elapsed_total=3320s`) but its output lived in an ephemeral temp directory
   that was wiped by the environment during a multi-hour idle gap before the
   results could be archived.
4. **Run 4** (`SECS=3300`, logs moved to the project's `target/` dir for
   durability): died mid-run (log cuts off after "running 1 test" with no
   result line, no panic; memory sampling shows a ~41-minute gap consistent
   with the environment being suspended, then the process was gone at
   ~2955/3300s).

None of runs 1–4 showed any error, panic, assertion failure, or growth
pattern in the data that did survive — the interruptions were environmental
(background-task/session lifecycle), not test failures. Run 5, above, is the
first attempt to run start-to-finish without interruption and is the
archived result of record for this TODO line.
