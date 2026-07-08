# ADR-004 — Les 4 couches mémoire

**Statut** : ✅ Accepted | **Date** : 2026-06

**Contexte**

« La mémoire d'un agent » n'est pas un concept unique. Un contexte de travail éphémère, un souvenir d'un événement passé, une procédure apprise et un fait établi ont des durées de vie, des modes d'accès et des stratégies de péremption différents. Tout mettre dans une seule table indifférenciée empêche de raisonner sur ces différences (TTL, GC, priorité de retrieval).

**Décision**

Quatre couches mémoire, modélisées en tables distinctes dans `basemyai` (jamais dans `basemyai-core`).

| Couche | Contenu | Durée de vie typique |
|---|---|---|
| `short_term` | Contexte de travail de la session courante | TTL court |
| `episodic` | Ce qui s'est passé et quand (événements, interactions) | Bornée dans le temps |
| `procedural` | Comment faire X : étapes, workflows, compétences apprises | Longue durée |
| `semantic` | Faits et connaissances, recherchables vectoriellement | Jusqu'à invalidation |

Chaque table porte les colonnes `valid_from` / `valid_until` (ADR-005) et est filtrée par `agent_id` (ADR-006). Index B-Tree adaptés au mode d'accès de chaque couche.

**Conséquences**

✅ Stratégie de péremption et de GC adaptée par couche.
✅ Le retrieval peut prioriser/filtrer par couche selon le besoin de l'agent.
✅ Modèle mental clair pour le développeur (« je range ça où ? »).
⚠️ Quatre schémas à maintenir cohérents.
⚠️ La frontière entre couches peut être floue pour certains cas d'usage ; documentation et exemples nécessaires.

**Alternatives rejetées**

Table mémoire unique avec un champ `type` — empêche d'optimiser index/GC par type, mélange des durées de vie incompatibles.

Couches arbitrairement configurables par l'utilisateur — complexité et incohérence ; les 4 couches couvrent les besoins réels des agents.
