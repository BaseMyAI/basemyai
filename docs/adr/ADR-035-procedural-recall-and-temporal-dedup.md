# ADR-035 — Recall procedural opt-in et dédup temporelle `exact_fact_exists`

**Statut** : ✅ Accepted  
**Date** : 2026-07-08  
**Relation** : amends ADR-004 (couches mémoire), ADR-012 (consolidation) ; complète
l'audit sécurité 2026-07-08 (memory poisoning + dédup).

## Contexte

Deux lacunes identifiées lors de l'audit sécurité :

1. **Memory poisoning** : la couche `procedural` (playbooks, instructions internes)
   était incluse dans `recall()` général — un contenu hostile injecté en
   procedural pouvait resurfacer dans le contexte LLM sans action explicite.
2. **Dédup consolidation** : `exact_fact_exists` ignorait la validité temporelle —
   un fait **invalidé** ou **expiré** bloquait encore la re-promotion lors de
   `consolidate`, contredisant ADR-005.

## Décision

### 1. Exclusion procedural par défaut

- `RecallOptions { include_procedural: false }` est le défaut de `recall()`,
  `recall_hybrid()` et les chemins vectoriels/keyword génériques.
- `recall_by_layer(Procedural, …)` et `include_procedural: true` restent
  disponibles pour les cas légitimes (agent qui consulte ses propres playbooks).
- Champ `Record.source` pour tracer la provenance (`user`, `consolidation`, `import`).
- Import JSONL : refus des lignes `procedural` sans flag `trusted`.

### 2. `exact_fact_exists(agent, content, at)`

La signature prend un instant `at` (Unix UTC). Seuls les faits **sémantiques**,
contenu **exact**, valides à `at` selon `[valid_from, valid_until)`, comptent.
`consolidate` passe `now_unix()`.

## Conséquences

- **Breaking** pour les intégrateurs qui s'appuyaient sur le recall procedural
  implicite — migration : `include_procedural: true` ou `recall_by_layer`.
- **Breaking** pour les mocks `MemoryStore` : ajout du paramètre `at` sur
  `exact_fact_exists` et `include_procedural` sur les recalls vectoriels.
- Tests adversariaux dédiés en CI (`poisoning_procedural_recall`,
  `temporal_dedup_consolidation`).

## Alternatives rejetées

- Supprimer la couche procedural : trop restrictive pour les workflows agent.
- Filtrer côté LLM uniquement : trop tardif — le poison est déjà dans le contexte.
- `exact_fact_exists` sans filtre temporel + invalidation physique : casse
  l'audit trail et la portabilité JSONL.
