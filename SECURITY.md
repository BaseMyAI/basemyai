# Security Policy

## Supported Versions

| Version | Supported |
|---|---|
| 0.x (current) | ✅ Active |

---

## Reporting a Vulnerability

**Do not open a public GitHub issue for security vulnerabilities.**

Report via: **security@basemyai.com**

Include:
- Description of the vulnerability
- Steps to reproduce
- Potential impact
- Suggested fix (optional)

**Response SLA**: acknowledgment within 48 hours, fix timeline within 7 days for critical issues.

We follow responsible disclosure: we'll coordinate a public disclosure date with you after the fix is released.

---

## Threat Model

BaseMyAI runs locally and stores an AI agent's memory — often the most sensitive data in a product (conversations, user profiles, established facts). It may host **multiple agents or tenants** in a single store. The following attack surfaces are in scope.

### High Priority

**Cross-Agent Memory Leakage**
```
Attack : agent "tenant-A" crafts a query that returns memory belonging
         to agent "tenant-B" (the agent_id filter is bypassed)
Defense: every read and write is scoped by agent_id AT THE STORAGE LAYER
         (key-prefix isolation on the native engine — ADR-027/ADR-033)
         vector recall post-filters by agent after ANN search
         isolation is a security INVARIANT, not a config option (ADR-006)
         tested with an adversarial dataset (p1_isolation_adversarial, CI)
```

**Encryption Key Handling**
```
Attack : the at-rest encryption key is recovered from disk, logs, or memory
Defense: native engine encryption (ADR-030) — XChaCha20-Poly1305, DEK/KEK
         envelope in crypto.meta, WAL and SST sealed at rest
         in basemyai, production surfaces open ONLY via open_encrypted
         the passphrase is supplied at open time and NEVER stored or logged
         by the product (EncryptionKey Debug is redacted)
         centralized resolution ADR-034 — see docs/security/key-resolution.md
         never store the passphrase in config.toml; prefer secret files in prod
         key custody and backup remain the operator's responsibility
```

**Model Integrity / Supply Chain (HuggingFace)**
```
Attack : a tampered all-MiniLM-L6-v2 model file is substituted on disk
Defense: the Embedder NEVER auto-downloads — it receives a LOCAL path
         the fetch is orchestrated explicitly by setup/CLI/MCP/REST with
         consent (--fetch, BASEMYAI_FETCH=1), checksum verification (SHA-256)
         no network connection is opened by the core by default
```

### Medium Priority

**Memory Poisoning**
```
Attack : adversarial content is written so future recalls surface attacker text
Mitigation: procedural layer excluded from default recall (ADR-035, opt-in only)
            TrustLevel / Record.source on all recall surfaces (ADR-036)
            import JSONL re-tags source=import (anti-spoof); procedural import
            requires --trusted; per-agent isolation; temporal validity
            consolidation prompt anti-injection; bounds on consolidate_apply
            embeddings are a relevance signal, not a trust signal — the
            consuming agent remains responsible for trusting retrieved content
```

**Database / Store Corruption**
```
Attack : a malicious or corrupted .bmai directory is supplied to the engine
Defense: WAL + atomic batches; CRC32 + AEAD on sealed artifacts; corruption
         detected at open returns typed errors (never silent bad reads)
         format.lock pins wire formats; drift breaks CI
```

**Exposed REST Sidecar**
```
Attack : basemyai-rest bound to a public interface without authentication
Defense: default bind is 127.0.0.1; Bearer auth required in production
         BASEMYAI_REST_DEV=1 disables auth but ONLY on loopback addresses
         (refused at startup if dev + non-loopback)
```

### Out of Scope

- Vulnerabilities in the embedding model's mathematical behavior
- The security of the agent / LLM consuming BaseMyAI
- Social engineering attacks
- Physical access attacks against an unlocked machine with the key in memory

---

## Encryption at Rest (ADR-030, ADR-033)

Since ADR-033, BaseMyAI uses **only** the native `basemyai-engine` backend. A `.bmai` store is a directory containing:

- `crypto.meta` — DEK wrapped under a KEK derived from the user key (SHA-256 + salt)
- `wal.log` — WAL records sealed individually (WalEnvelope)
- `*.sst` — SST files sealed as a whole (SstEnvelope)

**Production rule:** all product surfaces (CLI, REST, MCP, Python/Node bindings) call `open_encrypted`. Plaintext persistent stores exist only behind the `test-util` feature for tests.

The user key is supplied at open time (`BASEMYAI_DB_KEY`, binding parameter, etc.) and is **never** written to disk or logs by BaseMyAI.

Key rotation (`rotate_key`) re-wraps the DEK in O(1) via atomic `crypto.meta` replace — see ADR-030 §4 for the documented threat-model deviation (old key + old `crypto.meta` copy).

---

## Per-Agent Isolation (ADR-006, ADR-027)

Every memory row carries an `agent_id`. On the native engine, isolation is **structural** via key prefixes (`idx/memory/…`, `idx/fts/…`, `idx/graph/…`). Vector search uses a global ANN index with a mandatory post-filter by agent. There is no "shared memory" mode in V1.

### Reproduce the Public Isolation Test

```bash
cargo test -p basemyai --features test-util --test p1_isolation_adversarial
```

---

## Zero Network by Default

After setup (model cached locally), memory operations do not open network connections. The only product-orchestrated network access is:

- Explicit model fetch (`basemyai setup --fetch`, `BASEMYAI_FETCH=1`, binding consent flags)
- Optional local LLM detection/consolidation (localhost probes when consolidate runs)
- MCP sampling (routes to the client's LLM if enabled)

No telemetry. No silent cloud fallback.

---

## Data Handling

| Data | Stored | Leaves machine |
|---|---|---|
| Memory content (text) | Native engine, encrypted (ADR-030) | Never |
| Embedding vectors | Native vector index, encrypted at rest | Never |
| `agent_id` / namespace | Native KV, encrypted | Never |
| Temporal metadata | Native KV, encrypted | Never |
| Encryption key | Never stored by product | Never |
| Embedding model file | Local cache (`~/.basemyai/models`) | Fetched once, explicitly |

---

## Historical Note

ADR-007/ADR-011 described libSQL/SQLCipher encryption. **ADR-033 superseded that path.** libSQL is no longer in the active workspace; security claims in this file apply to the native engine only. Historical ADRs under `docs/adr/` are not rewritten.

---

## Deep-Dive Documentation

| Topic | Document |
|-------|----------|
| Threat model | [docs/security/threat-model.md](docs/security/threat-model.md) |
| Encryption (DEK/KEK, rotation) | [docs/security/encryption-model.md](docs/security/encryption-model.md) |
| User key resolution | [docs/security/key-resolution.md](docs/security/key-resolution.md) |
| Multi-agent isolation | [docs/security/agent-isolation.md](docs/security/agent-isolation.md) |
| Memory poisoning | [docs/security/memory-poisoning.md](docs/security/memory-poisoning.md) |
| MCP surface | [docs/security/mcp-security.md](docs/security/mcp-security.md) |
| REST sidecar | [docs/security/rest-security.md](docs/security/rest-security.md) |
| On-disk formats | [docs/security/native-engine-format-security.md](docs/security/native-engine-format-security.md) |
| Secure deployment | [docs/security/secure-deployment.md](docs/security/secure-deployment.md) |
