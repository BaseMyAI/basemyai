# Sécurité REST (`basemyai-rest`)

## Bind et mode dev

| Paramètre | Défaut | Règle |
|-----------|--------|-------|
| `bind` | `127.0.0.1:7743` | Ne pas exposer sans reverse-proxy + TLS |
| `dev` (`BASEMYAI_REST_DEV=1`) | `false` | Désactive Bearer **uniquement** si bind loopback |

`Config::validate()` **refuse** `dev=true` avec un bind non-loopback — échec au
démarrage, pas à la première requête.

## Authentification

- Production : header `Authorization: Bearer <BASEMYAI_REST_API_KEY>`.
- Résolution clé API : env / fichier secret (symétrique à ADR-034 pour la DB key).

## Erreurs crypto stables

Les erreurs de déchiffrement remontent avec des codes JSON distincts
(`WRONG_ENCRYPTION_KEY`, `ENCRYPTION_KEY_REQUIRED`, …) — voir
[encryption-model.md](encryption-model.md).

## Recall et poisoning

`POST /recall` accepte `include_procedural: false` par défaut — aligné ADR-035.

## Tests CI

```bash
cargo test -p basemyai-rest --no-default-features --features test-util
```

Inclut `dev_mode_rejects_non_loopback_bind` et le mapping d'erreurs crypto.

## Déploiement

Voir [secure-deployment.md](secure-deployment.md) pour Docker, secrets et
réseau.
