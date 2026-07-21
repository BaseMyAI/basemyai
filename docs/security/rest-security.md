# Sécurité REST (`basemyai-rest`)

## Bind et mode dev

| Paramètre | Défaut | Règle |
|-----------|--------|-------|
| `bind` | `127.0.0.1:7743` | Ne pas exposer sans reverse-proxy + TLS |
| `dev` (`BASEMYAI_REST_DEV=1`) | `false` | Désactive Bearer **uniquement** si bind loopback |

`basemyai_rest::config::validate()` (`StartupConfig`/`RuntimeConfig` séparés
depuis la restructuration en tranches verticales) **refuse** `dev=true` avec
un bind non-loopback, et refuse l'absence d'`api_key` hors `dev` — échec au
démarrage, pas à la première requête.

## Authentification

- Production : header `Authorization: Bearer <BASEMYAI_REST_API_KEY>`.
- Résolution clé API : env / fichier secret (symétrique à ADR-034 pour la DB key).

## Erreurs crypto stables

Les erreurs de déchiffrement remontent avec des codes JSON stables et
snake_case (`wrong_encryption_key`, `store_locked`, …), mappés une seule fois
dans `http::error::RestError` — voir [encryption-model.md](encryption-model.md)
et `crates/basemyai-rest/README.md` pour la liste complète des codes.

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
