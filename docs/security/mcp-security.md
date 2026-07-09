# Sécurité MCP (`basemyai-mcp`)

## Surfaces

| Transport | Auth | Usage |
|-----------|------|-------|
| `stdio` | Processus parent de confiance | IDE local |
| `http` | Bearer token (comparaison constante) | Sidecar réseau local |

## Auth HTTP

Le jeton Bearer est comparé en **temps constant** (`subtle::ConstantTimeEq`) —
jamais `==` sur des secrets.

Variable : `BASEMYAI_MCP_API_KEY` (ou fichier équivalent selon la config).

## Clé de chiffrement store

Le serveur MCP résout la passphrase via `EncryptionKey::resolve()` (ADR-034) —
même ordre que CLI/REST. Jamais de placeholder `change-me` en production.

## Sampling MCP (consolidation)

La consolidation peut emprunter le LLM du client MCP (ADR-018). C'est un
choix **explicite** de l'opérateur : le trafic réseau vers le LLM du client
n'est pas couvert par la garantie « zero network » de la mémoire seule.

## Bornes d'entrée

Les handlers MCP appliquent les mêmes limites de taille que REST sur `text`,
`query`, entités et relations — refus `VALIDATION_ERROR` au-delà des plafonds
documentés.

## Tests

```bash
cargo test -p basemyai-mcp --no-default-features --features stdio,http,test-util
```
