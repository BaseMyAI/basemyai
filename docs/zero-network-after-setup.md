# Zero Network After Setup

BaseMyAI's network rule is intentionally narrow:

- setup may fetch model files only with explicit consent;
- the embedder receives a local model path and never downloads;
- `remember`, `recall`, graph traversal, GC, and forgetting are local database
  operations;
- MCP sampling or user-configured LLM backends may use a network because the
  operator explicitly chose that integration.

## What Is Already Tested

The provisioning tests cover the most important invariant: when the model is
absent and consent is false, setup fails instead of silently downloading.

```bash
cargo test -p basemyai --features test-util provision_without_consent_fails_when_model_absent
```

## Manual Offline Proof

After a successful setup:

```bash
basemyai setup --fetch
```

run `remember` and `recall` with the machine disconnected or with an invalid
proxy:

```bash
export HTTPS_PROXY=http://127.0.0.1:9
export HTTP_PROXY=http://127.0.0.1:9
export BASEMYAI_DB_KEY='dev-proof-key'

basemyai init ./offline-proof.bmai
basemyai remember ./offline-proof.bmai --agent-id offline-agent --text "offline memory works"
basemyai recall ./offline-proof.bmai --agent-id offline-agent --query "offline memory"
```

Expected result: the memory roundtrip succeeds. If the model path is missing,
the command must fail with setup guidance rather than fetching anything
silently.

## CI Hardening Still To Add

A future CI job should pre-provision a tiny local test model or use the
`test-util` deterministic embedder, deny outbound sockets at the process level,
and execute a full `remember`/`recall` roundtrip.

That job should be named `zero-network-after-setup` and linked from the README
once it exists.
