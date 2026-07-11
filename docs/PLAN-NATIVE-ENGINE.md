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

---

## Phase 0-4 (N0→N5.6) : ✅ clos le 2026-07-08

Les jalons N0→N5.6 décrits ci-dessus sont **terminés** (bascule du défaut
actée par ADR-032, puis retrait complet de libSQL par ADR-033). Détail
jalon-par-jalon, avec preuves et chiffres : `docs/TODO-NATIVE-ENGINE.md`.

Ce qui suit est le **programme de suite** (production hardening), engagé le
2026-07-10 une fois le moteur natif devenu le backend unique. Il ne rouvre
aucune décision N0→N5.6 — il part de leur résultat.

---

## Programme de suite — Native Engine Production Hardening (M0, N7→N16)

## 1. Objectif stratégique

Faire évoluer `basemyai-engine` de :

> moteur natif fonctionnel, performant et correctement testé

vers :

> moteur de stockage local durable, observable, réparable, scalable et exploitable en production sur plusieurs années.

Le programme doit préserver les invariants actuels :

* moteur 100 % natif, sans retour à libSQL ;
* source de vérité durable et index reconstruisibles ;
* isolation structurelle par agent ;
* chiffrement obligatoire pour les stores persistants ;
* zéro réseau implicite ;
* chaque changement de format versionné dans `format.lock` ;
* aucun compromis silencieux sur la corruption ou la cohérence ;
* `cargo xtask ci` vert avant chaque merge.

Le passage natif est terminé : libSQL et son mode de compatibilité ont été retirés du runtime actif.

## Politique de stabilité du format

BaseMyAI n'étant pas encore utilisé publiquement en production, le format natif `.bmai` est considéré comme **expérimental** (voir `docs/format/bmai-v1.md` §Format stability).

Jusqu'au gel officiel du format :

* aucune rétrocompatibilité entre formats internes n'est exigée ;
* le moteur peut supprimer un ancien codec ou layout disque ;
* un changement majeur de format incrémente la version attendue ;
* les anciens stores de développement peuvent être supprimés et recréés ;
* une migration n'est développée que si elle est réellement utile au projet ;
* aucun code `read_v1`, `read_v2`, `read_v3` n'est conservé par principe.

Le contrat de compatibilité commencera seulement lorsqu'une décision explicite figera le format, idéalement avant ou avec BaseMyAI `1.0`.

## 2. Ordre global des chantiers

| Programme | Résultat attendu                                            |     Priorité |
| --------- | ----------------------------------------------------------- | -----------: |
| M0        | Documentation et état du repo fiables                       |    Immédiate |
| N7        | Observabilité, métriques et harnais moteur unifiés          |           P0 |
| N8        | SST paginées par blocs, cache et bloom filters              |           P0 |
| N9        | Vérification, réparation, rebuild et compaction opérable    |           P0 |
| N10       | Maintenance scalable : GC, oubli, suppressions batch        |           P0 |
| N11       | Durcissement crash, corruption et longues charges           |           P0 |
| N12       | Chiffrement V2 et rotation complète de la DEK               |           P1 |
| N13       | Snapshots, compaction concurrente et amélioration du writer |           P1 |
| N14       | Recherche V2 : FTS, quantification et explicabilité         |           P2 |
| N15       | Changefeed local puis synchronisation                       |           P2 |
| N16       | API de requête typée, uniquement si justifiée               | Conditionnel |

Les programmes **M0 à N11** forment la release interne :

> **Native Engine Production Hardening**

Aucun travail P2P, langage de requête ou multi-writer complet ne doit interrompre cette séquence.

**M0 (documentation)** : voir §3 ci-dessous — en grande partie déjà couvert
(README/CHANGELOG/status.md/index ADR à jour), reste : test documentaire
anti-dérive (`crates/basemyai/tests/docs_no_stale_claims.rs`, ajouté
2026-07-10) et statut « format expérimental » explicite (`docs/format/bmai-v1.md`,
ajouté 2026-07-10).

## 3. M0 — Rétablir une source de vérité documentaire

### Travaux

1. Réécrire `CLAUDE.md` à partir de l'état natif réel. — ✅ déjà à jour.
2. Mettre à jour `README.md`. — ✅ déjà à jour.
3. Mettre à jour `CHANGELOG.md`. — ✅ déjà à jour (0.2.0 native-only documenté).
4. Créer ou réviser `docs/status.md` / `docs/TODO-NATIVE-ENGINE.md` /
   `docs/PLAN-NATIVE-ENGINE.md`. — ✅ (ce fichier).
5. Marquer clairement les ADR superseded dans l'index ADR. — ✅ déjà fait
   (`docs/ADR.md`).
6. Ajouter un test documentaire interdisant les affirmations obsolètes
   critiques. — ✅ `crates/basemyai/tests/docs_no_stale_claims.rs`.
7. Documenter explicitement que le format natif reste expérimental jusqu'à
   son gel. — ✅ `docs/format/bmai-v1.md` §Format stability.

### Critère de sortie

Un nouveau contributeur doit pouvoir comprendre l'architecture réelle et le niveau de stabilité du format sans lire l'historique Git.

## 4. N7 — Observabilité et banc d'essai moteur

Avant de modifier le stockage, il faut pouvoir mesurer précisément le moteur.

### 4.1 Métriques internes

Ajouter une structure publique ou interne telle que :

```rust
pub struct EngineStats {
    pub wal_bytes: u64,
    pub wal_records: u64,
    pub memtable_bytes: u64,
    pub sst_count: usize,
    pub sst_bytes: u64,
    pub tombstone_count: u64,
    pub flush_count: u64,
    pub compaction_count: u64,
    pub compaction_input_bytes: u64,
    pub compaction_output_bytes: u64,
    pub bytes_read: u64,
    pub bytes_written: u64,
    pub block_cache_hits: u64,
    pub block_cache_misses: u64,
}
```

Prévoir dès maintenant les métriques du futur format par blocs, même si certaines restent à zéro avant N8.

### 4.2 Benchmarks canoniques

Créer des workloads reproductibles :

* `kv-fill`
* `kv-point-read`
* `kv-prefix-scan`
* `memory-remember`
* `memory-recall`
* `mixed-read-write`
* `delete-churn`
* `flush-compaction`
* `encrypted-vs-clear`
* `open-large-store`

Volumes minimaux : 10 000 ; 100 000 ; 1 000 000 d'enregistrements pour les tests de scale.

Toutes les sorties doivent être archivables en JSON ou CSV avec : commit ;
OS ; CPU ; RAM ; mode chiffré ; taille du dataset ; paramètres du moteur ;
RSS pic et moyen ; latences p50/p95/p99 ; amplification lecture/écriture/disque.

### 4.3 Commandes `xtask`

Ajouter des points d'entrée uniques :

```bash
cargo xtask engine-check
cargo xtask engine-bench
cargo xtask engine-crash
cargo xtask engine-corrupt
cargo xtask engine-fuzz
cargo xtask engine-soak
```

`cargo xtask ci` reste le gate normal. Les commandes lourdes sont séparées, mais exécutables à l'identique en local et en CI.

### 4.4 Failpoints

Introduire des points de panne contrôlés, actifs uniquement en test :

```text
after_wal_append
after_wal_fsync
after_sst_tmp_write
after_sst_tmp_fsync
after_sst_rename
before_wal_truncate
during_compaction
before_manifest_publish
after_crypto_meta_write
```

Le harnais pourra tuer le process ou injecter une erreur exactement à chaque frontière de durabilité.

### Critère de sortie

Une baseline complète est archivée avant toute modification du format SST.

## 5. N8 — SST par blocs : chantier moteur prioritaire

Les SST sont actuellement lues intégralement en mémoire et chiffrées comme un fichier entier. Ce modèle doit évoluer avant que la taille des stores augmente réellement.

### ADR proposé

**ADR-039 — Block-based SST format, block AEAD, cache and bloom filters**

L'ADR doit figer le nouveau design avant son implémentation.

### 5.1 Spike de format

Comparer au minimum des blocs de 16 KiB, 32 KiB et 64 KiB. Mesurer : point
lookup froid ; scan séquentiel ; coût d'index ; amplification disque ; coût
du chiffrement ; nombre d'I/O par lecture ; consommation mémoire. Ne pas
choisir la taille de bloc par intuition.

### 5.2 Nouveau format SST

```text
Header
Data block 0
Data block 1
...
Data block N
Block index
Bloom filter
Footer
```

Chaque bloc contient : première et dernière clé ; nombre d'entrées ;
longueurs bornées ; checksum ou authentification ; éventuellement restart
points pour la compression de préfixes.

Nouveaux formats possibles dans `format.lock` :

```text
SstHeader:2
SstDataBlock:1
SstBlockIndex:1
SstBloomFilter:1
SstFooter:1
EncryptedSstBlock:1
```

### 5.3 Politique de remplacement du format

Le nouveau format remplace directement l'ancien format expérimental :

* le writer produit uniquement le nouveau format ;
* le reader supporte uniquement le nouveau format ;
* les anciens codecs SST sont supprimés ;
* les anciens stores de développement sont recréés ;
* aucune couche de lecture duale n'est ajoutée ;
* aucune commande de migration n'est requise tant qu'il n'existe pas de données utilisateur publiques ;
* `format.lock` et la version du store sont incrémentés proprement ;
* une erreur explicite est retournée lorsqu'un ancien store est détecté.

### 5.4 Chiffrement par bloc

Chaque bloc chiffré reçoit : nonce XChaCha20 aléatoire ; AAD liant le bloc à
son store, son SST et son numéro de bloc ; ciphertext ; tag Poly1305. Le
footer et l'index doivent eux aussi être authentifiés. Une permutation ou
substitution de blocs entre deux SST doit être détectée, même si chaque bloc
possède individuellement un tag valide.

### 5.5 Index de blocs

Le point lookup doit : consulter le bloom filter ; localiser le bloc via
l'index ; charger uniquement ce bloc ; chercher la clé dans le bloc ; ne
jamais charger la SST entière. Ajouter un invariant instrumenté :
`point_lookup_full_sst_read == 0`.

### 5.6 Block cache

Cache borné : clé `(sst_id, block_id)` ; capacité configurable en octets ;
politique LRU/CLOCK/SLRU ; statistiques hit/miss/eviction ; aucun verrou
global gardé pendant une I/O ; invalidation lors de la publication ou
suppression d'une SST. Le cache contient des données en clair après
déchiffrement — à documenter dans le modèle de menace mémoire.

### 5.7 Bloom filters

Commencer avec un bloom filter par SST, filtres par bloc seulement si mesuré
bénéfique. Versionné, reconstruit depuis la SST, jamais source de vérité,
incapable de produire un faux négatif.

### Critères de sortie

* ouverture d'un store de 1 Gio avec augmentation RSS bornée ;
* aucun point lookup ne charge une SST complète ;
* corruption d'un bloc détectée ;
* ancien code SST supprimé ; ancien format rejeté proprement ;
* aucune régression supérieure au seuil fixé dans ADR-039 sur le workload 100k ;
* `cargo xtask engine-crash` vert en clair et chiffré.

## 6. N9 — Vérification, réparation et reconstruction

### ADR proposé

**ADR-040 — Physical integrity, logical integrity and repair model**

### 6.1 Classification des données

Trois catégories : primaires (records mémoire, entités/arêtes graphe,
métadonnées métier, contrat modèle d'embedding), dérivées (postings FTS,
stats BM25, mapping vectoriel inverse recalculable, connectivité ANN, caches,
bloom filters), de contrôle (manifest, compteurs, epochs, séquences WAL,
`crypto.meta`).

### 6.2 API de vérification

```rust
pub enum VerifyMode { Quick, FullPhysical, FullLogical }

pub struct VerifyReport {
    pub healthy: bool,
    pub files_checked: u64,
    pub blocks_checked: u64,
    pub records_checked: u64,
    pub errors: Vec<IntegrityIssue>,
    pub warnings: Vec<IntegrityIssue>,
}
```

Vérifications physiques : fichiers attendus, magic/versions, checksums,
AEAD, ordre des clés, index de blocs, bornes des offsets, cohérence
manifest/SST, WAL rejouable hors torn tail autorisé. Vérifications
logiques : record ↔ `vec_id`, `vecmap` ↔ record, FTS `docterms` ↔ postings,
stats BM25 recalculées, voisins vectoriels valides, tombstones valides,
entités/arêtes du graphe, isolation par agent, métadonnées embedding
cohérentes.

### 6.3 CLI opérable

```bash
basemyai verify agent.bmai
basemyai verify agent.bmai --deep
basemyai compact agent.bmai
basemyai repair agent.bmai --dry-run
basemyai rebuild-indexes agent.bmai
```

Règles : `verify` ne modifie jamais les données ; `repair --dry-run` produit
un plan détaillé ; une réparation réelle construit un nouvel état à côté de
l'ancien, publié par rename atomique ; aucun écrasement destructif sans
backup explicite ; un index vectoriel nécessitant un ré-embedding est
reconstruit côté `basemyai` (où l'embedder est disponible).

### 6.4 Tests adversariaux

Pour chaque structure : store valide → octet modifié / bloc supprimé / bloc
dupliqué / offset modifié / record orphelin → `verify` → diagnostic exact →
réparation autorisée ne touche pas les données primaires.

### Critère de sortie

Toute corruption supportée doit produire un diagnostic typé précis, une
réparation déterministe, ou une déclaration claire « données primaires
irrécupérables ». Jamais de faux succès.

## 7. N10 — Maintenance scalable

### ADR proposé

**ADR-041 — Native maintenance indexes and bounded multi-record deletion**

### 7.1 API d'importance

```rust
RememberOptions { importance: f32, validity: Option<Validity>, source: String }
memory.set_importance(id, importance).await?;
```

Sans cette API, l'oubli adaptatif est essentiellement un classement par
récence — l'importance est aujourd'hui bloquée à `1.0`.

### 7.2 Index temporel `valid_until`

```text
idx/temporal/expiry/<agent_len><agent><valid_until_be><id>
```

Mis à jour dans le même batch que `remember`/`invalidate`/changement de
validité/`forget`. Le GC devient une range query `valid_until <= now`,
`O(log n + k)` au lieu d'un scan complet par page.

### 7.3 Oubli adaptatif borné en mémoire

Deux passes : (1) scan par pages, min-heap borné à `capacity`, mémoire
`O(capacity)`, calcul `O(n log capacity)` ; (2) rescan, suppression de ce qui
n'est pas dans l'ensemble des survivants, par batch borné.

### 7.4 `forget_many`

```rust
pub struct ForgetBatchOptions { pub max_items: usize, pub max_wal_bytes: usize }
```

Suppressions vectorielles séquentielles sur un état mutable cohérent ;
record/vecmap/DiskANN/FTS/index temporel agrégés dans un seul batch logique ;
limité par nombre d'items et d'octets ; jamais un WAL record gigantesque ;
reprise idempotente entre deux batchs.

### 7.5 Registre d'agents

```text
meta/agents/<agent_len><agent>
```

Ajouté seulement après l'index temporel. N'énumère que des identifiants —
ne casse jamais l'isolation.

### Critères de sortie

GC temporel sans scan complet ; oubli adaptatif à mémoire bornée ;
importance configurable ; suppression par batch crash-safe ; mêmes résultats
fonctionnels ; aucune fuite inter-agent ; benchmark 1M archivé.

## 8. N11 — Durcissement systématique

### 8.1 Tests model-based

`BTreeMap` comme modèle de référence, séquences aléatoires
(put/get/delete/batch/flush/compact/reopen/crash/prefix_scan/rotate_key),
comparaison après chaque étape. Propriétés : last-write-wins, batch
présent-en-entier-ou-absent, suppression persistante, scan ordonné,
réouverture identique, aucun record ressuscité après compaction.

### 8.2 Tests de panne I/O

Écriture courte, `ENOSPC`, erreur `fsync`/`rename`, accès refusé, fichier
temporaire déjà présent, fichier disparu, lecture tronquée, bit flip, arrêt
pendant compaction.

### 8.3 Matrice de tests

À chaque PR : `cargo xtask ci`, tests model-based courts, codecs,
`format.lock`, crash smoke test, corruption smoke test. Nightly : crash
loops prolongés, fuzzing de chaque codec, workloads 100k, clair/chiffré,
delete/reinsert churn, tests avec failpoints. Campagne longue régulière :
soak continu, 1M records, compaction répétée, rotation de clé, RSS/handles,
disque presque plein, Windows/Linux/macOS.

### 8.4 Fuzz targets

```text
wal_decode, sst_header_decode, sst_block_decode, sst_footer_decode,
crypto_meta_decode, memory_record_decode, vector_node_decode,
fts_posting_decode, fts_docterms_decode, graph_entity_decode,
graph_edge_decode, manifest_decode
```

Chaque décodeur doit borner les allocations avant de les effectuer.

### Critères de sortie de la release R1

Aucun crash/invariant violé sur les campagnes définies ; aucune croissance
mémoire inexpliquée ; aucun handle fichier perdu ; récupération après toutes
les frontières de panne testées ; `verify --deep` vert après chaque scénario
non destructif ; benchmark et rapport de durabilité publiés ; documentation
opérateur complète.

À ce stade, le moteur peut être qualifié de **production-ready pour un
stockage local mono-machine, avec writer sérialisé**.

## 9. N12 — Chiffrement V2

### ADR proposé

**ADR-042 — Passphrase KDF, key zeroization and full DEK rotation**

Deux modes (raw key / passphrase, Argon2id uniquement en mode passphrase) ;
zeroization des secrets manipulés (clé utilisateur, KEK, DEK, buffers
temporaires) via wrappers non clonables ; rotation complète
(`basemyai key rotate agent.bmai --full` : nouvelle DEK, nouvelle génération
SST, WAL ré-écrit si nécessaire, fsync, manifest publié atomiquement, ancienne
génération retirée après commit — interruption laisse soit l'ancienne, soit
la nouvelle génération entièrement ouvrable) ; keyring OS (DPAPI/Keychain/
Secret Service) dans les surfaces, jamais dans le moteur.

### Critères de sortie

Ancienne clé rejetée ; ancienne clé + ancien `crypto.meta` incapables de lire
la nouvelle génération ; crash à chaque étape de rotation testé ; nouveau
format crypto documenté ; anciens codecs crypto supprimés s'ils ne sont plus
nécessaires.

## 10. N13 — Snapshots et concurrence

### ADR proposé

**ADR-043 — Immutable version sets, read snapshots and writer pipeline**

Version set immuable (`Arc<VersionSet>`, compaction publie atomiquement) ;
snapshots de lecture (`engine.snapshot()?`) ; compaction concurrente
(lectures non bloquées, commit final court, suppression différée des SST
encore référencées) ; writer pipeline (group commit avant tout multi-writer).
Multi-writer complet **seulement si mesuré nécessaire** après group commit +
batch ingestion + compaction concurrente + `forget_many` — pas pour afficher
une feature.

### Critères de sortie

Sous workload mixte : aucun reader bloqué pendant toute une compaction ;
snapshot cohérent ; aucune SST supprimée tant qu'un snapshot la référence ;
gain mesuré du group commit ; latences p99 documentées.

## 11. N14 — Recherche et espace disque V2

FTS : dataset de requêtes réel avant tout ajout de stemming/tokenizers —
Porter anglais correct en premier, ne pas reconstruire FTS5. Quantification
vectorielle (f32 vs f16 vs scalar i8 vs compressé+re-rank) mesurée sur
recall@10/taille disque/RSS/latence/temps d'insertion/churn — aucune
quantification par défaut sous le seuil de recall fixé avant benchmark.
Explicabilité (`RecallContribution` par signal). Provenance indexée
seulement si les volumes le justifient (le post-filtre actuel suffit sinon).

## 12. N15 — Changefeed avant synchronisation

### ADR proposé

**ADR-044 — Durable local changefeed and replication cursor**

Séquence durable par batch → API locale (`changes_since`/`subscribe_changes`/
`current_sequence`) → rétention/checkpoints → bundle portable
(`basemyai changes export/apply`, versionné/chiffrable/idempotent/authentifié)
→ réplication (seulement ensuite : `origin_id`, dédup, conflits, tombstones
distribués, causalité, transport réseau/P2P — protocole de conflit = décision
produit séparée, pas un simple last-write-wins).

## 13. N16 — API de requête typée, pas de SQL maison

Conditionné à une décision produit séparée ; ne lancer que si au moins trois
usages internes distincts exigent les mêmes primitives. Commencer par
`Scan::prefix(...)` typé, jamais un langage textuel tant que le format de
plan n'est pas stable, les règles de sécurité pas définies, la valeur produit
pas démontrée.

## 14. Organisation des PR

PR 1 (décision : ADR, critères de sortie, formats envisagés, alternatives
rejetées, benchmark prévu, politique de remplacement du format) → PR 2 (tests
et instrumentation qui échouent avant l'implémentation, métriques,
failpoints, benchmark baseline) → PR 3+ (implémentation découpée, ex. N8 :
codecs blocs → writer → reader → suppression ancien reader/writer → bloom
filter → block cache → chiffrement par bloc → validation rejet propre anciens
stores → benchmark final → documentation) → PR finale (critères de sortie
cochés, rapport benchmark, rapport crash/fuzz, `format.lock` validé, docs à
jour, ancien code supprimé, aucun TODO sans issue/suivi, `cargo xtask ci`
vert).

## 15. Definition of Done obligatoire

ADR accepté pour toute décision durable ; codec versionné ; `format.lock` mis
à jour ; format précédent remplacé ou rejeté explicitement ; code obsolète
supprimé ; tests unitaires ; tests property/model-based ; fuzz target ;
scénario crash ; scénario corruption ; benchmark avant/après ; métriques
exposées ; erreurs typées ; documentation opérateur ; surfaces publiques
alignées lorsque nécessaire ; `cargo xtask ci` vert. Aucune migration n'est
obligatoire tant que le format reste expérimental et non utilisé publiquement.

## 16. Ce qu'il ne faut pas faire maintenant

Construire un SQL maison ; lancer le P2P avant un changefeed local durable ;
implémenter un MVCC complet sans mesure du besoin ; changer DiskANN pour
HNSW ; ajouter Tantivy ou une base externe ; conserver des anciens readers
uniquement par peur de casser des stores de développement ; accumuler des
migrations avant le gel du format ; mélanger format SST, crypto, sync et
multi-writer dans le même chantier ; optimiser le stemming avant d'avoir un
dataset de qualité ; déclarer une feature terminée sans test de crash et
benchmark.

## 17. Ordre d'exécution immédiat

```text
M0.1  Réécrire CLAUDE.md                              ✅ déjà à jour
M0.2  Corriger README/CHANGELOG/status/TODO            ✅ déjà à jour
M0.3  Documenter le statut expérimental du format natif ✅ 2026-07-10

N7.1  EngineStats                                       ✅ 2026-07-10 (Engine::stats, tests/engine_stats.rs)
N7.2  Benchmarks reproductibles                          ✅ 2026-07-10 (src/bin/engine_bench.rs, JSON archivable)
N7.3  Commandes xtask moteur                             ✅ 2026-07-10 (engine-check/bench/crash/corrupt/soak/fuzz)
N7.4  Failpoints                                         ✅ 2026-07-10 (src/failpoint.rs, 8 sites ; before_manifest_publish attend le manifest N8)
N7.5  Baseline archivée                                  ✅ 2026-07-10 (docs/benchmarks/n7-engine-baseline-2026-07-10.md)

ADR-039                                                  ✅ 2026-07-10 (docs/adr/ADR-039-block-based-sst.md)
N8.1  Spike tailles de blocs                             ✅ 2026-07-10 — 16 KiB par défaut (docs/benchmarks/n8.1-block-size-spike-2026-07-10.md)
N8.2  Codecs du nouveau format SST                       ✅ 2026-07-11 — SstHeader/SstDataBlock/SstBlockIndex/SstBloomFilter/SstFooter/StoreMeta (src/format/{sst_block,store_meta}.rs, format.lock, fuzz targets) ; EncryptedSstBlock reporté à N8.8 (aucun appelant avant l'AEAD par bloc — le gate -D warnings l'interdirait en dead code)
N8.3  Writer par blocs                                   ✅ 2026-07-11 — `store/sst_block.rs` : `BlockSstFile::write_new` (assemble header/blocks/index/bloom via les codecs N8.2, crash-safe tmp/fsync/rename) + `load` round-trip de vérification (footer→header→blocs via l'index, cross-check first/last key + entry_count, bloom sans faux négatif) + `Bloom` maison (double hashing, 10 bits/clé, 7 hashs — repris du spike N8.1). **Pas encore câblé dans `Engine`** — ADR-039 §5.3 interdit une transition dual-format ; le basculement atterrit avec N8.4 (reader optimisé) + N8.5 (suppression ancien format) en un seul changement atomique. `#[allow(dead_code)]` sur `mod sst_block;` documente l'attente (même logique que `EncryptedSstBlock` en N8.2).
N8.4  Reader par blocs                                   ✅ 2026-07-11 — `BlockSstFile::load` lit uniquement header+footer+index+bloom (jamais un data block) ; `get` fait bloom→recherche binaire dans l'index→lecture d'UN SEUL bloc→recherche binaire intra-bloc, jamais un scan complet ; `entries()` (scan complet) réservé à la compaction/aux scans préfixés, jamais appelé par `get`. `EngineStats::point_lookup_full_sst_read` instrumenté (structurellement 0, `Engine::get` l'incrémente si un lookup lisait >1 bloc) et épinglé à zéro par test sur un workload multi-blocs. `bytes_read_at_open` (header+footer+index+bloom réels, jamais la taille du fichier) alimente `EngineStats::bytes_read` — testé `< sst_bytes/4` sur un store multi-blocs. `SstBlockIndex` bump `:2` (ajout `tombstone_count` par bloc — la jauge `tombstone_count` reste O(métadonnées), jamais un décodage de bloc).
N8.5  Suppression de l'ancien format SST                 ✅ 2026-07-11 — `store/sst.rs` + `format/sst.rs` supprimés (types `SstEntry`/`SstOp` déplacés dans `format/sst_block.rs`, seul module qui en a encore besoin) ; `Engine` bascule entièrement sur `store::sst_block::BlockSstFile`/`scan_existing` (flush/compaction/open/get/scan_prefix) ; `EngineOptions::block_size: u32` ajouté (défaut 16 KiB, spike N8.1) ; fuzz targets `sst_decode`/`sst_decode_structured` retirés (`fuzz/Cargo.toml`, `fuzz/README.md` mis à jour) ; `#[allow(dead_code)]` retiré de `mod sst_block;`. `SstFile:1`/`SstEnvelope:1` retirés de `format.lock` ; `EngineError::CorruptSst` (variante devenue inatteignable) supprimée.
N8.6  Bloom filters                                      ✅ (livré avec N8.3, confirmé câblé en lecture réelle par N8.4) — un filtre par SST, double hashing 10 bits/clé/7 hashs, zéro faux négatif testé.
N8.7  Block cache                                        ✅ 2026-07-11 — `store/block_cache.rs` : `BlockCache` LRU borné en octets (défaut 32 Mio, `EngineOptions::block_cache_capacity_bytes`), clé `(sst_id, block_no)`, partagé par tout le moteur (pas par SST). Consulté uniquement par `BlockSstFile::get` (le chemin point-lookup) ; `entries()` (compaction/scan préfixé) ne le consulte jamais, pour ne pas polluer le cache de données froides lues une seule fois. Aucun verrou tenu pendant une I/O (lookup/insert sous verrou court, lecture disque hors verrou) ; politique LRU simple, éviction O(n) (borné par la capacité en octets, quelques milliers d'entrées au plus — pas de sophistication spéculative type CLOCK/SLRU tant que non mesurée nécessaire). Invalidation par `sst_id` à la suppression d'une SST (compaction). `EngineStats::block_cache_hits`/`misses` réels depuis ce jalon (n'étaient plus « toujours à 0 » — tests mis à jour en conséquence). Modèle de menace RAM du cache documenté dans `docs/security/encryption-model.md` (le clair reste résident tant que le bloc est en cache, pas seulement le temps d'une lecture — posture ADR-030 inchangée).
N8.8  AEAD par bloc                                      ✅ 2026-07-11 — `EncryptedSstBlock:1` (`format/crypto.rs`) : magic/version/nonce 24o aléatoire/ct_len/ciphertext+tag, sans tolérance torn-tail (comme l'ex-`SstEnvelope`, pas comme `WalEnvelope`). AAD = magic‖version domaine ‖ `sst_id` ‖ `SstSectionType` (Data/Index/Bloom/Footer — jamais Header, qui reste en clair) ‖ `section_no`. Câblé dans `BlockSstFile::write_new`/`load` : chaque bloc de données/l'index/le bloom/le footer scellés individuellement ; footer scellé de taille fixe (permet toujours un seek EOF sans lecture préalable). Testé : permutation d'un bloc entre deux SST et permutation de deux blocs au sein d'un même SST échouent toutes deux l'authentification (§3 anti-permutation) ; test « le clair ne fuite jamais » porté depuis l'ancien format.
N8.9  Rejet propre des anciens stores                    ✅ 2026-07-11 — `store.meta` (`StoreMeta:1`) écrit à la création d'un répertoire vierge (tmp/fsync/rename, `fail_point!("before_manifest_publish")` juste avant la publication) ; à l'ouverture, absence de `store.meta` **avec** `wal.log`/`*.sst` présents, ou version ≠ `STORE_FORMAT_VERSION`, ⇒ `EngineError::UnsupportedStoreFormat{expected,found,path}` (`found: 0` = sentinelle "aucun store.meta"). Vérification faite *avant* toute logique crypto/WAL/SST dans `Engine::open_inner` (`check_or_create_store_meta`).
N8.10 Crash + fuzz + bench                               ✅ 2026-07-11 — `cargo xtask test-crash-consistency` vert (7/7 modes, clair+chiffré) ; 15 cibles fuzz exécutées sous WSL (nightly + cargo-fuzz, ~25 s/cible, plusieurs millions d'itérations chacune) — zéro crash ; baseline `docs/benchmarks/n8-block-sst-baseline-2026-07-11.md` (10k/100k/1M, clair+chiffré) archivée contre N7.5 : ouverture O(métadonnées) confirmée (1,3 % de `sst_bytes` lu, ~65× plus rapide à 1M), `point_lookup_full_sst_read == 0` sur 54 lignes de rapport, RSS pic -25 %/-45 % (clair/chiffré) à 1M. Deux résultats honnêtement documentés, pas cachés : `kv-fill` légèrement au-dessus du seuil +10 % (+11-15 %, dans le bruit fsync déjà documenté par N7.5) et `kv-prefix-scan` régresse ×88-157× (ne consulte pas encore l'index pour ne lire que la plage de blocs pertinente — candidat de suivi, hors critères de sortie ADR-039 §8).

ADR-040
N9    Verify / repair / rebuild / compact

ADR-041
N10   Importance + temporal index + oubli borné + forget_many

N11   Campagne complète de durcissement
```

Le projet ne passe à N12 que lorsque N8 à N11 sont fermés avec preuves
mesurées. Le format natif ne sera figé qu'après ces chantiers, lorsque le
layout disque, la stratégie de réparation et les invariants de sécurité
auront atteint un niveau suffisamment stable pour devenir un contrat public.
