# basemyai-rest

Sidecar HTTP/JSON local pour `basemyai`. Expose le moteur de mémoire aux
langages sans binding Rust natif (le binding direct — `bindings/basemyai-py`,
`bindings/basemyai-node` — reste le chemin recommandé quand il est
disponible). 100 % local : aucune donnée ne quitte la machine, aucun réseau
externe hors le `bind`/port configurés.

`basemyai-rest` reste une **surface fine** au-dessus de `basemyai` : il ne
réimplémente aucune logique de mémoire, de temporalité ou d'isolation — il
extrait/valide une requête HTTP, résout un `Memory` pour l'agent ciblé, appelle
l'API `basemyai` correspondante, et mappe le résultat/l'erreur vers un contrat
HTTP stable.

## Architecture

Organisation en **tranches verticales** (un module par capacité), pas par
couche technique générique :

```
src/
  config/        StartupConfig (une fois, au boot) / RuntimeConfig (par requête)
  context/       AppState, MemoryRegistry (résolution/cache des Memory par agent), RequestContext
  http/          RestError (modèle d'erreur stable), extracteurs, pagination, middlewares
  provider/      MemoryProvider (production = store natif + Candle ; test-util = in-memory)
  server/        bootstrap, assemblage du Router, arrêt propre, telemetry
  endpoints/     un dossier par domaine : health, memories, graph, maintenance, agents, events, context
```

Chaque domaine sous `endpoints/` expose `pub fn router() -> Router<AppState>`
et un `contract.rs` pour ses DTOs partagés — jamais les types internes
`basemyai::Record`/`basemyai::Reached`/etc. ne traversent la frontière HTTP
directement.

## Démarrage

```bash
# Build par défaut (feature "embed" — Candle, pèse lourd) :
cargo run -p basemyai-rest

# Build léger sans Candle (surface HTTP uniquement, pas de provider réel) :
cargo run -p basemyai-rest --no-default-features
```

Le binaire sans `embed` ne peut pas construire de provider réel et retourne
une erreur au démarrage — `embed` est nécessaire pour un usage en production.

### Variables d'environnement

Chargées après `~/.basemyai/config.toml` (`[rest]`), qui a le dernier mot.

| Variable                                            | Rôle                                                      | Défaut               |
|------------------------------------------------------|------------------------------------------------------------|-----------------------|
| `BASEMYAI_REST_BIND`                                 | Adresse d'écoute                                            | `127.0.0.1` (loopback) |
| `BASEMYAI_REST_PORT`                                 | Port d'écoute                                               | `7743`                |
| `BASEMYAI_REST_DB_PATH` (ou `BASEMYAI_DB_PATH`)       | Chemin du conteneur mémoire                                 | `~/.basemyai/memory.bmai` |
| `BASEMYAI_REST_DB_KEY` (ou `BASEMYAI_DB_KEY`)         | Clé de chiffrement (résolue via `EncryptionKey::resolve`)   | —                     |
| `BASEMYAI_REST_MODEL_PATH` (ou `BASEMYAI_MODEL_PATH`) | Chemin du modèle d'embedding provisionné                    | —                     |
| `BASEMYAI_REST_FETCH` (ou `BASEMYAI_FETCH`)           | Consent explicite au téléchargement du modèle (ADR-010)     | `false`               |
| `BASEMYAI_REST_AGENT_POLICY`                          | `any` ou tout autre valeur = agent fixe                     | `any`                 |
| `BASEMYAI_REST_AGENT_ID` (ou `BASEMYAI_AGENT_ID`)     | Fixe `agent_policy` à cet agent unique                      | —                     |
| `BASEMYAI_REST_API_KEY`                               | Clé Bearer attendue (obligatoire hors `dev`)                | —                     |
| `BASEMYAI_REST_DEV`                                   | `1`/`true` : désactive l'auth (loopback uniquement)         | `false`               |

`timeout_secs`, `max_result_bytes`, `max_body_bytes` ne sont réglables que via
`~/.basemyai/config.toml` (`[rest]`), pas par variable d'environnement, à
défaut de besoin identifié pour les faire varier par déploiement.

### Sécurité par défaut

- Bind loopback (`127.0.0.1`) par défaut — un bind non loopback avec `dev=true`
  est refusé au démarrage.
- Hors `dev`, une `api_key` est obligatoire — refusé au démarrage sinon.
- Limite de corps de requête (défaut 1 MiB), timeout de requête (défaut 30s),
  plafond de taille de réponse `recall`/`recall_graph` (défaut 256 KiB,
  `truncated: true` si dépassé).
- Aucun secret (clé API, clé de chiffrement) n'apparaît jamais dans un
  `Debug`/`Display`, un log, une erreur ou une réponse HTTP.
- Erreurs internes (SQL, chemin local, détails crypto) jamais renvoyées au
  client — loguées côté serveur avec le `request_id` associé.

## Routes

Toutes les routes métier sont montées sous `/v1` et protégées par
`Authorization: Bearer <api_key>` (sauf en mode `dev`). `/health/live` et
`/health/ready` (racine, hors `/v1`) ne nécessitent jamais d'auth.

| Méthode & route                          | Rôle                                                    |
|-------------------------------------------|----------------------------------------------------------|
| `GET /health/live`                        | Liveness (processus vivant)                              |
| `GET /health/ready`                       | Readiness (provider prêt)                                |
| `GET /v1/health`                          | Alias déprécié de `/health/live` (compatibilité)          |
| `POST /v1/remember`                       | Mémorise un texte                                        |
| `POST /v1/remember_batch`                 | Mémorise un lot de textes (atomique)                      |
| `POST /v1/recall`                         | Recherche sémantique (vectorielle)                        |
| `POST /v1/recall_hybrid`                  | Recherche hybride (vectorielle + BM25, RRF)               |
| `POST /v1/memories/{id}/invalidate`       | Invalide un souvenir (soft-delete)                        |
| `DELETE /v1/memories/{id}`                | Supprime physiquement un souvenir                         |
| `POST /v1/recall_graph`                   | Traversée du graphe d'entités                             |
| `POST /v1/graph/entities`                 | Ajoute/met à jour une entité                              |
| `POST /v1/graph/relations`                | Ajoute/met à jour une relation                             |
| `POST /v1/graph/search`                   | Recall filtré par entité du graphe                        |
| `POST /v1/maintenance/collect_expired`    | GC temporel manuel (ADR-038)                              |
| `POST /v1/maintenance/forget_adaptive`    | Oubli adaptatif manuel (ADR-037)                          |
| `POST /v1/compile_context`                | Context Engine : compile un contexte borné et traçable     |
| `GET /v1/agent/{agent_id}/stats`          | Compteurs par couche                                       |
| `GET /v1/agent/{agent_id}/export`         | Export JSONL de l'agent                                    |
| `POST /v1/agent/{agent_id}/import`        | Réimporte un export JSONL                                  |
| `DELETE /v1/agent/{agent_id}`             | Purge totale de l'agent (confirmation requise)             |
| `GET /v1/events` / `GET /v1/watch`        | Flux SSE des événements mémoire (deux noms, même handler)  |

Détail complet des schémas de requête/réponse : [`openapi.yaml`](openapi.yaml).

### Exemples curl

```bash
export KEY=... # BASEMYAI_REST_API_KEY

curl -sX POST localhost:7743/v1/remember \
  -H "Authorization: Bearer $KEY" -H 'Content-Type: application/json' \
  -d '{"agent_id":"agent-42","text":"Alice works at Acme Corp since 2023.","layer":"semantic"}'

curl -sX POST localhost:7743/v1/recall \
  -H "Authorization: Bearer $KEY" -H 'Content-Type: application/json' \
  -d '{"agent_id":"agent-42","query":"Where does Alice work?","k":5}'

curl -N localhost:7743/v1/watch?agent_id=agent-42 \
  -H "Authorization: Bearer $KEY"
```

## Modèle d'erreur

```json
{"error": {"code": "invalid_agent_id", "message": "A valid agent_id is required.", "request_id": "...", "details": null}}
```

`code` est stable et snake_case, indépendant du texte de `message` (qui peut
changer sans casser un client). Codes actuels : `unauthorized`,
`invalid_request`, `invalid_agent_id`, `invalid_layer`, `invalid_importance`,
`conflict`, `payload_too_large`, `rate_limited`, `wrong_encryption_key`,
`store_locked`, `internal_error`. `request_id` est toujours présent et
identique au header `x-request-id` de la réponse.

## SSE (`/events`, `/watch`)

Relaie `basemyai::Memory::watch` : un événement JSON (`agent_id`, `kind`,
`layer`, `id` — jamais le contenu du souvenir) par ligne `data:`. Le canal
sous-jacent est *lossy* : un client SSE lent perd des événements plutôt que de
bloquer `remember`/`forget` pour les autres clients ou pour l'agent lui-même.
Aucune tâche de fond n'est `spawn`ée pour un flux SSE — à la déconnexion du
client (ou à l'arrêt gracieux du serveur), l'abonnement est libéré
immédiatement.

## Tests

```bash
cargo test -p basemyai-rest --no-default-features --features test-util
```

Tout le test-util (contract/integration/security) tourne sans réseau, sans
LLM externe, sans modèle téléchargé, sans port fixe et sans état local
préexistant — `InMemoryProvider` remplace le store natif + Candle.

- `tests/contract/` — forme JSON stable, codes HTTP/erreur, compatibilité de route.
- `tests/integration/` — scénarios métier multi-étapes (remember→recall,
  temporalité, isolation, batch atomique, graphe, export/import, SSE).
- `tests/security/` — entrées malformées, limites de payload, non-fuite de
  secrets, valeurs par défaut sécurisées.

Reproduire exactement la CI (matrice de features par crate) :

```bash
cargo xtask check   # fmt --check + clippy par crate
cargo xtask test    # tests par crate (config légère, sans embed)
cargo xtask ci      # les deux
```

`cargo clippy --workspace` ne reproduit **pas** la CI pour ce crate — la CI
clippie/teste `basemyai-rest` avec `--no-default-features --features
test-util` spécifiquement ; `cargo xtask` reproduit cette matrice exacte.

## Compatibilité

- `GET /v1/health` reste servi (alias déprécié de `/health/live`, même forme
  de réponse) pour ne pas casser les clients existants.
- `lib.rs` réexporte `build_app` (alias de `server::build_router`) pour les
  consommateurs qui dépendaient de ce nom.

## Limites connues / non implémenté

- **`consolidation` (Phase 2 `basemyai::cognition`) n'est pas exposé en REST** :
  cette route nécessiterait un `LlmInference` réel, qu'aucun binding LLM
  local n'est aujourd'hui câblé dans ce sidecar — l'exposer aurait simulé une
  fonctionnalité non réellement disponible.
- **`GET /v1/memories` (liste paginée) n'existe pas** : `basemyai::Memory`
  n'expose actuellement pas de méthode de listing paginé — seul le trait bas
  niveau `MemoryStore` en a une, via un chemin que `MemoryProvider`/
  `MemoryRegistry` n'exposent pas aujourd'hui. Ajouter cette route
  nécessiterait d'abord une API `basemyai` correspondante.
- Le readiness (`/health/ready`) ne fait aucune I/O active par appel — il
  rapporte un fait déjà établi au démarrage, pas une sonde temps réel.
