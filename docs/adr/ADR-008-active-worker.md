# ADR-008 — Active Worker — thread de fond

**Statut** : ✅ Accepted | **Date** : 2026-06

**Contexte**

Le RAG temporel (ADR-005) laisse s'accumuler des lignes expirées (`valid_until` dépassé). Sans nettoyage, la DB grossit indéfiniment et les index se dégradent. Faire ce travail sur le chemin critique (à chaque write/read) ajoute de la latence imprévisible. Il faut un mécanisme de fond, découplé.

**Décision**

Un **Active Worker** : thread de fond (tokio) qui exécute périodiquement des tâches de maintenance, **sans bloquer** le chemin critique.

Côté **`basemyai-core`**, c'est le `MaintenanceWorker` : il fait tourner la boucle ; **les tâches sont injectées par le consommateur** (mécanisme au core, sens au consommateur). Le core fournit `PRAGMA optimize` / VACUUM partiel comme briques, mais n'embarque aucune tâche métier en dur.

Côté **`basemyai`**, les tâches enregistrées sont :
- **Task 1 — Garbage Collection** : nettoyer ou archiver les lignes dont le `valid_until` est expiré.
- **Task 2 — Optimisation** : `PRAGMA optimize`, VACUUM partiel si nécessaire.

```rust
worker.register(GcExpiredRows);     // tâche métier basemyai
worker.register(OptimizeDb);        // brique core, planifiée par basemyai
```

**Conséquences**

✅ Le chemin critique (write/recall) n'absorbe jamais le coût du GC ou de l'optimisation.
✅ La DB reste bornée et performante dans le temps.
✅ Le core reste agnostique : il fait tourner la boucle, le GC par `valid_until` est une tâche **injectée** par `basemyai`.
⚠️ Le GC introduit un délai entre l'expiration d'une ligne et son nettoyage effectif (les lignes expirées sont déjà exclues du recall par le filtre temporel, donc invisibles entre-temps).
⚠️ Un worker de fond mal réglé peut entrer en contention avec les écritures → WAL + busy_timeout + planification espacée.

**Alternatives rejetées**

GC synchrone sur le chemin critique — ajoute une latence imprévisible à chaque opération.

Pas de GC (laisser grossir) — DB non bornée, dégradation des perfs dans le temps.

Tâches de maintenance codées en dur dans `basemyai-core` — viole l'agnosticité du core (le GC par `valid_until` est un concept métier).
