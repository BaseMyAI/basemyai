# `basemyai` CLI Reference

`basemyai-cli` is the developer CLI for the BaseMyAI agent memory database. It
gives command-line access to the same engine consumed by the Rust crate,
Python/Node SDKs, and the REST/MCP sidecars: provisioning, `.bmai` container
lifecycle, the full memory lifecycle (`remember`/`recall`/`list`/`forget`/
`invalidate`/`purge`/`export`/`import`), the entity/relation graph, and
LLM-driven consolidation (`consolidate`).

Binary name: `basemyai`. Crate: `crates/basemyai-cli`. Status: see
[`status.md` §6](status.md#6-cli-basemyai).

## Install / build

```bash
cargo build -p basemyai-cli --release
# binary at target/release/basemyai
```

Default feature is `embed` (Candle for `remember`/`recall`). Without it the
binary still parses arguments but every command that needs the full memory
stack fails with an explicit error — there is no silent degraded mode.

## Global flags

These are accepted before or after the subcommand, and each has an
environment-variable / config-file fallback so scripts don't have to repeat
them:

| Flag | Fallback (in order) | Notes |
|---|---|---|
| `--db <path>` | `BASEMYAI_DB_PATH` → `~/.basemyai/config.toml` (`db-path`) | Required by every command except `init` (which takes the path positionally — creating a container without saying where would be dangerous to default). |
| `--agent <id>` | `BASEMYAI_AGENT` → `~/.basemyai/config.toml` (`agent`) | Required by every command that touches memory/graph data. |
| `--format <text\|json>` | `BASEMYAI_FORMAT` | `json` makes every command print one machine-readable JSON object on stdout — built so an AI agent can call this CLI as a tool without parsing human text. |
| `--color <auto\|always\|never>` | `NO_COLOR` / `FORCE_COLOR` (when `auto`) | Controls ANSI styling in text mode. `never` is recommended for deterministic snapshots. |
| `--quiet` | — | Suppresses non-essential informational text in `text` mode (errors still print). |
| `--no-progress` | — | Disables spinners/progress bars for long operations. |
| `-v`, `-vv` | — | Enables diagnostic logs on stderr (`info`/`debug`). |

Encryption is mandatory (ADR-007/ADR-030). Every command that opens a `.bmai`
file resolves the user passphrase via **ADR-034** (see
[`docs/security/key-resolution.md`](security/key-resolution.md)):

1. `BASEMYAI_DB_KEY` (canonical)
2. `BASEMYAI_ENCRYPTION_KEY` (legacy alias)
3. `BASEMYAI_DB_KEY_FILE`
4. `/run/secrets/basemyai_db_key`
5. `~/.basemyai/key` (from `basemyai config key generate`)

There is no CLI flag for the key and no way to open a file in plaintext. The
passphrase is **never** stored in `config.toml`.

In `text` mode, `basemyai` now uses a terminal-aware presentation layer:
tables for scanability, semantic color tokens, and progress feedback for long
operations. Machine-readable contracts stay unchanged: in `json` mode, stdout
remains clean JSON (no ANSI, no spinner output), while progress/errors stay on
stderr.

## Exit codes & error shape

Stable, additive-only contract — a script can branch on these without parsing
free-text messages. Defined in `crates/basemyai-cli/src/exit.rs` /
`src/error.rs`; values never get reassigned, only added to.

| Exit | Meaning |
|---|---|
| 0 | Success. |
| 1 | Generic/uncategorized error (storage, embedding, IO...). |
| 2 | Invalid flag combination (e.g. `recall --hybrid --layer --graph` together), unknown `config` key. |
| 3 | Encryption key missing or insecure file permissions (ADR-034). |
| 4 | `--db`/`--agent` not resolvable (no flag, no env var, no config entry). |
| 5 | Invalid input at the business level (empty agent id, text too long...). |
| 6 | Target already exists (`init` on an existing path). |
| 7 | Destructive action refused without explicit confirmation (`purge` without `--yes`). |
| 8 | Embedding model not provisioned — run `basemyai setup --fetch`. |
| 9 | No local LLM backend detected — run `basemyai llm detect`. |
| 10 | `verify`: container opens but doesn't match the expected `.bmai` format/version. |

In `--format json`, every error is also printed on stderr as a single object
with a stable `code` string (the same categories as the table above, e.g.
`KEY_REQUIRED`, `KEY_INSECURE`, `NOT_CONFIGURED`, `INVALID_AGENT`, `ALREADY_EXISTS`,
`CONFIRMATION_REQUIRED`, `MODEL_NOT_PROVISIONED`, `LLM_NOT_AVAILABLE`,
`VERIFICATION_FAILED`) and a human `message` that **is not** part of the
contract and may reword across releases:

```json
{"error":{"code":"KEY_REQUIRED","message":"encryption key required: …"}}
```

In `--format text` (default), errors print as `error: <message>` on stderr.

Caveat: a *wrong* key (vs. an absent one) on an already-encrypted container
isn't always distinguishable from generic storage corruption at this layer —
the native engine surfaces it as a late, generic error on first access rather
than a dedicated one. Only the "env var entirely unset" case reliably gets
`KEY_REQUIRED`/exit 3; a wrong key will usually fall through to the generic
exit code 1.

## Persistent config

```bash
basemyai config show
basemyai config set db-path ./agent.bmai
basemyai config set agent my-agent
basemyai config unset agent
```

Writes `~/.basemyai/config.toml` (`[cli]` section: `db_path`, `agent` only —
**never** the encryption passphrase).
Precedence is flag > environment variable > config file > explicit error —
the CLI never guesses a path or agent.

### Encryption key (ADR-034)

```bash
basemyai config key generate          # create ~/.basemyai/key (value never printed)
basemyai config key generate --force  # replace an existing key file
basemyai config key path              # show default key file path
basemyai config key check             # verify a passphrase source is available
```

Back up `~/.basemyai/key` securely — losing it means **permanent** loss of
access to encrypted `.bmai` containers. On Unix, permissions must be
`chmod 700 ~/.basemyai` and `chmod 600 ~/.basemyai/key`.

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
basemyai init ./agent.bmai      # create an encrypted native .bmai container (metadata)
basemyai inspect                # container metadata + memory count
basemyai verify                 # validate container: opens, expected format/engine/dim
basemyai migrate                # idempotent open (native format applied at open time)
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

Entities/relations scoped to the resolved agent; `traverse` is a depth-bounded
BFS on the native graph index (cycle-safe).

## Consolidation

```bash
basemyai consolidate   # episodes -> facts + graph, via the best local LLM detected
```

`consolidate` is a **root command** (not under `maintenance`). It requires a
local LLM backend (`basemyai llm detect` to diagnose) — it is never a hard
dependency of the rest of the CLI.

## Maintenance: adaptive forgetting and expired-memory GC

```bash
basemyai forget-adaptive --capacity 10000                       # evict least-retained active memories beyond capacity
basemyai forget-adaptive --capacity 10000 --half-life-secs 604800
basemyai forget-adaptive --capacity 10000 --dry-run              # report only, evict nothing

basemyai gc                                                      # delete every memory with valid_until <= now
basemyai gc --page-size 500                                      # bound the scan/delete batch size
basemyai gc --dry-run                                             # report only, delete nothing
```

Removed in ADR-033 (native-only) as `maintenance gc` / `maintenance
forget-adaptive` (they depended on libSQL-specific SQL windowing), both were
**reintroduced as flat root commands** — `forget-adaptive` by ADR-037, `gc` by
ADR-038 — on top of an applicative scan instead of a windowed/`DELETE` SQL
query. They implement two disjoint mechanisms by construction:

- `forget-adaptive` bounds the **active** population of an agent by capacity,
  evicting the lowest-retention-score memories first
  (`score = importance + half_life / (half_life + age)`, hyperbolic decay).
  Invalidated/expired memories are never counted and never touched by this
  command — see `gc` for those.
- `gc` deletes memories whose `valid_until <= now` (invalidated explicitly, or
  expired by their validity window) and **only** those — active memories are
  never touched by this command, no matter how many there are or how
  unimportant.

Both commands go through `open_engine` (raw store), exactly like
`list`/`forget`/`invalidate`/`purge` — **no Candle embedder is loaded**, so
neither needs a provisioned model. Both support `--dry-run` (compute and
report what would happen, mutate nothing) and print a structured JSON report
under `--format json` (`scanned`/`evicted`/`capacity` for `forget-adaptive`;
`examined`/`deleted`/`pages`/`page_size` for `gc`, plus `dry_run` on both).
`gc --page-size 0` is rejected explicitly (`VALIDATION_ERROR`, exit 5) rather
than silently reporting "nothing to do".

The same policies run as background tasks (`AdaptiveForgettingTask`,
`ExpiredMemoryGcTask`) via `basemyai::MaintenanceWorker` for surfaces that
keep a worker running continuously (the CLI itself is one-shot, no
background worker) — see `crates/basemyai/tests/maintenance_worker.rs`.
Design details: `docs/adr/ADR-037-native-adaptive-forgetting.md`,
`docs/adr/ADR-038-native-expired-memory-gc.md`.

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

- No published binary release (`cargo-dist` or equivalent) — build from
  source today.
- `assert_cmd` integration tests (`crates/basemyai-cli/tests/cli.rs`,
  `cargo test -p basemyai-cli`, **wired in CI gate**) cover every command that
  doesn't need the Candle embedder — `init`/`inspect`/`verify`/`migrate`/
  `list`/`forget`/`invalidate`/`purge`/`graph`/`forget-adaptive`/`gc`, plus
  key/agent/confirmation/already-exists/not-configured paths and the explicit
  absence of the `maintenance` subcommand group (both maintenance commands are
  flat root commands today, see above). `remember`/`recall`/`stats`/`export`/
  `import`/`consolidate` are still untested in CI (they load the embedding
  model, unavailable offline in CI) — `forget-adaptive`/`gc` do **not** need
  the embedder (raw store access, `open_engine`) so they're fully covered.
