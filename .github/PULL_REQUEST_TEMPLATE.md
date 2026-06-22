<!-- Thanks for contributing to BaseMyAI! Keep PRs focused: one logical change. -->

## What & why

<!-- What does this change and why? Link the issue/ADR it relates to. -->

Fixes #

## Type of change

- [ ] Bug fix
- [ ] New feature
- [ ] Refactor (no behavior change)
- [ ] Docs
- [ ] CI / build / tooling

## Checklist

- [ ] `cargo clippy --workspace --all-targets -- -D warnings` passes
- [ ] `cargo test --workspace` passes
- [ ] `cargo fmt --all --check` passes
- [ ] No `unwrap()` added to library code; no `Mutex` held across `.await`
- [ ] `basemyai-core` stays business-agnostic (no `agent_id`/`Symbol`/`Edge`)
- [ ] If this changes an architectural decision, a new ADR is added (ADRs are
      never edited)
- [ ] Docs / README / CHANGELOG updated if user-facing
