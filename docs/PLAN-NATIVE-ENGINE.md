# Plan — Moteur natif BaseMyAI (pari long terme)

**Statut** : roadmap actée — décision formalisée dans `docs/ADR-024-native-engine.md`, backlog dans `docs/TODO-NATIVE-ENGINE.md`.
**Date** : 2026-07-02 (v2 — enrichi par l'analyse d'organisation du repo SurrealDB, cf. §3).

## Contexte

`docs/status.md` §10 listait « Migration Turso DB (pur Rust, zéro C) » comme
⏸️ Deferred (V2, ADR-011). En instruisant ce chantier, la décision produit a
changé de nature : plutôt que d'adopter le moteur pur-Rust de quelqu'un
d'autre (Turso), l'objectif devient de **construire le moteur natif de
BaseMyAI** — stockage, index vectoriel, graphe, et à terme un langage de
requête maison, façon SurrealDB. Motivation assumée : pas une nécessité
technique court terme, un **pari stratégique long terme** (« posséder la
techno de bout en bout »), le temps n'est pas la contrainte.

Ce n'est pas un chantier nouveau — c'est la **réouverture délibérée** d'une
question déjà tranchée une fois. **ADR-019** (18 juin 2026) a explicitement
évalué « implement a native `.bmai` backend now » et l'a **rejeté**, avec une
raison précise : *« It would require crash recovery, compaction, vector
indexing, encryption and migration machinery before the memory product is
proven. »* Ce plan ne répète pas cette évaluation — la réponse à « maintenant,
pour V1 » était non ; la question posée aujourd'hui est différente : « comme
pari délibéré multi-années, en parallèle du produit qui continue de tourner
sur libSQL ». Les cinq risques qu'ADR-019 a listés (crash recovery,
compaction, index vecteur, chiffrement, migration) sont exactement le plan de
travail ci-dessous — pas ignorés, séquencés. La décision formelle est actée
par **ADR-024**.

**Ce que ce pivot change** :

- Le chantier « migration Turso » **disparaît** — le spike Turso n'a plus de
  sens, on n'adopte plus le moteur de quelqu'un d'autre.
- Le chantier **multi-modèles d'embedding reste valide tel quel** et continue
  en parallèle — `Embedder` est déjà agnostique du moteur de stockage.
- Le chantier **sync P2P devient plus facile à terme, pas plus dur** : un
  WAL/format de changement conçu par nous peut exposer le change-capture comme
  primitive de premier ordre dès le départ. Il reste en aval du moteur natif.

## 1. Ce qui existe déjà et qu'on ne reconstruit pas

- **`StorageEngine` trait + `EngineCapabilities`/`EngineKind`**
  (`crates/basemyai-core/src/storage/engine.rs`) — déjà posé comme *« the
  first stable seam for backend identity and feature discovery »*.
  `EngineKind::Libsql` existe ; un `EngineKind::Native` s'ajoute mécaniquement.
- **`basemyai::storage::MemoryStore`/`LibsqlMemoryStore`** (ADR-020) — la
  majorité de `basemyai` (memory/cognition) ne parle déjà plus directement au
  driver libSQL. Un nouveau moteur n'a qu'à implémenter ce trait. Zones
  résiduelles à migrer à part (déjà documentées, ADR-020) :
  `memory/porting.rs`, `maintenance/{gc,forgetting}`.
- **Suite de tests de contrat** (`crates/basemyai/tests/storage_contract.rs`,
  `crates/basemyai-core/tests/contracts.rs`) — le filet de sécurité du projet.
  Tout nouveau moteur doit les passer avant d'être pris au sérieux.
- **`.bmai` comme identité publique découplée du moteur interne**
  (`docs/format/bmai-v1.md`, ADR-019) — le fichier reste `.bmai` que le moteur
  interne soit libSQL ou natif.
- **Benchmark de référence** (`docs/benchmarks/m6-knn-results-2026-07-01.md`)
  — chiffres KNN 10k/100k déjà mesurés sur libSQL : cible de parité du futur
  index vectoriel natif.
- **Analyse comparative déjà faite contre un vrai moteur maison**
  (`docs/research/surrealdb-gap-analysis.md` §6) : code source SurrealDB vérifié
  (`core/src/idx/trees/{hnsw,diskann}/`), conclusion que l'index natif libSQL
  derrière `vector_top_k` est déjà de la famille **DiskANN (LM-DiskANN)** —
  pas un brute-force. Le moteur natif doit **égaler** un DiskANN déjà correct,
  pas « rattraper » un retard.

## 2. Ce que « posséder le moteur de bout en bout » veut dire, en couches

Quatre couches, profils de risque très différents — les confondre en un seul
« on réécrit tout » referait l'erreur qu'ADR-019 a évitée.

### Couche 1 — Stockage durable (page/segment store, WAL, transactions, crash recovery)

La fondation, le risque le plus élevé, le seul qui peut tuer le projet en
silence (corruption/perte de données). Décision de conception à trancher
**avant** le code produit, via plusieurs prototypes jetables comparés :

- B-tree copy-on-write (façon `redb`, SQLite) — bonnes garanties de lecture
  concurrente, compaction/GC de pages modérée.
- LSM-tree (façon `fjall`, RocksDB) — écritures séquentielles rapides,
  compaction plus complexe, meilleur pour le change-capture (utile pour le
  sync plus tard).
- Étudier `redb`/`fjall`/`sled` (vérifier leur statut de maintenance en 2026,
  ne pas assumer) comme référence de design. Décision ouverte, pas tranchée
  ici — mérite son propre spike avant de figer l'architecture de la couche.

**Discipline de test non négociable, construite *avant* le moteur lui-même** :

- Harnais de **cohérence après crash** : kill -9 en boucle sous charge
  d'écriture, réouverture, vérification d'intégrité complète. En CI dès le
  premier commit du moteur.
- **Fuzzing** (cargo-fuzz, toolchain nightly séparée façon SurrealDB) sur les
  surfaces de parsing bas niveau : encodage/décodage de clés, replay de WAL,
  lecture de pages. SurrealDB fuzze parseur et exécuteur ; notre équivalent
  est le format on-disk.
- **`format.lock` — l'équivalent du `revision.lock` de SurrealDB** : un
  fichier commité qui fige la version de sérialisation de chaque type persisté
  (`TypeName:version(hash)`), avec un check CI qui échoue si un type on-disk
  change sans bump explicite de version. C'est le garde-fou anti-corruption
  de format — un moteur maison n'hérite d'aucune décennie de durcissement
  SQLite, ce lock est ce qui empêche une PR innocente de casser silencieusement
  la lecture des fichiers existants.
- Idéalement : test déterministe façon simulation (`turmoil`/Jepsen-style)
  pour la concurrence.

### Couche 2 — Index vectoriel natif (HNSW/DiskANN pur Rust)

Risque modéré, algorithme bien documenté. Cible de parité : benchmarks M6
(10k/100k) sur libSQL. Construit par-dessus la Couche 1 (le store fournit
get/put par clé + itération triée ; l'index est une structure logique
au-dessus).

### Couche 3 — Graphe natif (remplace les CTE récursives)

Risque modéré. Réutilise `tests/graph.rs` existants comme spec de comportement
à égaler (scoping `agent_id`, cycle-safety).

### Couche 4 — Langage de requête maison (façon SurrealQL)

Risque technique le plus faible, **urgence produit la plus faible**.
`docs/research/surrealdb-gap-analysis.md` §0 documente déjà que l'avantage structurel
de BaseMyAI est que l'agent appelle `remember`/`recall`, jamais du SQL —
exposer un langage de requête à l'agent abandonnerait cet avantage. Plus de
valeur comme outil interne/admin/CLI qu'en surface agent. À confirmer avant de
démarrer. Si construit : suivre le découpage SurrealDB en micro-crates
séquentielles (`token` → `parser` → `ast`), qui rend le pipeline compilable en
parallèle, testable et fuzzable isolément.

## 3. Organisation cible — patterns SurrealDB adoptés

L'analyse du repo SurrealDB (2026-07-02, deux agents d'exploration) a produit
une liste de patterns d'organisation dont le chantier hérite. Les décisions :

### 3.1 Layout du moteur

Un **nouveau crate `crates/basemyai-engine`** (interne, non publié tant que
non prouvé), organisé dès le départ selon la séparation qui a fait ses preuves
chez SurrealDB (`core/src/{kvs,key,idx}`) :

```text
crates/basemyai-engine/src/
  store/       ← pages/segments, WAL, transactions, crash recovery (Couche 1)
  key/         ← encodage du keyspace, isolé, un module par entité
  idx/         ← index secondaires : vector/ (Couche 2), graph/ (Couche 3), fts/
  format/      ← types persistés versionnés (couverts par format.lock)
  error.rs     ← thiserror, #[non_exhaustive]
```

Il implémente `basemyai::storage::MemoryStore` (ADR-020) et
`basemyai_core::StorageEngine` — branché derrière `EngineKind::Native`, activé
par feature Cargo `engine-native`, **jamais le défaut** avant parité prouvée.
Micro-crates plus tard seulement si les temps de compilation le justifient
(mesurés, façon `compile-time-analysis.md` de SurrealDB — pas supposés).

### 3.2 Tests déclaratifs multi-backend

Le pattern le plus rentable de SurrealDB pour nous : leur `language-tests` —
un test = un fichier de scénario (données + attendus), un runner qui rejoue
**le même scénario sur chaque backend** (`--backend`). Notre équivalent : un
runner `memory-tests` qui rejoue des scénarios mémoire
(remember/recall/invalidate/graphe/validité temporelle) sur `Libsql` **et**
`Native`, et diffe les résultats. C'est le juge de paix de la parité — bien
plus scalable que dupliquer des tests Rust à la main. À construire pendant la
Phase 1, avant que le moteur natif ait quoi que ce soit à prouver.

### 3.3 DX du workspace (chantier 0 — préalable, s'applique à tout le repo)

Constat de l'audit : aucun justfile/cargo-make, et **les commandes documentées
ne reproduisent pas la CI** (clippy par-crate avec combinaisons de features
que `cargo clippy --workspace` ne couvre pas — un dev peut passer localement
et casser en CI). Un chantier multi-années sans DX solide meurt. Actions :

- **`justfile`** à la racine encodant exactement la matrice CI : `just check`
  (fmt+clippy config CI), `just test`, `just test-embed`, `just test-crypto`,
  `just ci` (tout). CONTRIBUTING/CLAUDE pointent vers ces cibles ; idéalement
  la CI les appelle aussi (single source of truth).
- **Assainissement docs** : une seule source de statut (`docs/status.md`), un
  seul changelog (racine), analyses ponctuelles archivées sous
  `docs/research/`, suppression des résidus (`docs/ANALYSIS.md`), AGENTS.md
  réduit à un pointeur vers CLAUDE.md, `openapi-sidecar.yaml` déplacé dans
  `crates/basemyai-rest/`.
- Reporté (P2, décision séparée) : éclater le `ADR.md` monolithique en
  `docs/adr/`, déplacer `bindings/` sous `crates/`, sortir
  `basemyai-branding/`.

## 4. Stratégie anti-risque

Le produit actuel (libSQL, publié crates.io/PyPI) continue de tourner pendant
toute la durée du chantier (« strangler fig ») :

1. Le moteur natif se développe derrière `EngineKind::Native` + feature
   `engine-native`, jamais le défaut avant parité prouvée.
2. Jugé sur les mêmes suites de tests de contrat qui pinnent déjà le
   comportement attendu, plus le runner déclaratif multi-backend (§3.2).
3. Doit franchir la même barre de hardening que libSQL en M6 (pool, bench KNN,
   stress 1h, key rotation) **plus** la suite crash-consistency, le fuzzing et
   `format.lock` (§ Couche 1) — que libSQL n'a jamais eu besoin d'avoir chez
   nous (décennies de durcissement SQLite héritées gratuitement ; un moteur
   maison part de zéro).
4. La bascule du défaut est une décision séparée, actée par un nouvel ADR une
   fois la parité prouvée par des chiffres.

## 5. Séquencement par jalons (pas par calendrier)

Backlog détaillé : `docs/TODO-NATIVE-ENGINE.md`.

| Phase | Contenu | Dépend de |
| --- | --- | --- |
| 0 | **Chantier 0** : DX + organisation repo (§3.3) ; ADR-024 rédigé ✅ | — |
| 0b | Spike de conception Couche 1 : prototypes comparés B-tree CoW vs LSM, étude `redb`/`fjall`/`sled` | Phase 0 |
| 1 | Couche 1 : store durable + WAL + txn, jugé sur harnais crash-consistency construit en premier ; `format.lock` + fuzzing en CI ; runner déclaratif multi-backend (§3.2) | Phase 0b |
| 2 | Couche 2 : index vectoriel natif, parité bench M6 | Phase 1 |
| 3 | Couche 3 : graphe natif, parité `tests/graph.rs` | Phase 1 |
| 4 | Parité complète tests de contrat + chiffrement + FTS/BM25 | Phases 1–3 |
| 4b | Décision de bascule du défaut (nouvel ADR) | Phase 4 |
| — | Multi-modèles d'embedding (indépendant, en parallèle dès maintenant) | Aucune |
| 5 | Sync P2P | Phase 1 au minimum, idéalement Phase 4 |
| 6 | Couche 4 : langage de requête maison (micro-crates token→parser→ast) | Phase 4 |

**À ne pas faire** : construire la Couche 4 avant que la Couche 1 soit prouvée
durable. Ne pas geler le travail produit sur libSQL pendant que le moteur
natif mûrit. Ne pas écrire une ligne du store avant que le harnais
crash-consistency existe.
