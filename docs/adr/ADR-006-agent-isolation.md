# ADR-006 — Isolation multi-agent par `agent_id`

**Statut** : ✅ Accepted | **Date** : 2026-06

**Contexte**

Un service hébergeant plusieurs agents (ou plusieurs utilisateurs/tenants) partage le même store mémoire. Un agent ne doit **jamais** lire la mémoire d'un autre. Une fuite cross-agent n'est pas un bug fonctionnel : c'est un incident de sécurité (exfiltration de données d'un tenant vers un autre). Filtrer côté application est fragile — un oubli, et la fuite est silencieuse.

**Décision**

Chaque ligne mémoire porte un `agent_id`. **Toute** lecture et **toute** écriture sont filtrées par `agent_id` **au niveau SQL**, dans `basemyai`. Une requête sans `agent_id` valide échoue ; elle ne retourne jamais les données d'un autre agent.

```sql
WHERE agent_id = ?1   -- jamais omis, jamais optionnel
```

Le filtre `agent_id` est passé à `basemyai-core.knn(q, k, filtre)` comme partie du filtre SQL fourni par l'appelant. Le core applique le filtre sans savoir ce qu'est un agent. **L'isolation est un invariant de sécurité, pas une option de configuration.**

**Conséquences**

✅ Fuite cross-agent structurellement empêchée : le filtre est au niveau SQL, pas dans la logique applicative.
✅ Argument compliance direct (multi-tenant, RGPD).
✅ S'exprime via le mécanisme de filtre générique du core — pas de concept d'agent dans `basemyai-core`.
⚠️ Le filtre `agent_id` ne doit jamais pouvoir être contourné par injection SQL → inputs paramétrés, jamais interpolés (REQ-032).
⚠️ Pas de mémoire partagée volontaire entre agents en V1 (le défaut, et le seul mode, est l'isolation stricte).

**Alternatives rejetées**

Filtrage côté application (en Rust/Python, après la requête) — un oubli = fuite silencieuse ; trop fragile pour un invariant de sécurité.

Une DB par agent — coûteux à grande échelle (milliers d'agents), perd les avantages du mono-fichier, complexifie le worker de maintenance.

`agent_id` optionnel (isolation opt-in) — transforme un invariant de sécurité en piège ; rejeté.
