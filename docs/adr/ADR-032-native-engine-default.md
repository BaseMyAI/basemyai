# ADR-032 — Bascule du défaut : le moteur natif remplace libSQL comme backend par défaut

**Statut** : ✅ Accepted
**Date** : 2026-07-08
**Relation aux ADR existants** : clôt N5.6, le dernier jalon de la Phase 4 du
chantier acté par ADR-024. S'appuie sur ADR-025 (fondation LSM), ADR-026
(index vectoriel LM-DiskANN), ADR-027 (`MemoryStore` sur Native), ADR-028
(FTS/BM25 natif) et ADR-030 (chiffrement au repos natif). **Amende ADR-011
et ADR-019 sur un point précis** : libSQL cesse d'être le backend par défaut ;
il reste le backend de **compatibilité** pour les fichiers `.bmai` v1
existants. ADR-007 (chiffrement obligatoire) et ADR-005/006/012 (sémantiques)
sont inchangés et s'appliquent à l'identique au backend natif.

## Contexte

ADR-024 posait la condition de bascule : « libSQL reste le défaut jusqu'à
parité prouvée ». ADR-027 §1 en faisait un jalon dédié (N5.6), « décision
séparée et humaine, chiffres à l'appui, jamais prise en passant ». La
décision a été prise explicitement par le propriétaire du projet le
2026-07-08. Cet ADR l'instruit.

### Les chiffres (tous archivés, mesurés sur la même machine que M6)

| Critère | libSQL (mesuré M6) | Natif (mesuré N3/N5.5) | Source |
|---|---|---|---|
| Requête KNN k=10, N=10k (index nu) | ~48-49 ms | **7.52 ms** (6,5×) | `docs/benchmarks/n3-vector-parity-2026-07-05.md` |
| Requête KNN k=10, N=100k (index nu) | ~48-49 ms | **12.67 ms** (3,8×) | idem |
| Build incrémental d'index, /ligne | ~78-79 ms (bulk-load ; l'incrémental 100k n'a jamais fini en 3h+) | **5.69 ms** (10k) / **17.32 ms** (100k) | idem |
| `recall_vector` bout-en-bout (chemin `MemoryStore` complet, N=10k) | ~48.98 ms (`vector_top_k` **nu**, sans hydratation) | **17.03 ms** (oversampling ×8 + hydratation + filtre + `touch` inclus, ~2,9×) | `docs/benchmarks/n5.5-memorystore-knn-bench-2026-07-07.md` |
| Recall@10 | jamais mesuré par M6 | **1.0000** (10k et 100k, y compris après churn 20 % delete/réinsert ×3) | ADR-026 §6, `n3-vector-parity` |
| Lectures concurrentes (64 lectures mixtes) | pool lecteur ADR-021 | **~3× plus rapide que séquentiel** (RwLock N5.5) | `tests/memory_tests.rs` |

### La parité (prouvée, pas affirmée)

- **19 scénarios `backend_suite!`** — 100 % de `storage_contract.rs` — rejoués
  verbatim contre `Libsql`, `Native` **et** `NativeEncrypted` : zéro
  divergence (N5.3/N5.4).
- **Crash-consistency sous kill réel** : 5 modes (base/batch/vector/graph/
  encrypted_batch) + mode `memory` (triplet record+vecteur+FTS, clair et
  chiffré), 20 cycles chacun, **0 violation** (N2→N5.5).
- **Chiffrement au repos** (ADR-030) : AEAD XChaCha20-Poly1305 pur Rust,
  enveloppe DEK/KEK, rotation de clé O(1) **sans réouverture** — mieux que le
  `PRAGMA rekey` libSQL, et **sans CMake** (le talon d'Achille DX de la
  feature `crypto` libSQL).
- **`EngineCapabilities::native()`** : toutes les capacités à `true`
  honnêtement (`vectors`, `full_text`, `recursive_queries`, `transactions`,
  `encrypted`).

## Décision

**Le moteur natif (`basemyai-engine`) devient le backend par défaut de
BaseMyAI pour toute nouvelle base, de façon définitive.** libSQL passe en
backend de **compatibilité** : les fichiers `.bmai` v1 existants continuent de
s'ouvrir en lecture/écriture, mais aucune surface ne crée plus de base libSQL
par défaut. Le retrait complet de libSQL est une décision future séparée (il
exigera son propre ADR) ; rien dans le présent ADR ne casse un store existant.

### 1. Format sur disque : `.bmai` v2 = répertoire du moteur natif

- **`.bmai` v1** (ADR-019) : un fichier unique SQLite/libSQL. Inchangé.
- **`.bmai` v2** : un **répertoire** portant l'extension `.bmai` (ex.
  `agent.bmai/`), contenant WAL, SST et méta du moteur natif (`crypto.meta`
  si chiffré). Le versionnage de wire reste gouverné par `format.lock`.
- **Détection** : structurelle et non ambiguë — un chemin existant qui est un
  **répertoire** est un store natif ; un **fichier** est un store libSQL v1
  (confirmable par le magic `SQLite format 3` en clair, indéterminable si
  chiffré — le type de nœud du système de fichiers suffit et reste le
  critère). Un chemin inexistant est **créé en natif** (le défaut).

### 2. Façade `Memory` : backend interne à deux variantes, contrat unique

`Memory` cesse d'être câblée en dur sur `LibsqlMemoryStore`. Elle porte un
backend interne à deux variantes (`Libsql` / `Native`), et tous les chemins
sémantiques (remember/recall/forget/graphe/stats/consolidation) passent
exclusivement par le contrat `MemoryStore` (ADR-020) — déjà le cas. Les
opérations **backend-spécifiques** (export/import JSONL, contrat embedding
`bmai_meta`, rotation de clé) sont résolues par le backend, sans gonfler le
contrat sémantique avec de la plomberie de portabilité.

Constructeurs :

- `Memory::open_native(path, key, embedder, agent)` — **le défaut**. `key`
  obligatoire pour un store sur disque (ADR-007 s'applique au natif à
  l'identique) ; pas de feature `crypto`/CMake requise (ADR-030).
- `Memory::open(store, embedder, agent)` — inchangé, backend libSQL
  (compatibilité v1 ; les bindings et tests existants ne cassent pas).
- `Memory::open_in_memory` (test-util) bascule sur l'équivalent natif
  éphémère quand la feature `engine-native` est présente (le défaut) : la
  suite de tests façade exerce désormais le backend par défaut réel.

### 3. Portabilité et contrat embedding sur Native

- **Contrat embedding** (`embedding_model_id`/`embedding_dim`, ADR-019) :
  porté sur Native via des enregistrements KV sous le préfixe réservé
  `meta/bmai/` (valeurs UTF-8 brutes, pas de codec versionné — une paire
  clé/valeur opaque, même sémantique `INSERT OR IGNORE` puis vérification
  stricte que côté libSQL).
- **Export/import JSONL** (migration, backup — et LE chemin de migration
  libSQL→Native) : implémenté sur Native à partir des scans existants du
  moteur (`PersistentMemoryIndex::scan`, `PersistentGraph::entities`, scan
  d'arêtes par agent). Même format JSONL v1, même idempotence (les ids déjà
  présents sont comptés `*_skipped`). **Écart d'atomicité assumé** : côté
  natif, les souvenirs s'insèrent en un seul batch WAL tout-ou-rien
  (`put_many`, N5.5), mais entités/arêtes suivent en upserts individuels
  durables — l'import est **idempotent et reprennable**, pas globalement
  atomique (même classe d'écart que `purge_agent`, ADR-027 §6 ; se répare en
  relançant l'import).

### 4. Migration des stores v1 existants

Aucune migration silencieuse, jamais (cohérent ADR-010 : rien d'implicite) :

- Un `.bmai` v1 s'ouvre en libSQL, indéfiniment, sans avertissement bloquant.
- La migration est **explicite et documentée** : `export_jsonl` depuis le
  store v1 → `import_jsonl` vers un store natif neuf (les embeddings sont
  recalculés — c'est le chemin de migration déjà défini pour le changement
  de modèle d'embedding, réutilisé tel quel).
- La CLI expose ce chemin (`export` / `import` existent déjà) ; un
  raccourci `migrate` dédié est un confort différé, pas un préalable.

### 5. Features et matrice CI

- `engine-native` entre dans les features **par défaut** de `basemyai` (et
  transitivement de la CLI, MCP, REST et des bindings). La feature reste
  déclarée (additive) : `--no-default-features` permet toujours un build
  libSQL pur.
- La feature `crypto` (libSQL/CMake) devient **optionnelle de fait** pour le
  cas nominal : le chiffrement du backend par défaut est natif (ADR-030),
  compilé inconditionnellement. `crypto` ne sert plus qu'aux stores v1.
- La matrice `xtask`/CI est mise à jour en miroir strict : les jobs
  par défaut compilent désormais `basemyai-engine` ; les combinaisons
  `--no-default-features` existantes gardent le chemin libSQL pur couvert.

### 6. Ce que cet ADR ne décide pas

- **Le retrait de libSQL** (dépendance, feature `crypto`, pool ADR-021) —
  décision future, ADR séparé, conditionnée à l'usage réel des stores v1.
- **La racinisation Porter** (gap FTS assumé, ADR-028 §2) — inchangé.
- **Le multi-écrivain natif** — hors périmètre (ADR-025, mono-écrivain).
- **Les surfaces de sync P2P** (N6) — débloquées par cette bascule (le WAL
  natif comme primitive de change-capture), mais non actées ici.

## Alternatives rejetées

- **Basculer seulement à `basemyai` 0.2 / attendre plus de terrain.** Rejeté :
  les critères de sortie chiffrés posés *avant* mesure (ADR-026 §6) sont tous
  tenus avec des marges de 3,8× à 13,8× ; la parité contractuelle est de
  100 % sur les trois backends de test ; attendre n'ajouterait que du temps
  calendaire, pas de l'information. La compatibilité v1 borne le risque.
- **Migration automatique des `.bmai` v1 à l'ouverture.** Rejeté : violerait
  la règle « jamais d'action implicite » (ADR-010), doublerait transitoirement
  l'empreinte disque sans consentement, et re-calculer les embeddings sans
  demander est inacceptable sur une base volumineuse.
- **Généraliser `Memory` sur `Arc<dyn MemoryStore>` pur** (sans variantes de
  backend). Rejeté : export/import, contrat embedding et rotation de clé ont
  des mécaniques réellement différentes par backend (SQL brut vs scans KV,
  `PRAGMA rekey`-et-réouverture vs re-scellement O(1) en place) ; les fondre
  dans le contrat sémantique ADR-020 le polluerait de plomberie de
  portabilité pour un bénéfice nul (deux backends, connus, énumérables).
- **Nouveau format d'export pour la migration.** Rejeté : le JSONL v1
  existant (embeddings exclus, recalculés à l'import) est déjà le chemin de
  migration inter-modèles ; il est backend-agnostique par construction.

## Conséquences

- Toute nouvelle base créée par la CLI, MCP, REST ou les bindings est native :
  ~2,9× plus rapide en recall bout-en-bout mesuré, ~4,5-13,8× sur le build
  d'index, chiffrement sans CMake, rotation de clé sans réouverture.
- Les fichiers `.bmai` v1 restent pleinement fonctionnels ; leur migration
  est explicite, outillée et documentée.
- `basemyai-engine` (BUSL-1.1, ADR-031) devient une dépendance par défaut de
  tout le workspace — cohérent avec la licence unifiée.
- Le harnais multi-backend (`backend_suite!`) devient le gardien permanent de
  la non-régression libSQL : tant que le backend de compatibilité existe, les
  19 scénarios tournent contre les deux moteurs en CI.
- CLAUDE.md (racine écosystème + basemyai), `docs/status.md`,
  `docs/PLAN-NATIVE-ENGINE.md` et `docs/TODO-NATIVE-ENGINE.md` sont mis à
  jour : la clause « libSQL reste le défaut jusqu'à parité prouvée » est
  résolue — la parité est prouvée, la bascule est actée.
