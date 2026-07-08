# ADR-012 — Phase 2 Cognition — Graphe, RRF, Oubli adaptatif, Consolidation

**Statut** : ✅ Accepted | **Date** : 2026-06

**Contexte**

ADR-011 a posé le socle libSQL + vecteur natif + async, et noté en conséquence que « `vector_top_k` applique le filtre après le top-k → sur-échantillonner pour garantir `k` résultats filtrés ». La VISION §3 identifie cinq ingrédients d'une mémoire efficace : vecteurs, **graphe**, temporalité, **consolidation** et **oubli**. La pile libSQL + CTE récursives permet de tout tenir dans le même fichier sans système externe. La Phase 2 implémente ces quatre piliers restants dans le crate `basemyai`.

**Décision**

Quatre mécanismes implémentés dans `basemyai` (jamais dans `basemyai-core`) :

**1 — Oversampling KNN (correctif ADR-011 § TODO)**

`vector_top_k` applique le filtre *après* le top-k interne. Quand un filtre est présent, on tire `k × 8` (constante `KNN_OVERSAMPLE`) depuis l'index et on re-trie après jointure. Distance réelle via `vector_distance_cos` exposée dans le SELECT. Implémenté dans `basemyai-core::store::vector_knn`.

**2 — Graphe entités / relations**

Migration v2 : tables `entity(id, agent_id, kind, label, created_at)` + `edge(id, agent_id, src_id, rel, tgt_id, weight, created_at)`.

Traversée multi-sauts via **CTE récursive** :

```sql
WITH RECURSIVE reach(id, kind, label, depth) AS (
    SELECT id, kind, label, 0 FROM entity WHERE id = ?1 AND agent_id = ?2
    UNION                          -- UNION, pas UNION ALL : déduplication + terminaison de cycles
    SELECT e2.id, e2.kind, e2.label, r.depth + 1
    FROM   reach r
    JOIN   edge  ed ON ed.src_id = r.id AND ed.agent_id = ?2
    JOIN   entity e2 ON e2.id = ed.tgt_id
    WHERE  r.depth < ?3
)
SELECT * FROM reach;
```

`UNION` (pas `UNION ALL`) garantit la terminaison sur les cycles sans sentinelles supplémentaires.

**3 — Fusion multi-signal par RRF**

`rrf_fuse(rankings, k)` implémente la *Reciprocal Rank Fusion* :

```
score(doc) = Σ 1 / (60 + rank(doc, signal))
```

`k = 60` (valeur standard, choisie pour minimiser la sensibilité aux variations de rang). Tri final déterministe : score décroissant, puis id croissant. Provenance des signaux conservée dans `Fused::contributions`.

**4 — Oubli adaptatif**

Migration v3 : colonnes `importance REAL` et `last_access INTEGER` sur la table `memory`.

Score de rétention utilisé pour le GC périodique :

```
score = importance + H / (H + max(0, now - last_access))
```

où `H` est la demi-vie en secondes (`recency_half_life_secs`). Decay **hyperbolique** (pas exponentielle) pour deux raisons : (a) libSQL n'expose pas `pow`/`exp`/`ln` comme fonctions SQL ; (b) `0.5^(age/H)` avec `age ≈ 1.78 × 10⁹` s (timestamp Unix) s'arrondit à `0.0` en flottant, rendant tous les souvenirs indiscernables. La forme hyperbolique reste distinguable à toutes les échelles réelles.

Fenêtre de rétention par `agent_id` : `ROW_NUMBER() OVER (PARTITION BY agent_id ORDER BY score DESC)`, suppression des lignes `rn > capacity`.

**5 — Consolidation épisodes → faits**

`consolidate(memory, llm: &dyn LlmInference)` lit les N derniers épisodes (`episodic`), soumet un prompt d'extraction structurée au LLM, parse la réponse JSON, et :
- Peuple le graphe (entités + relations) via `ON CONFLICT DO UPDATE` (idempotent).
- Promeut les faits extraits en `semantic` avec déduplication par contenu exact.

Le pipeline est **idempotent** : relancer la consolidation sur les mêmes épisodes ne duplique ni les entités du graphe ni les faits sémantiques.

**Conséquences**

✅ Les cinq ingrédients de la mémoire hybride (VISION §3) tiennent dans un seul fichier libSQL.
✅ Pas de Kuzu, pas de LanceDB, pas de second système : graphe + vecteur + temporel en un seul fichier.
✅ Oubli adaptatif : capacité par agent bornée, signal de récence réel à toutes les échelles Unix.
✅ Consolidation idempotente : safe à rejouer périodiquement depuis le `MaintenanceWorker`.
✅ Graphe sans cycle infini : `UNION` dans la CTE récursive garantit la terminaison.
⚠️ libSQL n'expose pas `pow`/`exp`/`ln` — tout le calcul de score doit rester en arithmétique pure.
⚠️ `consolidate` nécessite un `LlmInference` injecté — câbler le provider est la responsabilité de l'appelant.
⚠️ Wiring de la consolidation dans `MaintenanceWorker` reporté : `MaintenanceTask::run` reçoit un `&Store`, pas `Arc<Memory>` + LLM provider.

**Alternatives rejetées**

`UNION ALL` dans la CTE récursive — pas de déduplication des nœuds, boucle infinie sur les cycles.

Decay exponentielle `0.5^(age/H)` — underflow à 0.0 à toutes les échelles de timestamps Unix réels ; indiscernable entre les souvenirs.

`pow`/`exp` en SQL — non disponibles dans libSQL (pas de fonction math standard) ; remplacés par arithmétique pure.

Graphe dans un système externe (Kuzu, Neo4j) — deux systèmes à synchroniser, viole le mono-fichier.
