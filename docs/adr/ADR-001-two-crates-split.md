# ADR-001 — Découpage en deux crates `basemyai-core` / `basemyai`

**Statut** : ✅ Accepted | **Date** : 2026-06

**Contexte**

BaseMyAI doit être consommé par des publics incompatibles. Les builders d'agents Python et JS veulent une API mémoire de haut niveau (« remember », « recall », RAG temporel). Mais le même cœur — pool SQLite durci, sqlite-vec, embeddings, chiffrement, worker — est exactement ce dont un consommateur **Rust** a besoin **sans** la sémantique mémoire. En particulier, ForgeMyAI (moteur de contexte de code) veut le socle vectoriel, mais ses concepts sont `Symbol`/`Edge`, pas `agent_id`/`valid_until`.

Si on empaquette tout dans un seul crate, le consommateur Rust hérite de concepts métier qui n'ont aucun sens pour lui (RAG temporel, isolation par agent, GC par date). Et PyO3 comme NAPI-RS doivent de toute façon wrapper le **même** cœur — la séparation core/sémantique est due quoi qu'il arrive.

**Décision**

Un seul workspace Cargo, **deux crates publiables indépendamment** :

```
basemyai-core   socle AGNOSTIQUE métier
                Store durci · sqlite-vec · Candle · sqlcipher · MaintenanceWorker
                ne connaît JAMAIS : agent_id, valid_from/valid_until, les 4 couches,
                ni Symbol/Edge

basemyai        sémantique mémoire, posée SUR basemyai-core
                4 couches · RAG temporel · isolation agent_id · chiffrement obligatoire
                + bindings Python / Node / REST
```

Règle de dépendance : les flèches pointent toujours **vers `basemyai-core`**. Il ne `use` jamais `basemyai` ni `forge-*`.

**Le pattern clé : mécanisme au core, sens au consommateur.** `basemyai-core.knn(q, k, filtre?)` applique un filtre SQL fourni par l'appelant ; le `MaintenanceWorker` exécute des tâches injectées par le produit. Le core fournit le mécanisme générique ; le consommateur fournit le sens.

Les 4 surfaces de consommation :

| Surface | Consommateur | Couche |
|---|---|---|
| SDK Python (PyO3) | builders Python | `basemyai` |
| SDK Node (NAPI-RS) | builders JS/TS | `basemyai` |
| Sidecar REST | Go, Ruby, autres | `basemyai` |
| Crate Rust natif | **ForgeMyAI** | **`basemyai-core`** |

**Conséquences**

✅ ForgeMyAI consomme `basemyai-core` comme crate Rust natif — pas de FFI, pas de HTTP, pas d'overhead.
✅ `basemyai-core` reste testable et réutilisable sans rien savoir du métier mémoire.
✅ PyO3 et NAPI wrappent le même cœur ; la dette de séparation est payée une fois.
✅ Histoire d'écosystème : *« ForgeMyAI, powered by BaseMyAI »*.
⚠️ Deux crates à versionner et publier ; discipline semver stricte exigée.
⚠️ Tentation de faire fuiter un concept métier dans le core — contrée par un test d'agnosticité (grep des termes interdits en CI).

**Alternatives rejetées**

Crate unique tout-en-un — fait hériter au consommateur Rust (ForgeMyAI) le RAG temporel et l'`agent_id`, qui n'ont aucun sens pour du code.

Troisième crate neutre `coremyai` possédé par personne — un repo de plus à nommer, versionner, documenter, sans bénéfice en contexte solo. Le socle a un propriétaire naturel : BaseMyAI.

Repos séparés — friction inutile ; un seul workspace Cargo suffit, avec deux crates publiables.
