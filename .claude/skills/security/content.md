# Skill: security — BaseMyAI (moteur natif, ADR-033)

## Surface d'attaque principale

BaseMyAI est un moteur de mémoire **local**. Vecteurs réalistes :

1. **Contournement d'isolation** entre `agent_id` (fuite cross-tenant)
2. **Memory poisoning** — contenu hostile rappelé dans le contexte LLM
3. **Exfiltration de passphrase** (logs, config, disque)
4. **Store corrompu** — panic ou lecture silencieuse sur `.bmai` malveillant
5. **REST/MCP exposés** sans auth ou hors loopback
6. **Auto-download silencieux** de modèles (réseau non consenti)

Docs détaillées : `docs/security/` et `SECURITY.md`.

---

## Isolation multi-agent (ADR-006)

**Invariant** : chaque opération storage est scellée par `AgentId` au niveau moteur
(préfixes KV natifs), pas seulement dans la façade `Memory`.

```rust
// BON — AgentId newtype, jamais de concat SQL
let agent = AgentId::new("tenant-a")?;
memory.remember("fact", layer).await?;

// MAUVAIS — string brute là où AgentId est attendu
```

Tests adversariaux CI : `p1_isolation_adversarial`, `export_isolation_adversarial`,
`isolation_recall_graph_adversarial`.

---

## Chiffrement natif (ADR-030)

```rust
// Production — toujours chiffré
NativeMemoryStore::open_encrypted(path, key)?;

// Clair — test-util uniquement
#[cfg(feature = "test-util")]
NativeMemoryStore::open(path)?;
```

Passphrase : `EncryptionKey::resolve()` (ADR-034). Jamais dans `config.toml`.
`Debug` masqué. Rotation : `rotate_key` (re-wrap DEK O(1)).

Erreurs stables : `WRONG_ENCRYPTION_KEY`, `ENCRYPTION_KEY_REQUIRED`, etc.

---

## Memory poisoning (ADR-035)

- `recall()` **exclut** `MemoryLayer::Procedural` par défaut.
- Opt-in : `RecallOptions { include_procedural: true }`.
- `Record.source` pour la provenance.
- Import JSONL : `--trusted` requis pour lignes procedural.

---

## REST / MCP

- REST : bind `127.0.0.1` par défaut ; Bearer obligatoire sauf `dev` + loopback.
- `Config::validate()` refuse `dev=true` + bind public.
- MCP HTTP : Bearer comparé en temps constant (`subtle`).

---

## Moteur natif — formats

- WAL/SST : CRC32 + AEAD ; `MAX_BATCH_OPS` sur decode batch.
- Fuzz nightly : `crates/basemyai-engine/fuzz/` (Linux/WSL, pas Windows MSVC).
- `format.lock` en CI.

---

## Zero network

Après setup explicite du modèle, `remember`/`recall`/graphe n'ouvrent pas de socket.
Test CI : `zero_network_recall`, `provision_without_consent_fails_when_model_absent`.

---

## Checklist avant commit touchant la sécurité

1. `cargo xtask ci` vert
2. Pas de `unwrap()` en lib ; pas de secret loggé
3. Nouveau comportement sensible → test adversarial + doc `docs/security/`
4. Décision architecturale → nouvel ADR (ne pas modifier les ADR existants)
