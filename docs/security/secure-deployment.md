# Déploiement sécurisé

## Checklist opérateur

1. **Passphrase** : via secret file ou `BASEMYAI_DB_KEY` injecté au runtime —
   jamais dans `config.toml` ni dans l'image Docker en clair.
2. **Backup** : sauvegarder le répertoire `.bmai` **et** la passphrase (perte =
   perte définitive).
3. **REST** : bind loopback ou derrière TLS ; `BASEMYAI_REST_API_KEY` fort ;
   ne pas activer `BASEMYAI_REST_DEV=1` hors développement local.
4. **Modèle embedding** : provisioning explicite (`basemyai setup --fetch`) avec
   vérification SHA-256 — pas d'auto-download silencieux.
5. **Permissions Unix** : `chmod 700 ~/.basemyai` et `chmod 600 ~/.basemyai/key`
   si fichier local (vérifié à l'open, ADR-034).

## Docker Compose (REST)

```yaml
services:
  basemyai-rest:
    image: basemyai/basemyai-rest:latest
    secrets:
      - basemyai_db_key
      - basemyai_api_key
    environment:
      BASEMYAI_DB_KEY_FILE: /run/secrets/basemyai_db_key
      BASEMYAI_REST_API_KEY_FILE: /run/secrets/basemyai_api_key
    ports:
      - "127.0.0.1:7743:7743"  # loopback only
```

## Rotation de clé planifiée

```bash
basemyai rotate-key --db /data/agent.bmai --new-key "$(cat /run/secrets/new_key)"
```

Archiver l'ancien `crypto.meta` selon votre politique de rétention (voir ADR-030 §4).

## Zero network après setup

Une fois le modèle en cache local, `remember`/`recall`/graphe n'ouvrent pas de
socket. Voir [zero-network-after-setup.md](../zero-network-after-setup.md).

Job CI : `zero-network-after-setup` (proxy invalide + tests locaux).
