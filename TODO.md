# TODO — BaseMyAI M6+ (as of 2026-06-21)

## Benchmarking (In Progress)

- [ ] **mem0+Qdrant 500-item run**: Retry with fixed Qdrant timeout (120s, was 5s) after client crash at ~2h04
  - Options:
    - (A) Full 500-item run (~2h+, progress saved now if it fails again)
    - (B) Shrink to 100 items (~30-40min, document asymmetry vs BaseMyAI n=500)
    - (C) Publish with BaseMyAI n=500 + mem0 n=10 (old), note the gap
  - **Decision pending**
- [ ] Update `docs/benchmarks/local-memory-vs-mem0-qdrant.md` once mem0 run completes (n=500 numbers, remove n=10 caveat)
- [ ] Regenerate `benchmarks/p1-market/out/summary.md` via `summarize.py`
- [ ] Commit benchmark data & updated docs

## CI & Release (Partially wired, not validated end-to-end)

- [ ] Add `basemyai-cli` to GitHub Actions matrix (currently missing from `ci.yml`)
- [ ] Add dedicated job for `p1_isolation_adversarial` test (ADR-018 adversarial isolation)
- [ ] Validate release workflows on a staging tag with real secrets:
  - `release.yml` (crates.io publish gate exists, unproven on a live tag)
  - `python-wheels.yml` (PyPI build/publish exists, unproven on a live tag)
  - `node-prebuilds.yml` (npm prebuild/publish exists, unproven on a live tag)
- [ ] Dry-run publish to staging before announcing

## Cleanup (Optional)

- [ ] Stop Qdrant container: `docker compose -f benchmarks/p1-market/docker-compose.qdrant.yml down`
- [ ] Unload Ollama models if not needed: `ollama pull llama3.2:1b && ollama pull all-minilm` (or `ollama rm`)

---

**Status**: Benchmark retry decision still needed; CI/release wiring exists but has not been validated by an end-to-end public release.
