# Recall Quality Lab

Status: R2.x wiring complete (workspace/xtask/CI/CLI — see "Required
integrations" below). The lab is deterministic, offline, model-free and
network-free. It exercises the native ephemeral store, `recall`,
`recall_hybrid`, graph-filtered recall and `compile_context`.

## Scope

The implementation lives in `crates/basemyai-eval`, a regular member of the
root Cargo workspace (outside `default-members`, like `xtask` — it activates
`basemyai/test-util` and is never pulled in by a bare `cargo build`/`test`).
`cargo xtask check`/`test`/`ci` cover it like any other crate. Its dataset
run is also surfaced as a non-blocking CI artifact job
(`.github/workflows/ci.yml` job `recall-quality-lab`), and as a thin
subcommand of the product CLI (`basemyai eval run|compare`, feature
`eval-lab`, off by default — see `docs/cli.md` §Recall Quality Lab).

The core dataset covers:

- direct relevance and exact IDs;
- expired facts and their current replacement;
- hostile imported content and strict provenance filtering;
- explicitly enabled procedures;
- exact deduplication with citation union;
- budgets of 512, 2,000, 8,000 and 32,000 estimated tokens;
- graph-filtered recall across linked entities;
- normalized repeat-run determinism.

No LLM, tokenizer download, model file or network endpoint is used.
`HashEmbedder` and the native ephemeral store are enabled through the existing
`test-util` feature.

## Dataset schema

`eval/datasets/recall-core.jsonl` contains one JSON object per line. Every case
has `schema_version: 1`, a stable case ID, suite, description, seed, query,
`k`, token budget, memories and expectations.

The seed deterministically namespaces the ephemeral agent and orders fixture
ingestion. No process-global RNG is consulted.

Each memory declares:

- stable fixture `id`, text, layer and provenance (`user`, `consolidation`,
  `import` or `unknown`);
- validity offsets relative to the start of the case;
- optional graded relevance in `0..=3`;
- stale, required-procedure and conflict-group annotations.

Top-level `must_include` and `must_exclude` apply to the compiled bundle.
Per-mode retrieval expectations live under `retrieval.vector`,
`retrieval.hybrid` and `retrieval.graph`. `expected_provenance` is checked
against observed records before bundle filtering.

The schema is strict: unknown fields, unsupported versions, duplicate IDs,
dangling expectations, invalid temporal windows, malformed graph references
and invalid bounds fail before any case executes.

## Commands

From the repository root (the crate is a regular workspace member now — `-p`
works like any other crate, no `--manifest-path` needed):

```powershell
cargo test -p basemyai-eval
cargo clippy -p basemyai-eval --all-targets -- -D warnings

cargo run -p basemyai-eval -- run `
  eval/datasets/recall-core.jsonl `
  --output eval/reports/recall-core.json `
  --human eval/reports/recall-core.md

# Equivalent shortcut, refreshes the canonical recall-core report/baseline:
cargo xtask eval-run

# Product CLI wrapper (feature eval-lab, off by default — see docs/cli.md):
cargo build -p basemyai-cli --features eval-lab
basemyai eval run eval/datasets/recall-core.jsonl --output report.json
```

Default reports omit wall-clock measurements and are byte-stable when the
engine output is stable. `--timings` adds `latency_micros` to recall modes and
bundle compilation; timing is therefore explicitly separated from quality.

Compare a baseline and a current report:

```powershell
cargo run --manifest-path crates/basemyai-eval/Cargo.toml -- compare `
  eval/reports/baseline.json `
  eval/reports/recall-core.json `
  --output eval/reports/comparison.json `
  --human eval/reports/comparison.md `
  --fail-on-regression
```

`run` exits 1 when a blocking assertion fails and 2 on dataset/runtime errors.
`compare --fail-on-regression` exits 1 when failed cases increase or a quality
metric moves in its adverse direction.

## Metrics

Retrieval reports Hit@K, Recall@K, Precision@K, MRR, exact-ID hit rate and nDCG
when graded relevance is present.

Bundle reports mandatory-item coverage, forbidden inclusion, hard budget
compliance, duplicate-token ratio, provenance coverage, stale-fact rate,
source-filter leakage, required-procedure coverage and unreported conflict
groups. Metrics are emitted per case and aggregated by retrieval mode.

The local adapter seeds the public `MemoryStore` contract with fixture IDs and
deterministic `HashEmbedder` vectors, then constructs the normal `Memory`
facade. Timings and Context Engine `compiled_at` are absent from deterministic
reports. Maps and normalized ID lists use stable ordering. Reports include a
content fingerprint, and comparison rejects different corpus contents even
when filenames match.

## Current limits

- Provenance seeding uses the lower-level public `MemoryStore` contract because
  high-level `Memory` writes do not accept arbitrary provenance. This adapter
  is eval-only and is not a product ingestion surface.
- Context Engine compilation consumes hybrid recall only. Graph quality is
  reported as a separate retrieval mode; graph candidates cannot yet be passed
  into the public context compiler.
- The bundle has no persisted supersession relation or conflict-warning
  contract. Conflict groups can be measured as unreported, but no warning can
  currently satisfy them.
- `HashEmbedder` validates deterministic engine behavior and exact/BM25 paths;
  it is not a semantic-quality benchmark. A model-backed suite must remain a
  separate, explicitly provisioned job.
- Isolation is enforced by each ephemeral `Memory`, but a shared-store
  cross-agent fixture is not part of this first autonomous dataset.

## Required integrations

Wiring status (R2.x):

1. ✅ `crates/basemyai-eval` is a root workspace member (outside
   `default-members`), sharing the root `[workspace.dependencies]` and
   `[lints]` tables.
2. ✅ `cargo xtask check`/`test`/`ci` run its clippy/tests; `cargo xtask
   eval-run` refreshes the canonical dataset report/baseline.
3. ✅ `basemyai eval run|compare` exists in the product CLI
   (`crates/basemyai-cli`, feature `eval-lab`, off by default) — a thin
   wrapper over the same `basemyai_eval::{run_dataset, compare_reports, ...}`
   runner, no duplicated policy. `docs/cli.md` §Recall Quality Lab.
4. ✅ A non-blocking CI artifact job (`recall-quality-lab` in
   `.github/workflows/ci.yml`) runs the `recall-core` dataset and uploads the
   report — **not** in `required-checks`. Budget, provenance, determinism and
   critical include/exclude assertions remain non-blocking until a reviewed
   baseline is committed and a follow-up change promotes this job to
   required — that promotion is a deliberate product decision, not done here.

Still deliberately out of scope (product-level evolutions, not wiring):

5. Expose a controlled high-level ingestion path for consolidation/unknown
   provenance before any non-eval consumer needs to write those sources.
6. Expose compilation from an explicit candidate set, or a graph-aware context
   request, before treating graph retrieval as bundle quality.
7. Add a first-class conflict/supersession signal before enabling conflict
   assertions as a gate.
