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

N8.11 Suivi : scan préfixé par l'index de blocs          ✅ 2026-07-11 — `BlockSstFile::entries_with_prefix` (partition_point sur `last_key` → décodage des seuls blocs chevauchant la plage du préfixe, arrêt au premier `first_key` hors plage), branché dans `Engine::scan_prefix`. Corrige la régression ×88-157 documentée par N8.10 : `kv-prefix-scan` 1M passe de 360,84 ms → 1,47 ms clair (2,8× plus rapide que N7.5) et 447 ms → 1,74 ms chiffré. Aucun changement de format (format.lock inchangé), cache de blocs toujours réservé aux point lookups (N8.7). Épinglé par tests de comptage de blocs décodés. Addendum dans docs/benchmarks/n8-block-sst-baseline-2026-07-11.md. La compaction reste sur `entries()` (doit voir toutes les clés).

ADR-040                                                  ✅ 2026-07-11 (docs/adr/ADR-040-integrity-and-repair.md — classification primaires/dérivées/contrôle, modes Quick/FullPhysical/FullLogical, règles verify read-only/jamais de faux succès/diagnostic typé, modèle de réparation rename-atomique, phasage N9.1→N9.6)
N9.1  ADR-040                                            ✅ 2026-07-11 — voir ci-dessus.
N9.2  verify moteur (Quick + FullPhysical)               ✅ 2026-07-11 — `store/verify.rs` : `verify_store(dir, key, mode)` → `VerifyReport{healthy, files/blocks/records_checked, errors, warnings}`, anomalies typées `IntegrityIssue{IssueKind, path, detail}` (18 kinds, `#[non_exhaustive]`). `Quick` = O(métadonnées) (store.meta/crypto.meta, header/footer/index/bloom par SST, bornes+contiguïté des offsets de blocs, ordre des clés inter-blocs via l'index, scan WAL structurel) ; `FullPhysical` = + décodage de chaque data block par le VRAI chemin de lecture (`read_and_verify_block`), ordre strict intra-bloc, `tombstone_count` bloc↔index, zéro faux négatif bloom sur les clés réelles. **Strictement read-only** : nouveau `store::wal::scan_readonly` (décodeur partagé avec `replay` via fn libre `decode_next`, mais sans la troncature de queue déchirée que `open` s'autorise) — queue WAL déchirée et orphelins `*.tmp` = warnings, pas erreurs (états post-crash attendus). Mauvaise clé/clé manquante/clé sur store en clair = `Err` typée de l'appel, jamais une entrée du rapport (ADR-040 §2 r.4). `VerifyMode` sans `FullLogical` pour l'instant (un mode menteur n'est pas exposé — N9.3). 12 tests adversariaux : octet modifié dans un data block (invisible en Quick PAR CONSTRUCTION, diagnostiqué en FullPhysical, clair et chiffré), SST tronqué, index tamper (Quick le voit), WAL record complet corrompu = erreur vs queue déchirée = warning, invariant « verify ne modifie jamais un octet » épinglé par snapshot intégral du répertoire, store pré-ADR-039, répertoire vide sain. format.lock inchangé (aucun changement on-disk).
N9.3  verify logique (FullLogical)                       ✅ 2026-07-12 — `store/verify_logical.rs` + `VerifyMode::FullLogical`. Vue KV fusionnée construite sans jamais ouvrir le store en écriture : blocs décodés par la passe physique (SST oldest→newest) + overlay des records WAL (`scan_readonly` porte désormais les records, pas juste le compte), tombstones éliminés — exactement la vue vivante qu'un `open` servirait. Parse générique des 4 keyspaces `idx/` (wire-distrust : longueurs bornées avant tout slice ; clé étrangère/malformée dans un keyspace réservé = `IdxKeyMalformed`, valeur indécodable = `IdxValueCorrupt`). Règle erreur/warning unique documentée : une incohérence que le moteur guérit automatiquement (méta vectorielle rebuilt à l'open, stats BM25 manquantes, allocateur absent) = warning ; tout ce que le moteur croirait tel quel = erreur (`AllocatorStale` décodable-mais-périmé ⇒ réutilisation d'id, stats BM25 intactes-mais-fausses ⇒ scores faussés silencieusement, liens record↔vecmap↔node cassés, `AgentIsolationBreach` cross-structure via vecmap). Garde de périmètre : les checks de liaison mémoire ne tournent que si `idx/memory/` est peuplé (index vectoriel standalone légitime — le moteur est un mécanisme). Arêtes graphe pendantes = warning (l'API n'a jamais imposé l'existence des entités). Passe logique sautée avec warning explicite `LogicalChecksSkipped` si erreurs physiques (pas de diagnostics secondaires en cascade — testé). 12 tests adversariaux sur store composé réel (PersistentMemoryIndex+Vector+Fts+Graph, tombstones de forget + tail WAL non flushée = sains). format.lock intact.
N9.4  Compaction opérable                                ✅ 2026-07-11 — `Engine::compact_now()` public : flush puis full-merge inconditionnel (même sous `compaction_sst_threshold`) — le point d'entrée moteur du futur `basemyai compact` (N9.6). Utile même à 1 SST (réécriture sans tombstones, sûr car le merge couvre toutes les données). No-op sur store vide. Testé chiffré : multi-SST + tail non flushé → 1 SST, `tombstone_count == 0`, `wal_bytes == 0`, données intactes après reopen.
N9.5  repair --dry-run + rebuild-indexes                 ✅ 2026-07-12 — socle moteur : `store/repair.rs` expose `plan_repair(&VerifyReport)` (pur, strictement sans I/O/écriture) et `rebuild_indexes(&mut Engine)`. Ne sont réécrits que `idx/memory/vecmap` + allocateur, `idx/fts/` et la connectivité/méta DiskANN ; records mémoire et graphe ne sont jamais touchés. Les vecteurs absents restent explicitement listés dans `RebuildReport::reembedding_required` : l'engine n'invente aucun embedding. Tests : plan sans écriture ; corruption dérivée restaurée avec snapshot des primaires identique. format.lock intact. Câblage conteneur (`basemyai/src/storage/integrity.rs`, chemin `.bmai` + clé plutôt qu'un `Engine` déjà ouvert) livré avec N9.6.
N9.6  Surface CLI verify/compact/repair/rebuild-indexes/reembed  ✅ 2026-07-12 — `basemyai-cli` : `verify [--physical|--logical]` fusionne l'ancien check de métadonnées de conteneur (format/version/storage_engine) avec l'audit moteur (`Quick` par défaut, `FullPhysical`/`FullLogical` sur flag) — l'audit moteur tourne *avant* l'ouverture normale du store (qui recouvre une queue WAL déchirée, effaçant l'anomalie qu'un `Quick` doit révéler). `repair [--dry-run]` audite en `FullLogical`, calcule le plan, et sans `--dry-run` applique `rebuild-indexes` seulement si `RepairPlan::can_apply_derived_only()` — sinon refuse (nouvel exit code `REPAIR_REFUSED = 11`, jamais de réparation automatique de données primaires). `rebuild-indexes` : application inconditionnelle, sans audit préalable (pour un opérateur qui sait déjà quoi corriger). `compact` : `Engine::compact_now`, rapport `EngineStats` avant/après. Sortie JSON via le `--format json` global existant (pas de flag `--json` dédié — redondant). 4 tests d'intégration CLI (verify physical/logical sain, repair --dry-run no-op, rebuild-indexes no-op, compact stats).
      Ré-embedding livré le même jour : `basemyai::storage::integrity::{reembed_missing_container, reembed_ids_container, reembed_all_container}` + fonction partagée `reembed_targets` — recompute le vecteur au `vec_id` *existant* du souvenir (jamais un nouvel id alloué, jamais touché record/vecmap/FTS), via `PersistentVectorIndex::delete` (idempotent, no-op si déjà non-vivant) puis `insert` — le même schéma "update = delete + reinsert" que le moteur documente déjà, ce qui couvre uniformément le vecteur structurellement absent (`reembedding_required`) ET le vecteur vivant qu'on veut recalculer (changement de modèle), sans branche séparée. `basemyai reembed` (sans flag) relance `rebuild_indexes` pour une liste `reembedding_required` à jour puis la corrige, portée = tout conteneur/tous agents (la liste du moteur n'est déjà pas scopée par agent). `basemyai reembed --agent X --ids a,b` / `--all` réembed sans condition, portée = un agent (`--all` énumère `idx/memory/rec/<agent>` via `memory_index::record_agent_prefix`, déjà public côté moteur — pas de nouvelle primitive nécessaire). Un id demandé disparu entre la liste et l'exécution atterrit dans `missing`, jamais une erreur. Seule commande d'intégrité qui charge l'embedder Candle (comme `remember`/`recall`) — donc **hors de la suite `assert_cmd` gatée CI** (modèle indisponible hors-ligne en CI), vérifiée manuellement de bout en bout à la place sur le modèle local réellement provisionné sur cette machine : no-op sain sur store healthy, réembed ciblé (`--ids`) et en masse (`--all`) sur des vecteurs déjà vivants, `verify --logical` reste sain après coup, `recall` retrouve toujours la bonne mémoire (similarité cohérente) après réembed, id inconnu → `missing` pas une erreur, `--all --ids` ensemble → rejeté par clap (exit 2, erreur d'usage).
N9 (ADR-040) entièrement clos 2026-07-12 — N9.1 à N9.6, verify/repair/rebuild-indexes/compact/reembed tous livrés moteur + basemyai + CLI.

N11.1 Fuzz targets — couverture complète des décodeurs  ✅ 2026-07-12 — audit exhaustif de chaque `pub fn decode`/`decode_*` de `basemyai-engine` (15 avant ce chantier, `format::{sst_block,store_meta,wal}` + `idx::{vector,graph}`) contre la liste cible §8.4 : 9 décodeurs sans cible fuzz identifiés par grep direct (pas par supposition) — les trois de chiffrement au repos (`format::crypto::decode_{crypto_meta,wal_envelope,encrypted_sst_block}`, ADR-030/ADR-039 §3) et six décodeurs `idx::{fts,memory}` (`docterms`, `postings`, `stats`, `memory::meta` l'allocateur, **`memory::record` — la donnée primaire elle-même**, `memory::vecmap`). 9 nouvelles cibles ajoutées, 24 au total, une par décodeur du crate sans exception. Point de conception : les trois décodeurs crypto restent `pub(crate)` (leurs types `CryptoMeta`/`Nonce`/`WalEnvelopeRef` sont délibérément privés au crate, ADR-030) — plutôt que les rendre `pub` et faire fuir ces types dans l'API publique (`private_interfaces`), trois wrappers `pub fn fuzz_decode_*` minces (juste `let _ = decode(...)`) ont été ajoutés dans `format/crypto.rs`, gardant la même garantie (zéro panic, zéro UB) sans élargir la surface publique du crate. `format::crypto` (le module) passe de `pub(crate)` à `pub` (nécessaire pour que le crate `fuzz/` externe atteigne ces wrappers) ; `cargo xtask check`/clippy `-D warnings` (incluant `unreachable_pub`) revérifiés verts après ce changement.
      **Exécution réelle faite le même jour, sous WSL/Kali** : `cargo-fuzz` ne tourne pas nativement sous Windows (runtime ASan absent pour `x86_64-pc-windows-msvc`, confirmé en session précédente — `STATUS_DLL_NOT_FOUND` au lancement malgré un link propre) — bascule vers une distribution WSL fraîchement réinstallée. Toolchain provisionné sur cette machine : `rustup` (nightly, profil minimal), `build-essential`/`clang`/`llvm` (apt), `cargo-fuzz`. Les 24 cibles ont tourné pour de vrai (`-max_total_time=30` chacune, plusieurs millions à dizaines de millions d'exécutions par cible selon la taille du corpus), **zéro crash, zéro panic, zéro timeout** sur les 24 — `exit=0` confirmé pour chacune, y compris les 9 nouvelles (crypto ×3, fts ×3, memory ×3) et re-confirmation des 15 déjà existantes. N11.1 entièrement clos : couverture posée ET exécutée.

N11.2 Tests model-based                                  ✅ 2026-07-13 — `tests/model_based.rs` : `BTreeMap<Vec<u8>, Vec<u8>>` comme modèle de référence, PRNG maison xorshift64* (même construction que `src/harness.rs`, aucune dépendance `rand`/`proptest` — convention du workspace), séquences pondérées bornées (put/get/delete/batch/flush/compact/reopen/crash/prefix_scan/rotate_key) rejouées contre l'`Engine` réel, clair et chiffré. Chaque propriété de §8.1 est épinglée par une assertion attachée à l'opération qui l'exerce (pas juste "ça ne panique pas") : last-write-wins (`get` immédiat après `put`), suppression persistante (`get` immédiat après `delete`), batch tout-ou-rien côté succès (chaque op stagée revérifiée individuellement après un `apply_batch` réussi — le versant panne-pendant-l'écriture reste le rôle de `failpoints.rs`/`crash_consistency.rs`, volontairement hors périmètre ici), aucun record ressuscité après compaction (`ever_deleted` revérifié après chaque `compact_now`), scan ordonné et réouverture identique (`scan_prefix(b"")` comparé élément par élément au modèle après `flush`/`compact_now`/reopen gracieux (`close`+réouverture)/arrêt sale (`drop` sans `close`+réouverture) — ce dernier exerce le vrai replay WAL sur un état durablement fsync, pas une écriture torn, qui reste le rôle du kill-loop). 7 seeds clair + 3 seeds chiffrés, ~140/100 opérations chacun, tourne en ~2s (gate de PR §8.3, pas la campagne nightly).

N11.3 Tests de panne I/O                                 ✅ 2026-07-13 — `tests/io_faults.rs` : la majorité de §8.2 était déjà couverte (écriture courte/`ENOSPC`/erreur `fsync`/`rename`/arrêt pendant compaction via `failpoints.rs`, `Action::Abort` via `crash_consistency.rs`, bit flip/lecture tronquée via `corruption_smoke.rs`) — ce chantier ferme les deux scénarios qui ne l'étaient pas. **Accès refusé** : le fichier tmp cible (`*.sst.tmp`/`crypto.meta.tmp`) rendu lecture-seule (`std::fs::Permissions::set_readonly`, portable clair/chiffré) juste avant que le moteur écrive dessus — `flush()`/`rotate_key()` échouent typé (`EngineError::Io`), `next_sst_id` n'avance jamais sur un échec donc un retry après levée de l'obstruction réutilise le même id proprement ; `rotate_key` ne mute jamais la DEK en mémoire (seulement son wrap sur disque) donc l'instance reste utilisable et l'ancienne clé rouvre toujours tant que `crypto.meta` n'a pas été remplacé. **Fichier temporaire déjà présent** : un tmp périmé/garbage pré-existe au chemin exact que le moteur va écrire (crash antérieur) — `BlockSstFile::write_new`/`crypto::write_meta` ouvrent en `create(true).write(true).truncate(true)` (jamais `create_new`), donc l'écrasement est propre par construction ; testé avec un payload garbage plus gros que la charge réelle pour attraper une troncature partielle. Corrige aussi en passant le commentaire pré-N9 obsolète de `corruption_smoke.rs::deleted_sst_is_currently_silent_data_loss_known_n9_gap` (renommé `..._no_manifest_yet`) : vérifié empiriquement (nouveau test `verify_full_logical_does_not_catch_a_deleted_sst_either`) que `verify_store` en mode `FullLogical` — le plus profond — ne détecte pas non plus une SST vivante supprimée, faute de manifest indépendant listant les SSTs attendues ; le gap n'est pas fermé par N9, un manifest/version-set (candidat naturel : N13/ADR-043) reste nécessaire.

§8.3 Matrice de tests                                     ✅ 2026-07-13 (partiel, voir suivi) — trois bugs concrets trouvés et corrigés en creusant la CI existante avant d'écrire quoi que ce soit de neuf : (1) le job `gate` de `ci.yml` liste ses fichiers `--test` un par un (pas de découverte automatique) et n'incluait ni `model_based` ni `io_faults` — corrigé, les deux tournent maintenant à chaque PR ; (2) `fuzz.yml` (nightly, cron existant depuis N7) référençait une cible `sst_decode_structured` supprimée en N8.5 (aurait cassé le job) et ne couvrait que 5 des 24 cibles réelles — matrice réécrite à l'identique de `fuzz/Cargo.toml`, une entrée par cible, zéro exception ; (3) `cargo xtask engine-soak` (N7.3) documenté "usage nightly" depuis le début mais jamais câblé dans aucun workflow. **Nouveau `nightly.yml`** : `crash_consistency` avec `BASEMYAI_CRASH_CYCLES` (nouvelle variable d'env, défaut 20 inchangé — testé que le gate de PR n'est pas affecté) porté à 200 cycles par défaut (au lieu de refaire un second fichier de test) + `engine_bench` à 100k clair+chiffré (`cargo xtask engine-bench`, qui fait déjà les deux automatiquement) archivé en artefact CI. **Nouveau `soak-campaign.yml`** (hebdomadaire) : `cargo xtask engine-soak` sur Linux/Windows/**macOS** (macOS volontairement absent du gate et du nightly pour le coût ×10 documenté dans `ci.yml`, acceptable à cadence hebdomadaire) — chaque cycle exerce déjà flush/compaction répétées et clair+chiffré par construction (`engine_bench` sous le capot), `workflow_dispatch` permet un run 1M à la demande (`n=1000000`) sans payer ce coût par défaut sur le cron. **Volontairement non fait, pas de fausse promesse** : rotation de clé pendant un soak, simulation de disque presque plein, comptage de handles — aucun des trois n'a d'instrumentation moteur/xtask existante à réutiliser (contrairement à RSS, déjà dans `EngineStats`/`engine_bench`) ; ce sont des chantiers de code séparés (nouveaux modes `engine-soak`/`EngineStats`), pas de la config CI, donc hors périmètre de cette passe.

ADR-041                                                  ✅ 2026-07-13 (docs/adr/ADR-041-native-maintenance-indexes.md — importance, index temporel, maintenance bornée ; complété au fil des livraisons, §7.1→§7.5 tous livrés)
N10   Importance + temporal index + oubli borné + forget_many
      §7.1 API d'importance                              ✅ 2026-07-13 — `Memory::remember_with_importance`/`set_importance`, `MemoryError::InvalidImportance` (NaN/infini rejetés, négatif accepté), champ `importance` sur `NewMemory`/`put_memory` (`DEFAULT_IMPORTANCE = 1.0`). Aucun changement de format (champ réservé depuis N5.1).
      §7.2 Index temporel + Engine::scan_range           ✅ 2026-07-13 — primitive moteur d'abord (`BlockSstFile::entries_with_range` + `Engine::scan_range`, vrai range-scan à deux bornes avec saut de blocs), puis keyspace réservé `idx/temporal/expiry/` (`key::temporal_index`, timestamps signés sortables par inversion du bit de signe, valeur vide — rien au format.lock), maintenu dans le même batch atomique que put/invalidate/set_importance/forget/purge. `scan_expired` (contrat public inchangé) passe du scan complet par agent à la requête bornée — zéro décodage de `MemoryRecord`.
      §7.3 Oubli adaptatif borné en mémoire              ✅ 2026-07-13 — même discipline « primitive moteur d'abord » : `BlockSstFile::entries_with_range_limited` (arrêt à `limit` matches, flag `truncated`) + `Engine::scan_range_page` (fusion LSM par frontière = min des dernières clés des sources tronquées ; protocole `next_start`, une page vide peut être une progression) + `key::memory_index::record_agent_upper_bound` + `PersistentMemoryIndex::scan_agent_page` (contrat curseur-id simple, page courte ⇔ épuisé). Contrat `MemoryStore::scan_for_forgetting` paginé (breaking 0.2.0 : `after_id`/`limit`, pages de candidats actifs). `adaptive_forgetting` en deux passes : sélection par `SurvivorSelector` (tas borné à `capacity`, O(capacity + page) mémoire, résultat indépendant de l'ordre des pages) puis éviction paginée de tout non-survivant au même `now` (prédicat gelé), victime par victime (le lot = §7.4). dry-run sans mutation après la passe 1. Fenêtre inter-passes documentée (ADR-041). Tests : SST/engine (pages chaînées ≡ scan complet, frontière memtable vs clé masquante, plage tombstonée = progression), stub scripté deux passes à page minuscule, contrat store paginé sur population mixte actifs/invalidés/expirés, 19 scénarios maintenance_worker verts.
      §7.4 forget_many                                   ✅ 2026-07-13 — suppression par lots atomiques bornés. Primitives par index d'abord : `PersistentFts::stage_delete_many` (une seule écriture de stats agrégée pour le groupe — le read-modify-write de `stage_delete` perdrait des décréments dans un batch partagé, testé) + `delete_footprint` (sonde d'octets), `PersistentVectorIndex::delete_many_with` (toutes les tombstones + UN méta, même asymétrie extra-sur-no-op que `delete_with`), `Batch::approx_wire_bytes`. Puis `PersistentMemoryIndex::forget_many(…, ForgetBatchOptions{max_items: 256, max_wal_bytes: ~4 Mio})` : chunks atomiques (record+vecmap+expiry+FTS+tombstones = un WAL record), comptabilité d'octets estimative (cible de dimensionnement, un souvenir n'est jamais scindé), reprise idempotente entre chunks (ids absents sautés). Surface : `MemoryStore::forget_many` (breaking 0.2.0), câblé dans les 4 chemins d'éviction — GC + oubli adaptatif, CLI (`run`) et façade `Memory` (`forget_batch_with_events` : couches capturées avant, événements `Forgotten` après commit, contrat de `Memory::forget` préservé). Tests à chaque couche (FTS stats agrégées, groupe vectoriel atomique + reopen sans rebuild, résultat indépendant des bornes de chunk, idempotence, isolation inter-agent au niveau store).
      §7.5 Registre d'agents                             ✅ 2026-07-13 — `meta/agents/<agent_len><agent>` (`key::agent_registry`, valeur vide — rien au format.lock). Marqueur empilé dans le même batch que chaque `put_many` (écrasement idempotent) ; retiré par `purge_agent` EN DERNIER (crash mid-purge ⇒ relancer retire l'entrée — jamais d'agent non purgé invisible) ; volontairement PAS retiré au forget du dernier souvenir (le registre = « qui visiter », jamais un compteur). `PersistentMemoryIndex::list_agents` + `NativeMemoryStore::list_agents` (méthode inhérente conteneur, pas sur le trait — même statut que `total_memory_count`). Identifiants seuls, zéro fuite inter-agent. Limite documentée (ADR-041) : non rétroactif sur les stores pré-N10 (acceptable, format non publié).
N10 (ADR-041) entièrement clos 2026-07-13 — §7.1 à §7.5 livrés ; benchmark 1M archivé le 2026-07-15 avec la campagne N11 (voir plus bas), plus de suivi ouvert.

N11   Campagne complète de durcissement

N11.4 Campagne soak 1M réellement exécutée et archivée ✅ 2026-07-15 — le
      "reste de suivi" laissé par N10/§8.3 (`soak-campaign.yml` posé mais
      jamais réellement déclenché à `n=1000000`) est maintenant fait pour de
      vrai, pas seulement câblé. **4 cycles** à `n=1 000 000`, clair **et**
      chiffré à chaque cycle (8 invocations `engine_bench`, dépasse le
      minimum de 3-5 cycles du plan), plus `cargo xtask
      test-crash-consistency` (7 variantes × 20 cycles kill réel) dans la
      même session. Nouveau flag `--verify` ajouté à `engine_bench`
      (`crates/basemyai-engine/src/bin/engine_bench.rs::verify_dir_if_requested`)
      qui rouvre chaque store juste après sa fermeture propre (avant
      suppression du répertoire temporaire) et appelle
      `basemyai_engine::verify_store(dir, key, VerifyMode::FullLogical)` —
      `FullLogical` est le mode le plus profond réellement nommé ainsi dans
      le code/CLI (`--logical`) ; le "`--deep`" de ce plan (§8, critères de
      sortie R1) est ce même mode, pas un flag distinct qui n'existe pas.
      **Résultat : 14/14 audits `healthy=true`, 0 erreur, 0 warning** ; RSS
      peak stable 553,5-561,7 Mo sur les 8 runs sans tendance monotone
      cycle-à-cycle ; `sst_bytes`/`wal_bytes`/`tombstone_count`/
      `compaction_count`/`block_cache_hits`/`misses` identiques bit-pour-bit
      entre les 4 cycles clairs entre eux et les 4 cycles chiffrés entre eux
      (workload déterministe — reproductibilité parfaite, donc toute
      divergence future serait un signal fort) ; `test-crash-consistency`
      7/7 vert (140 cycles kill réel cumulés, single-key/batch/graph/
      memory/vector, clair+chiffré). **Aucun bug trouvé.** Latences
      (`open-large-store`, `kv-fill`) plus bruitées que N7.5/N8 — la
      campagne a tourné en tâche de fond pendant d'autres commandes cargo
      sur la même machine — documenté honnêtement dans le rapport, sans
      impact sur les métriques structurelles (déterministes, indépendantes
      du CPU disponible). Seul critère de sortie R1 non vérifié
      positivement : comptage de handles fichier (aucune instrumentation
      moteur/xtask existante, comme déjà documenté par §8.3 — pas de fausse
      promesse ici non plus). Rapport complet :
      `docs/benchmarks/n11-soak-1m-2026-07-15.md` ; données brutes
      `docs/benchmarks/data/n11-soak-1m/` (8 rapports JSON + `campaign.log`).

N11 entièrement clos 2026-07-15 — N11.1 (fuzz, 2026-07-12), N11.2
(model-based, 2026-07-13), N11.3 (pannes I/O, 2026-07-13), §8.3 (matrice
CI, 2026-07-13) et N11.4 (campagne 1M, 2026-07-15) tous livrés avec preuve
mesurée. Les critères de sortie R1 (§8, ci-dessus) sont couverts à
l'exception du comptage de handles fichier (non instrumenté, documenté).
Documentation opérateur (`docs/cli.md`) revue dans cette session : déjà
complète (verify/repair/rebuild-indexes/compact/reembed, codes de sortie,
ce qui est read-only vs ce qui écrit), aucune lacune trouvée qui aurait
justifié une réécriture.
```

Le projet ne passe à N12 que lorsque N8 à N11 sont fermés avec preuves
mesurées. Le format natif ne sera figé qu'après ces chantiers, lorsque le
layout disque, la stratégie de réparation et les invariants de sécurité
auront atteint un niveau suffisamment stable pour devenir un contrat public.
