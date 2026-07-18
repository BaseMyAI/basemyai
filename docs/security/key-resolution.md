# Passphrase utilisateur — résolution ADR-034

BaseMyAI chiffre chaque conteneur `.bmai` au repos (ADR-030). L'utilisateur
fournit une **passphrase** à l'ouverture ; elle n'est **jamais** persistée par le
moteur ni loguée (`EncryptionKey` masque le `Debug`).

L'architecture crypto **DEK/KEK** (enveloppe dans `crypto.meta`) est inchangée —
ce document ne couvre que **comment la passphrase arrive** au runtime.

## Ordre de résolution

| Priorité | Source | Usage typique |
|----------|--------|---------------|
| 1 | Argument explicite (`encryption_key` SDK) | Override programmatique |
| 2 | `BASEMYAI_DB_KEY` | CI/CD, Docker env, scripts |
| 3 | `BASEMYAI_ENCRYPTION_KEY` | Alias legacy (préférer `BASEMYAI_DB_KEY`) |
| 4 | `BASEMYAI_DB_KEY_FILE` | Chemin vers un fichier secret |
| 5 | `/run/secrets/basemyai_db_key` | Docker Swarm / Compose secrets |
| 6 | `~/.basemyai/key` | Développement local, post-`config key generate` |
| — | *(aucune)* | Erreur `KEY_REQUIRED` (CLI exit 3) |

Implémentation : `basemyai_core::EncryptionKey::resolve` /
`resolve_with_source`.

## Fichier local `~/.basemyai/key`

```bash
basemyai config key generate    # crée le fichier, n'affiche jamais la passphrase
basemyai config key check       # vérifie qu'une source est disponible
basemyai config key path        # affiche le chemin par défaut
```

**Backup obligatoire** : perte du fichier = perte définitive d'accès aux `.bmai`
chiffrés avec cette passphrase.

### Permissions Unix

Si la source est `~/.basemyai/key`, le moteur vérifie :

- `~/.basemyai` : au plus `0700` (pas d'accès groupe/autres) ;
- `~/.basemyai/key` : au plus `0600` (pas d'exécution, pas d'accès groupe/autres).

Sinon : erreur avec hint `chmod 700 ~/.basemyai` ou `chmod 600 ~/.basemyai/key`.

### Windows

Les vérifications `chmod` Unix ne s'appliquent pas. Protégez le profil
utilisateur ; l'intégration **DPAPI / Credential Manager** est prévue en **V2**
(ADR-034).

## Ce qu'il ne faut pas faire

- Stocker la passphrase dans `~/.basemyai/config.toml` (non supporté).
- Committer `.env`, `*.bmai`, ou `~/.basemyai/key` (voir `.gitignore`).
- Utiliser `change-me` ou d'autres placeholders en production.
- Logger ou afficher la passphrase (y compris dans les messages d'erreur).

## Docker (REST sidecar)

Préférer un **secret file** plutôt qu'une variable en clair dans `docker-compose.yml` :

```yaml
services:
  basemyai-rest:
    image: basemyai/basemyai-rest:latest
    secrets:
      - basemyai_db_key
    environment:
      BASEMYAI_DB_KEY_FILE: /run/secrets/basemyai_db_key
      BASEMYAI_REST_API_KEY_FILE: /run/secrets/basemyai_api_key
    ports:
      - "7743:7743"

secrets:
  basemyai_db_key:
    file: ./secrets/basemyai_db_key.txt
  basemyai_api_key:
    file: ./secrets/basemyai_api_key.txt
```

Créer les fichiers avec `chmod 600` et les ajouter à `.gitignore`.

## Variables d'environnement (référence)

| Variable | Rôle |
|----------|------|
| `BASEMYAI_DB_KEY` | Passphrase canonique |
| `BASEMYAI_ENCRYPTION_KEY` | Alias legacy |
| `BASEMYAI_DB_KEY_FILE` | Chemin vers un fichier contenant la passphrase |
| `BASEMYAI_DB_KEY_MODE` | `raw-key` par défaut (compatibilité) ou `passphrase` pour Argon2id |

## V2 — OS keyring (non implémenté)

- macOS Keychain
- Windows DPAPI / Credential Manager
- Linux libsecret / GNOME Keyring

Objectif : la passphrase ne repose plus en clair sur disque en dev desktop.
