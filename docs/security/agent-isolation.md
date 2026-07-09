# Isolation multi-agent (ADR-006, ADR-027)

## Invariant

Chaque lecture et écriture mémoire est scellée par un [`AgentId`](../adr/ADR-006-agent-isolation.md).
Il n'existe **pas** de mode « mémoire partagée » en V1.

## Mécanisme natif

Sur `basemyai-engine`, l'isolation est **structurelle** via préfixes de clés KV :

- `idx/memory/{agent_id}/…`
- `idx/fts/{agent_id}/…`
- `idx/graph/{agent_id}/…`

L'index vectoriel ANN est global ; chaque recall **post-filtre** obligatoirement
par `agent_id` après la recherche approximative.

## Tests adversariaux (CI)

```bash
cargo test -p basemyai --features test-util --test p1_isolation_adversarial
cargo test -p basemyai --features test-util --test export_isolation_adversarial
cargo test -p basemyai --features test-util --test isolation_recall_graph_adversarial
```

Scénarios couverts : `agent_id` hostile en FTS, export JSONL, traversée graphe
cross-agent, `search_graph` sans fuite.

## Bonnes pratiques intégrateur

- Traiter `agent_id` comme un identifiant de tenant, pas comme une chaîne libre
  affichée à l'utilisateur final sans validation.
- Ne jamais réutiliser le même `agent_id` pour des contextes de confiance
  différents sans isolation de store séparée.
