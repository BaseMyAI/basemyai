# Sécurité des formats on-disk (`basemyai-engine`)

## Fichiers

| Artefact | Protection | Versionnage |
|----------|------------|-------------|
| WAL record | CRC32 + (AEAD si chiffré) | `format/wal.rs` |
| SST | CRC32 entier + entrées | `format/sst.rs` |
| Blocs vector/graphe | CRC32 + layout versionné | `format.lock` |

`format.lock` est vérifié en CI (`cargo xtask format-lock`) — toute dérive de
format casse le gate.

## Ouverture d'un store non fiable

Un répertoire `.bmai` fourni par un tiers est traité comme **input hostile** :

- CRC / AEAD invalides → `CorruptWal` / `CorruptSst` (pas de panic).
- Mauvaise passphrase → `WrongEncryptionKey` **avant** lecture des payloads.
- Batch WAL : `MAX_BATCH_OPS = 10_000` — refus des compteurs démesurés.

## Tests

```bash
cargo test -p basemyai-engine --features test-util --test malformed_open
cargo test -p basemyai-engine --test format_lock
```

## Fuzzing (nightly)

Cibles `cargo-fuzz` sous `crates/basemyai-engine/fuzz/` — job CI
`.github/workflows/fuzz.yml` (Linux uniquement, toolchain nightly).

Ne fait **pas** partie du gate `cargo xtask ci` (trop lent, dépend de libFuzzer).
