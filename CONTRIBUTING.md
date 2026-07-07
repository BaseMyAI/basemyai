# Contributing to BaseMyAI

Thanks for your interest in BaseMyAI — the local memory engine for AI agents.
This guide covers how to build, test, and propose changes.

## Code of Conduct

This project follows the [Contributor Covenant](CODE_OF_CONDUCT.md). By
participating, you agree to uphold it. Report unacceptable behavior to
**conduct@basemyai.com**.

## Ground rules

BaseMyAI is two crates in one workspace:

- **`basemyai-core`** — the business-agnostic foundation. It knows nothing about
  agent memory (`agent_id`, temporal validity, layers) or code (`Symbol`,
  `Edge`). **Mechanism lives in the core; meaning lives in the consumer.**
- **`basemyai`** — the memory semantics, built on top of the core.

A few invariants are non-negotiable and enforced by tests and CI:

- **`basemyai-core` stays agnostic.** No `agent_id`, `valid_from/until`, memory
  layer, or `Symbol/Edge` in it (ADR-001). A grep for those terms in
  `crates/basemyai-core/src` must return zero.
- **Per-agent isolation is a security invariant**, not a config option (ADR-006).
  Every read and write is filtered by `agent_id` at the SQL level.
- **All agent inputs are bound as SQL parameters**, never interpolated.
- **Encryption is mandatory in `basemyai`** (libSQL `crypto` feature).
- **The `Embedder` never downloads** and never detects hardware — it receives a
  resolved path and `Device` (ADR-010).

If a change requires bending one of these, it needs an **ADR** first (see below).

## Development setup

Requires a recent stable Rust toolchain (edition 2024, see
`rust-toolchain.toml`). The native vector path compiles without CMake; only the
`crypto` feature (encryption at rest) needs **CMake** installed.

```bash
# The quality gate — must pass before every commit. `cargo xtask` reproduces
# the exact CI matrix (.github/workflows/ci.yml): per-crate clippy/tests with
# the right feature combinations.
cargo xtask ci           # fmt --check + clippy + tests (the pre-commit gate)

cargo xtask check        # fmt --check + per-crate clippy (CI features, -D warnings)
cargo xtask test         # per-crate tests, light config (no embed/crypto)
cargo xtask test-embed   # CI `embed` job (Candle — heavy compile)
cargo xtask test-crypto  # CI `crypto` job (libSQL encryption — needs CMake)
```

Note: `cargo clippy --workspace --all-targets -- -D warnings` and
`cargo test --workspace` are useful locally but do **not** reproduce CI — CI
targets each crate with specific feature combinations (e.g.
`-p basemyai-mcp --no-default-features --features stdio,http,test-util`).
Use the `cargo xtask` targets above. Other useful commands:

```bash
cargo fmt --all                                         # formatting
cargo build -p basemyai-core --features embed           # Candle (heavy)
cargo build -p basemyai-core --features crypto          # encryption (needs CMake)
```

`crates/basemyai-engine/fuzz/` holds `cargo-fuzz` targets for the native
engine's decode paths (WAL/SST/key). Like `crypto`, it needs an extra
toolchain — **nightly**, not CMake — and is deliberately outside the
workspace and every `cargo xtask` command; see
`crates/basemyai-engine/fuzz/README.md` for how to run it (Linux/macOS/WSL
only — `cargo-fuzz`/libFuzzer doesn't link on native Windows).

The workspace lint policy (`[workspace.lints]` in the root `Cargo.toml`) encodes
the Rust rules below into the compiler. Your change must keep the clippy gate
green.

## Rust style (edition 2024, 2026)

- `thiserror` in libraries; `#[non_exhaustive]` on public error enums.
- **No `unwrap()` in library code** (tests are exempt via `clippy.toml`).
  `expect("message")` with a message is allowed.
- No `static mut`. No std `Mutex` held across an `.await`.
- Getters without a `get_` prefix. Prefer `&str` over `String` in parameters.
- `Arc::clone(&x)` over `x.clone()` on ref-counted values.

## ADRs — how decisions are made

Architecture decisions live under `docs/adr/` (one file per ADR), indexed by
`docs/ADR.md`. **An ADR is never edited**: a decision that changes is recorded
as a *new* ADR that supersedes the old one. If your contribution changes
architecture or touches an invariant above, open an issue proposing the ADR
first so the direction can be agreed before you write code.

## Licensing and sign-off (DCO)

The whole workspace — `basemyai-core`, `basemyai`, CLI, MCP, REST,
`basemyai-engine`, and the bindings — is licensed under the **Business
Source License 1.1** (see [LICENSE](LICENSE) and
[ADR-031](docs/adr/ADR-031-unified-busl-license.md)). To keep a clear chain of
title, every commit must include a
[Developer Certificate of Origin](https://developercertificate.org/)
sign-off, certifying you wrote the contribution or otherwise have the right
to submit it under the project's license:

```bash
git commit -s -m "feat(engine): ..."
```

PRs with unsigned commits will be asked to amend before merge.

## Pull requests

1. Fork and branch from `main` (or `dev`).
2. Keep PRs focused — one logical change per PR.
3. Make sure `cargo xtask ci` passes locally (it runs `cargo fmt --all --check`
   plus the per-crate clippy and test matrix that CI runs; `--workspace`
   commands are not equivalent).
4. Write a clear PR description: what changed, why, and which ADR/issue it
   relates to. Reference issues with `Fixes #123`.
5. Use [Conventional Commits](https://www.conventionalcommits.org/) for commit
   and PR titles (e.g. `feat(core): …`, `fix(cli): …`, `docs: …`).

CI runs clippy, tests, formatting, CodeQL, and supply-chain checks on every PR.
First-time contributor PRs are labeled and may wait for a maintainer to approve
workflow runs.

## Reporting bugs and requesting features

Use the [issue templates](https://github.com/basemyai/basemyai/issues/new/choose).
For **security vulnerabilities, do not open a public issue** — see
[SECURITY.md](SECURITY.md) (report privately to security@basemyai.com or via a
GitHub security advisory).

## Questions

Open a [Discussion](https://github.com/basemyai/basemyai/discussions) or join the
[Discord](https://discord.gg/basemyai).
