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
Defense: every read and write is filtered by agent_id AT THE SQL LEVEL
         a query without a valid agent_id fails — it never falls back to
         returning another agent's data
         isolation is a security INVARIANT, not a config option (ADR-006)
         tested with an adversarial dataset that tries to bypass the filter
```
This is the **highest-priority** threat. A cross-agent leak is an exfiltration of one tenant's data into another's hands.

**SQL Injection via Agent Inputs**
```
Attack : memory.remember('"; DROP TABLE semantic; --', agent_id="A")
         or a crafted agent_id / filter designed to escape the WHERE clause
Defense: ALL inputs are bound as SQL parameters, never interpolated
         the agent_id filter cannot be escaped via injection
         (a broken agent_id filter would become a cross-agent leak — see above)
```

**Model Integrity / Supply Chain (HuggingFace)**
```
Attack : a tampered all-MiniLM-L6-v2 model file is substituted on disk
         or during the product-orchestrated fetch, poisoning embeddings
Defense: the Embedder NEVER auto-downloads — it receives a LOCAL path
         the fetch is orchestrated explicitly by the product, with checksum
         verification, and the model is cached locally after a verified fetch
         no network connection is opened by the core by default
```

**Encryption Key Handling**
```
Attack : the at-rest encryption key is recovered from disk, logs, or memory
Defense: libSQL's built-in encryption (feature `crypto`) encrypts the
         database at rest (ADR-007, updated by ADR-011)
         in `basemyai`, encryption is MANDATORY — a store cannot be opened
         without a key
         the key is supplied at open time and NEVER stored or logged
         key custody is the consumer's responsibility (documented)
```

### Medium Priority

**Memory Poisoning**
```
Attack : an adversarial memory is written so that future recalls surface
         attacker-controlled content for unrelated queries
Mitigation: memory is scoped per-agent (a poisoned memory cannot cross agents)
            temporal validity (valid_until) bounds the lifetime of any entry
            embeddings are a relevance signal, not a trust signal — the
            consuming agent remains responsible for trusting retrieved content
```

**Database Corruption**
```
Attack : a malicious or corrupted .db file is supplied to the engine
Defense: WAL + ACID transactions; corruption detected at open returns an
         explicit error rather than silently reading corrupt data
         migrations are versioned; an incompatible schema is rejected
```

**Maintenance Worker Misuse / Contention**
```
Attack : crafted data forces the background GC into pathological behavior,
         starving the critical path
Mitigation: the worker runs off the critical path (WAL + busy_timeout +
            spaced scheduling); GC tasks are injected by the product, not
            attacker-controlled
```

### Out of Scope

- Vulnerabilities in the embedding model's mathematical behavior
- The security of the agent / LLM consuming BaseMyAI
- Social engineering attacks
- Physical access attacks against an unlocked machine with the key in memory

---

## Encryption at Rest

`basemyai` requires encryption at rest via libSQL's built-in **`crypto`** feature: the database is instantiated with an `encryption_key`, and the file on disk is unreadable without it. The key is supplied at open time and **never stored**.

In `basemyai-core`, encryption is **optional** (`Store::open(path, key: Option<…>)`) so the agnostic foundation can be reused by consumers that don't need it. In the `basemyai` product it is **mandatory** — opening a memory store without a key fails.

> ⚠️ Build note: libSQL's `crypto` feature requires **CMake** at build time (ADR-011). It is **opt-in** and deferred — the native vector support compiles without CMake. This replaces the former sqlcipher + sqlite-vec linkage risk (D4), which disappears with libSQL's native vectors. It only affects encryption-mandatory consumers.

---

## Per-Agent Isolation

Every memory row carries an `agent_id`. Every read and write is filtered by `agent_id` **at the SQL level** (ADR-006). A query without a valid `agent_id` fails; it never returns another agent's data. There is no "shared memory" mode in V1 — strict isolation is the only mode, and it is a security invariant.

---

## Data Handling

| Data | Stored | Leaves machine |
|---|---|---|
| Memory content (text) | libSQL, encrypted (`crypto`) | Never |
| Embedding vectors | libSQL native vectors, encrypted | Never |
| `agent_id` / namespace | libSQL, encrypted | Never |
| Temporal metadata (`valid_from`/`valid_until`) | libSQL, encrypted | Never |
| Encryption key | Never stored (supplied at open) | Never |
| Embedding model file | Local cache (`~/.basemyai/models`) | Fetched once, explicitly, by the product |

No data leaves the machine by default. The only possible network access is the **product-orchestrated** model fetch — the `Embedder` itself never opens a connection. Telemetry is **off by default**.
