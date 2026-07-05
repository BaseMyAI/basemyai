# ADR-005 — RAG temporel — `valid_from` / `valid_until`

**Statut** : ✅ Accepted | **Date** : 2026-06

**Contexte**

Une mémoire qui ignore le temps ment. « L'utilisateur est sur le plan Free » était vrai au T1, faux au T2. Un RAG classique (cosine pur) retourne les deux faits avec la même confiance ; l'agent affirme alors des informations périmées. Il manque une notion de validité temporelle intégrée à la recherche, pas appliquée après coup.

**Décision**

Chaque mémoire porte deux colonnes temporelles : `valid_from` et `valid_until` (nullable = « valide jusqu'à invalidation explicite »). Le recall est une **requête hybride** qui combine, en une seule requête SQL, la similarité vectorielle et le filtre temporel :

```sql
-- conceptuel
SELECT id, text
FROM   memory
WHERE  agent_id = ?1                         -- isolation (ADR-006)
  AND  (valid_until IS NULL OR valid_until > now())   -- validité temporelle
ORDER  BY vec_distance_cosine(embedding, ?2)  -- pertinence (sqlite-vec, ADR-002)
LIMIT  ?3;
```

Le filtre temporel est passé à `VectorIndex.knn(q, k, filtre)` comme le filtre SQL fourni par l'appelant. `basemyai-core` exécute le KNN ; il ne sait pas que le filtre concerne le temps — c'est le sens, propriété de `basemyai`.

**Conséquences**

✅ Le recall ne retourne que ce qui est pertinent **ET** encore valide.
✅ Historiser un fait (le remplacer) = poser `valid_until` sur l'ancien et insérer le nouveau ; l'historique reste auditable.
✅ Le filtre temporel s'exprime via le mécanisme générique de filtre du core — pas de couplage métier dans le core.
⚠️ `now()` doit être cohérent (horloge système) ; les fuseaux/horaires sont gérés en UTC.
⚠️ Les lignes expirées s'accumulent jusqu'au passage du GC (Active Worker, ADR-008).

**Alternatives rejetées**

Filtrer le temps **après** le KNN, côté application — inefficace (on récupère puis on jette), et risque de retourner < k résultats valides.

Pas de notion de temps (cosine pur) — l'agent affirme des faits périmés ; c'est précisément le problème à résoudre.

Versionnement d'événements complet (event sourcing) — overkill pour V1 ; deux colonnes temporelles suffisent.
