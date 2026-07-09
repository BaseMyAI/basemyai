# Modèle de chiffrement — moteur natif (ADR-030)

## Vue d'ensemble

Chaque conteneur `.bmai` est un **répertoire** chiffré au repos :

```
crypto.meta   — DEK scellée sous KEK dérivée de la passphrase utilisateur
wal.log       — enregistrements WAL scellés individuellement (WalEnvelope)
*.sst         — fichiers SST scellés en bloc (SstEnvelope)
```

Algorithme : **XChaCha20-Poly1305** (AEAD). Dérivation KEK : **SHA-256** + sel
dans `crypto.meta`.

Détails des types (`Nonce`, `Salt`, `Dek`), générateurs et frontière test/production :
[crypto-material.md](crypto-material.md).

## Règles produit

- **Production** : toutes les surfaces appellent `open_encrypted` uniquement.
- `Engine::open` (clair) existe derrière `test-util` pour les tests.
- La passphrase est fournie à l'ouverture ([key-resolution.md](key-resolution.md),
  ADR-034) et n'est **jamais** écrite sur disque ni loguée (`Debug` masqué).

## Rotation de clé

`rotate_key` re-scelle la DEK sous une nouvelle passphrase en **O(1)** (remplacement
atomique de `crypto.meta`). L'ancienne passphrase + une copie de l'ancien
`crypto.meta` permettent toujours la lecture — voir ADR-030 §4 (déviation
documentée du modèle de menace).

```bash
basemyai rotate-key --db ./agent.bmai --new-key "$NEW_PASSPHRASE"
```

## Erreurs stables

| Condition | Code REST | Code CLI |
|-----------|-----------|----------|
| Mauvaise passphrase | `WRONG_ENCRYPTION_KEY` | `WRONG_ENCRYPTION_KEY` |
| Store chiffré, clé absente | `ENCRYPTION_KEY_REQUIRED` | `KEY_REQUIRED` |
| `crypto.meta` illisible | `CORRUPT_ENCRYPTION_METADATA` | `CORRUPT_ENCRYPTION_METADATA` |
| Échec AEAD opérationnel | `ENCRYPTION_ERROR` | `ENCRYPTION_ERROR` |
