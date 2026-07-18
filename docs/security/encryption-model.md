# Modèle de chiffrement — moteur natif (ADR-030)

## Vue d'ensemble

Chaque conteneur `.bmai` est un **répertoire** chiffré au repos :

```
store.meta    — marqueur de génération de format (ADR-039 §7), en clair
crypto.meta   — DEK scellée sous KEK dérivée de la passphrase utilisateur
wal.log       — enregistrements WAL scellés individuellement (WalEnvelope)
*.sst         — SST par blocs (ADR-039) : header en clair (bootstrap, sst_id),
                chaque bloc de données/l'index/le bloom/le footer scellés
                individuellement (EncryptedSstBlock, AAD liée à sst_id +
                type de section + numéro de section — anti-permutation)
```

Algorithme : **XChaCha20-Poly1305** (AEAD). Dérivation KEK : **SHA-256** + sel
dans `crypto.meta`.

**Modèle de menace du cache/RAM** (ADR-039 §5.6, cache implémenté N8.7) :
comme pour le reste du moteur, la menace couverte est le disque au repos,
pas la RAM du process — un bloc SST déchiffré reste en clair non seulement
le temps d'une lecture, mais **tout le temps où il reste résident dans le
cache de blocs** (`store::block_cache::BlockCache`, LRU borné en octets,
partagé par tout le moteur), posture inchangée depuis ADR-030. Le cache
n'introduit aucune nouvelle surface disque (rien n'est jamais persisté hors
mémoire) ; son invalidation par `sst_id` à la suppression d'une SST
(compaction) évite seulement une fuite mémoire, pas une fuite de
confidentialité — le clair en RAM était déjà le modèle de menace accepté.

Détails des types (`Nonce`, `Salt`, `Dek`), générateurs et frontière test/production :
[crypto-material.md](crypto-material.md).

## Règles produit

- **Production** : toutes les surfaces appellent `open_with_key` et respectent
  le mode explicite `raw-key`/`passphrase` porté par `EncryptionKey`.
- `Engine::open` (clair) existe derrière `test-util` pour les tests.
- La passphrase est fournie à l'ouverture ([key-resolution.md](key-resolution.md),
  ADR-034) et n'est **jamais** écrite sur disque ni loguée (`Debug` masqué).

## Rotation de clé

`rotate_key` re-scelle la DEK sous une nouvelle passphrase en **O(1)** (remplacement
atomique de `crypto.meta`). L'ancienne passphrase + une copie de l'ancien
`crypto.meta` permettent toujours la lecture — voir ADR-030 §4 (déviation
documentée du modèle de menace).

```bash
basemyai rotate-key --db ./agent.bmai --new-key "$NEW_PASSPHRASE" --passphrase
basemyai rotate-key --db ./agent.bmai --new-key "$NEW_PASSPHRASE" --passphrase --low-memory
basemyai rotate-key --db ./agent.bmai --new-key "$NEW_PASSPHRASE" --passphrase --full
```

Le profil `--low-memory` est un choix explicite pour matériel contraint
(19 MiB/t2/p1), à répéter lors de chaque rotation qui doit le conserver. Sans
ce flag, toute nouvelle création/rotation Argon2id utilise le profil normal
64 MiB/t3/p4. L'ouverture rejoue toujours les paramètres persistés.

`--full` génère une nouvelle DEK et ré-encrypte tous les enregistrements
vivants avant de publier atomiquement la nouvelle génération. Il demande
temporairement jusqu'à environ 2× l'espace disque. Il ne garantit ni
l'effacement physique de l'ancien ciphertext sur SSD, ni la révocation des
backups ou copies réalisés avant la rotation.

## Erreurs stables

| Condition | Code REST | Code CLI |
|-----------|-----------|----------|
| Mauvaise passphrase | `WRONG_ENCRYPTION_KEY` | `WRONG_ENCRYPTION_KEY` |
| Store chiffré, clé absente | `ENCRYPTION_KEY_REQUIRED` | `KEY_REQUIRED` |
| `crypto.meta` illisible | `CORRUPT_ENCRYPTION_METADATA` | `CORRUPT_ENCRYPTION_METADATA` |
| Échec AEAD opérationnel | `ENCRYPTION_ERROR` | `ENCRYPTION_ERROR` |
