# Sécurité des formats on-disk (`basemyai-engine`)

## Fichiers

| Artefact | Protection | Versionnage |
|----------|------------|-------------|
| WAL record | CRC32 + (AEAD par enregistrement si chiffré) | `format/wal.rs` |
| SST | ADR-039 : format **par blocs** — header/footer/index/bloom filter et chaque bloc de données CRC32 (ou scellés individuellement en AEAD si chiffré, `EncryptedSstBlock`, AAD liée à `sst_id`‖`section_type`‖`section_no`) | `format/sst_block.rs` |
| Blocs vector/graphe | CRC32 + layout versionné | `format.lock` |

Corrigé le 2026-07-23 (remédiation de l'audit adversarial BaseMyAI, finding
CRYPTO-4) : ce tableau décrivait encore un module `format/sst.rs` et un
modèle de scellement SST "fichier entier" qui n'existent plus depuis
ADR-039 (SST par blocs, N8) — le module réel est `format/sst_block.rs`, et
chaque bloc (donnée, index, bloom, footer) est scellé **individuellement**,
pas le fichier dans son ensemble. Voir `docs/security/encryption-model.md`
pour la description à jour et testée (`encrypted_block_moved_between_two_ssts_fails_authentication`,
`encrypted_blocks_swapped_within_the_same_sst_fail_authentication`).

`format.lock` est vérifié en CI (`cargo xtask format-lock`) — toute dérive de
format casse le gate.

## Ouverture d'un store non fiable

Un répertoire `.bmai` fourni par un tiers est traité comme **input hostile** :

- CRC / AEAD invalides → `CorruptWal` / `CorruptSstHeader` / `CorruptSstDataBlock` /
  `CorruptSstBlockIndex` / `CorruptSstBloomFilter` / `CorruptSstFooter` /
  `CorruptEncryptedSstBlock` selon la section concernée (jamais un panic).
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
