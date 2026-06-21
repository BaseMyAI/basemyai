# `basemyai` CLI Reference

`basemyai-cli` is the developer CLI for the BaseMyAI agent memory database. It
gives command-line access to the same engine consumed by the Rust crate,
Python/Node SDKs, and the REST/MCP sidecars: provisioning, `.bmai` container
lifecycle, the full memory lifecycle (`remember`/`recall`/`list`/`forget`/
`invalidate`/`purge`/`export`/`import`), the entity/relation graph, one-shot
maintenance tasks, and LLM-driven consolidation.

Binary name: `basemyai`. Crate: `crates/basemyai-cli`. Status: see
[`status.md` §6](status.md#6-cli-basemyai).

## Install / build

```bash
cargo build -p basemyai-cli --release
# binary at target/release/basemyai
```

Default features are `crypto` + `embed` (mirrors `basemyai-mcp`). Without
them the binary still parses arguments but every command that touches a
`.bmai` file fails with an explicit error — there is no silent degraded mode.

## Global flags

These are accepted before or after the subcommand, and each has an
environment-variable / config-file fallback so scripts don't have to repeat
them:

| Flag | Fallback (in order) | Notes |
|---|---|---|
| `--db <path>` | `BASEMYAI_DB_PATH` → `~/.basemyai/config.toml` (`db-path`) | Required by every command except `init` (which takes the path positionally — creating a container without saying where would be dangerous to default). |
| `--agent <id>` | `BASEMYAI_AGENT` → `~/.basemyai/config.toml` (`agent`) | Required by every command that touches memory/graph data. |
| `--format <text\|json>` | `BASEMYAI_FORMAT` | `json` makes every command print one machine-readable JSON object on stdout — built so an AI agent can call this CLI as a tool without parsing human text. |

Encryption is mandatory (ADR-007): every command that opens a `.bmai` file
requires the key via `BASEMYAI_DB_KEY`. There is no flag for the key and no
way to open a file in plaintext.

## Persistent config

```bash
basemyai config show
basemyai config set db-path ./agent.bmai
basemyai config set agent my-agent
basemyai config unset agent
```

Writes `~/.basemyai/config.toml` (`[cli]` section: `db_path`, `agent`).
Precedence is flag > environment variable > config file > explicit error —
the CLI never guesses a path or agent.

## Provisioning

```bash
basemyai setup --fetch   # detect hardware, fetch+verify the embedding model (explicit consent — ADR-010)
basemyai status          # detected hardware + provisioned model + file presence
basemyai llm detect      # local LLM backends + best model for this machine
basemyai llm suggest     # installable models for this hardware (e.g. `ollama pull <tag>`)
```

No command ever downloads a model without `--fetch` (or the equivalent
explicit consent in the SDKs). See
[zero-network-after-setup.md](zero-network-after-setup.md).

## Container lifecycle

```bash
basemyai init ./agent.bmai      # create an encrypted .bmai container (migrations + metadata)
basemyai inspect                # container metadata + memory count
basemyai verify                 # validate container: opens, expected format/engine/dim
basemyai migrate                # apply pending schema migrations (idempotent)
basemyai stats                  # per-layer valid-memory counts for the resolved agent
```

## Memory lifecycle

```bash
basemyai remember "The user is on the Pro plan." --layer semantic
basemyai remember --file facts.txt --layer episodic   # one line = one memory, batched embedding
basemyai remember --file - --layer episodic            # stdin

basemyai recall "current billing plan" -k 5
basemyai recall "current billing plan" --hybrid         # vector + BM25 fused via RRF
basemyai recall "current billing plan" --layer semantic # single-layer filter
basemyai recall "current billing plan" --graph          # KNN bounded to graph entities
# --hybrid, --layer and --graph are mutually exclusive

basemyai list --layer semantic --limit 20               # raw listing, no semantic search
basemyai list --include-invalid                          # include invalidated/expired rows

basemyai invalidate <id>     # soft-delete: valid_until = now
basemyai forget <id>         # physical delete (GDPR right to erasure)
basemyai purge --yes         # delete ALL data for the resolved agent (memory + graph) — irreversible

basemyai export --out backup.jsonl   # versioned JSONL export of the agent's memory; stdout if --out omitted
basemyai import --file backup.jsonl  # re-embeds and imports; idempotent (skips already-present rows)
basemyai import --file -              # stdin
```

`list`, `forget`, `invalidate`, `purge`, and the graph commands skip loading
the Candle embedder (they go through `basemyai::storage::MemoryStore`
directly) — they don't pay the model-load cost for operations that do no
embedding.

## Graph

```bash
basemyai graph add-entity shared-root secret "Agent A private graph node"
basemyai graph add-edge shared-root points_to shared-leaf --weight 1.0
basemyai graph traverse shared-root --depth 3
```

Entities/relations scoped to the resolved agent; `traverse` is a recursive
SQL CTE (cycle-safe, depth-bounded).

## Maintenance & consolidation

```bash
basemyai maintenance gc                                            # delete expired memories (valid_until passed)
basemyai maintenance forget-adaptive --capacity 5000 --half-life-secs 2592000
basemyai consolidate                                                 # episodes -> facts + graph, via the best local LLM detected
```

`consolidate` requires a local LLM backend (`basemyai llm detect` to
diagnose) — it is never a hard dependency of the rest of the CLI.

## Shell completions

```bash
basemyai completions bash > /etc/bash_completion.d/basemyai
basemyai completions zsh  > ~/.zfunc/_basemyai
basemyai completions fish > ~/.config/fish/completions/basemyai.fish
```

## Scripting example (JSON mode)

```bash
export BASEMYAI_DB_KEY='dev-key'
export BASEMYAI_FORMAT=json

basemyai init ./agent.bmai
basemyai --db ./agent.bmai --agent demo remember "The user prefers dark mode."
basemyai --db ./agent.bmai --agent demo recall "UI preference" --hybrid | jq '.results[0].text'
```

## What's not here yet

- No `gc --agent-id <id>` scoping (today `maintenance gc` runs across all
  agents in the container).
- No published binary release (`cargo-dist` or equivalent) — build from
  source today.
- No automated CLI integration tests in CI (`assert_cmd`/`trycmd`); coverage
  today is a manual end-to-end smoke test. See `docs/TODO.md` M5.
