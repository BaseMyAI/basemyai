# TODO — Moteur natif BaseMyAI

Backlog du chantier acté par `ADR-024-native-engine.md`, séquencé par
`PLAN-NATIVE-ENGINE.md` §5. Jalons N0→N6. Une case ne se coche que si le
critère de sortie est vérifié (test/CI/chiffre), pas « le code existe ».

## N0 — Chantier 0 : DX + organisation repo (préalable, Phase 0)

Constat : audit d'organisation 2026-07-02 (2 agents, référence SurrealDB).

### DX

- [x] `xtask` (crate workspace + alias `.cargo/config.toml`) encodant
  **exactement** la matrice CI (`ci.yml`) : `cargo xtask check` (fmt + clippy
  par-crate avec les vraies combinaisons de features), `cargo xtask test`,
  `cargo xtask test-embed`, `cargo xtask test-crypto`, `cargo xtask ci`.
  Choix xtask plutôt que justfile/cargo-make : zéro outil à installer (cargo
  suffit), pur Rust (2026-07-02)
- [x] `CONTRIBUTING.md` et `CLAUDE.md` pointent vers les cibles `cargo xtask`
  au lieu de commandes `--workspace` qui ne reproduisent pas la CI (2026-07-02)
- [ ] (optionnel, plus tard) faire appeler les mêmes cibles par `ci.yml`
  (single source of truth)

### Assainissement docs (P0/P1 de l'audit)

- [x] Une seule source de statut : items ouverts de `TODO.md` (racine) fusionnés
  dans `docs/status.md`, `TODO.md` racine supprimé, `docs/TODO.md` archivé
  sous `docs/archive/` (2026-07-02)
- [x] Un seul changelog : contenu FR non reporté de `docs/CHANGELOG.md` remonté
  dans `CHANGELOG.md` racine, puis `docs/CHANGELOG.md` supprimé (2026-07-02)
- [x] `AGENTS.md` réduit à un pointeur vers `CLAUDE.md` ; « deux crates »
  périmé corrigé dans `CLAUDE.md` (2026-07-02)
- [x] `.agents/skills/` (copie octet-à-octet de `.claude/skills/`) supprimé
  (identité vérifiée par diff avant suppression, 2026-07-02)
- [x] `openapi-sidecar.yaml` déplacé vers `crates/basemyai-rest/openapi.yaml`,
  références corrigées (dont le chemin fautif `analayse/openapi-sidecar.yaml`
  dans `basemyai-rest/src/lib.rs`) ; mentions restantes dans `docs/*.md`
  laissées à la passe docs (2026-07-02)
- [x] `docs/ANALYSIS.md` (snapshot git périmé) supprimé (2026-07-02)
- [x] Analyses ponctuelles déplacées sous `docs/research/` : `surrealdb-*.md`
  (×5), `type-mapping.md`, `mcp-blueprint.md` (2026-07-02)

### Reporté (P2 — décisions séparées, ne pas faire en passant)

- [x] Éclater `docs/ADR.md` (monolithe 001–018) en `docs/adr/ADR-0XX-*.md` —
  fait le 2026-07-04, décision explicite de l'utilisateur (tous les ADR
  001–025 vivent maintenant sous `docs/adr/`, `docs/ADR.md` réduit à un index)
- [ ] `bindings/` → `crates/` (ou documenter la règle de séparation)
- [ ] `basemyai-branding/` → `docs/branding/` ou repo séparé
- [ ] `examples/README.md` expliquant racine (SDK par langage) vs
  `crates/basemyai/examples/` (Rust)

## N1 — Spike Couche 1 (Phase 0b) ✅ clos le 2026-07-04

- [x] Vérifier le statut de maintenance 2026 de `redb`, `fjall`, `sled`
  (versions, activité, prod-readiness) — ne pas assumer (2026-07-04 : `redb`
  4.1 actif/mûr, `fjall` 3.1 maintenu mais développement de features en net
  ralentissement, `sled` écarté sans étude approfondie)
- [x] Prototype jetable A : B-tree copy-on-write (lecture concurrente, GC pages)
  — 107 296 inserts/s, 8,7 µs/lecture, ×14,3 amplification espace (pas de
  free-list dans le spike), 10/10 PASS crash-consistency
- [x] Prototype jetable B : LSM-tree (write-path, compaction, change-capture)
  — 435 894 inserts/s, 3,52 µs/lecture, ×1,05 amplification, 10/10 PASS
  crash-consistency
- [x] Comparatif écrit : perf write/read, complexité de crash recovery,
  aptitude au change-capture (pour sync P2P futur) —
  `docs/benchmarks/n1-storage-engine-spike-2026-07-04.md`
- [x] Décision fondation-maison vs fork/dépendance consciente d'un KV Rust
  (critère : propriété des couches 2–4 garantie dans les deux cas) —
  fondation-maison, famille LSM (`ADR-025`)
- [x] Mini-ADR de sortie de spike (architecture Couche 1 figée) —
  `docs/ADR-025-native-engine-storage-foundation.md`

## N2 — Couche 1 : store durable (Phase 1)

**Ordre imposé : le harnais d'abord, le moteur ensuite.**

- [x] Harnais crash-consistency : kill -9 en boucle sous charge d'écriture,
  réouverture, vérification d'intégrité — implémenté (2026-07-04) :
  `crates/basemyai-engine/src/bin/crash_writer.rs` (écriture durable d'un
  flux compteur, confirmation journalisée avec `sync_all`) +
  `tests/crash_consistency.rs` (spawn, `taskkill /F` forcé sur Windows /
  `kill -9` sur Unix, réouverture, vérification intégrale), 20 cycles
  (rigueur du spike N1), `cargo xtask test-crash-consistency`, job CI
  dédié dans `.github/workflows/ci.yml` (matrice ubuntu+windows). Exécuté
  réellement 3 fois en conditions réelles, ciblant spécifiquement la
  fenêtre flush/rename/truncate (seuils de flush abaissés) : 0 corruption
- [x] Fuzzing cargo-fuzz (nightly séparée) : encodage/décodage clés, replay
  WAL, parsing pages — infra posée (2026-07-04) : `crates/basemyai-engine/fuzz/`
  (layout standard `cargo-fuzz`, isolé du workspace via `[workspace]` vide +
  `fuzz/rust-toolchain.toml` → nightly ; jamais dans `cargo xtask`, voir
  `fuzz/README.md`), 4 cibles (`key_roundtrip`, `wal_decode`, `sst_decode`,
  `sst_decode_structured`) exécutées réellement sous WSL (libFuzzer ne link
  pas sur Windows natif — vérifié, pas supposé). `wal_decode` et
  `key_roundtrip` : des millions d'exécutions, aucun crash. `sst_decode`
  (octets bruts) : aucun crash — attendu, le crc32 pré-décodage bloque la
  mutation aléatoire. `sst_decode_structured` (crc32 recalculé, donc le
  fuzzer explore au-delà de ce garde-fou) : **crash trouvé en quelques
  secondes**, **corrigé et testé (2026-07-04)** — `format::sst::decode`
  faisait `Vec::with_capacity(entry_count as usize)` avec `entry_count: u64`
  lu tel quel du fichier, avant toute validation contre la taille réelle du
  buffer → panic "capacity overflow" sur un fichier de 18 octets avec
  `entry_count = u64::MAX` et un crc32 correct (calculable trivialement par
  un attaquant, le crc32 n'étant pas cryptographique). Correctif : borne
  `entry_count` contre le nombre d'entrées que le buffer restant peut
  physiquement contenir avant l'allocation, retourne `CorruptSst` sinon ;
  régression `huge_entry_count_is_rejected_not_panicking` ajoutée
  (`crates/basemyai-engine/src/format/sst.rs`), `cargo test -p
  basemyai-engine` et `cargo clippy -p basemyai-engine --all-targets -- -D
  warnings` verts. Reste ouvert : item éventuel de CI nightly planifiée
  (pattern `embed`/`crypto` de `ci.yml`, non ajouté ici — décision humaine
  sur la cadence, pas devinée)
- [x] `format.lock` + check CI (équivalent `revision.lock` SurrealDB) : chaque
  type persisté versionné, échec CI si changement sans bump — implémenté
  (2026-07-04) : `crates/basemyai-engine/format.lock` (`WalRecord:1`,
  `SstFile:1`), hash CRC32 sur un `FormatSpec` (nom+version+champs de wire,
  dans l'ordre disque — pas le commentaire prose, pas un dérivé automatique
  du struct décodé) défini juste à côté d'`encode`/`decode` dans
  `format/{wal,sst}.rs` (`src/format/lock.rs`), vérifié par le test
  `crates/basemyai-engine/tests/format_lock.rs`, gatté par
  `cargo xtask format-lock` (et inclus dans `check`/`ci`). Testé les deux
  sens : perturbation délibérée d'un champ → échec loud avec message
  actionnable ; retour à l'état correct → vert
- [x] Crate `crates/basemyai-engine` : `store/`, `key/`, `format/`, `error.rs`
  (layout PLAN §3.1) — feature `engine-native`, jamais défaut — implémenté
  (2026-07-04) : membre du workspace (pas `default-members`), `publish =
  false`, feature `engine-native` déclarée réservée pour le câblage
  `EngineKind::Native` (pas encore branchée). Moteur WAL+memtable+SST+
  recovery réel derrière (`Engine::open/put/get/delete/flush/close`),
  jugé vert par le harnais crash-consistency ci-dessus.
- [x] WAL + transactions + recovery, jugés sur le harnais — implémenté
  (2026-07-04) : `Batch`/`Engine::apply_batch` (`store/engine.rs`), un batch =
  **un seul enregistrement WAL externe** (`WalOp::Batch`) contenant une
  séquence imbriquée de sous-opérations put/delete, auto-délimitée et
  couverte par le même `crc32`/`val_len` que le framing single-record
  existant — donc le tolérance-troncature-de-queue déjà présente dans
  `Wal::replay` rejette un batch incomplet **en bloc**, sans reconstruction
  du replay. Changement de format de wire réel et assumé : `WAL_RECORD_VERSION`
  1→2, `format.lock` mis à jour délibérément (`WalRecord:2(b4570df1)`),
  `cargo xtask format-lock` vert. Harnais crash-consistency étendu
  (`crash_writer` mode `batch`, `BATCH_SIZE=6`) : 20 cycles réels via
  `cargo xtask test-crash-consistency`, **cas intéressant réellement
  observé** (batch durci en WAL mais pas encore confirmé au moment d'un kill
  réel — présent intégralement ou absent, jamais partiel). `cargo xtask ci`
  vert.
- [x] Runner de tests déclaratifs multi-backend (`memory-tests`) — scaffold
  implémenté et exécuté (2026-07-04) : scénarios déclaratifs
  (`crates/basemyai/tests/memory_tests/scenarios.rs` — 5 portés depuis
  `storage_contract.rs` : remember/recall round-trip, invalidate,
  forget physique, graphe upsert+traverse, borne `valid_until`) + runner
  paramétré (`memory_tests/mod.rs`, `run_scenario<S: MemoryStore>` — aucune
  implémentation concrète dans le runner) + enregistrement des backends par
  macro `backend_suite!` (`tests/memory_tests.rs`). Vert contre `Libsql`
  (seul backend réel) via `cargo test -p basemyai --features test-util
  --test memory_tests`, couvert par la matrice `xtask test` existante.
  **Diff multi-backend PAS encore prouvé** — impossible aujourd'hui, `Native`
  n'a pas de `MemoryStore` (bloqué N3/N4). Brancher `Native` = implémenter
  `MemoryStore`, une factory async, une ligne `backend_suite!`.
- [x] `EngineKind::Native` dans `basemyai-core` (additif, `Libsql` inchangé) —
  identité + capacités seulement : `EngineCapabilities::native()`
  (`crates/basemyai-core/src/storage/engine.rs`) rapporte l'état honnête
  actuel de `basemyai-engine` — `vectors: false`, `full_text: false`,
  `recursive_queries: false`, `transactions: true` (batches atomiques réels,
  `Engine::apply_batch`), `encrypted: false` (pas de chiffrement au repos,
  vérifié dans la source) — plus un wrapper `NativeEngine`
  (`crates/basemyai-core/src/storage/native.rs`) autour de
  `basemyai_engine::Engine` implémentant `StorageEngine` (pas `MemoryStore`),
  derrière la feature `engine-native` (jamais par défaut ; dépendance
  optionnelle `basemyai-engine` ajoutée dans `Cargo.toml`). Test dédié
  (`native_engine_reports_honest_capabilities`) + matrice `xtask` étendue
  (`clippy`/`test` avec `--features engine-native`) + grep d'agnosticité
  (`tests/agnosticity.rs`) toujours vert avec la feature activée. `cargo
  xtask check`/`test` verts (2026-07-04).
- [x] impl `MemoryStore` branchée sur `Native` — débloquée par N3+N4, faite
  au N5.1 (2026-07-05, ADR-027) : voir la section N5.1 ci-dessous.

## N3 — Couche 2 : index vectoriel natif (Phase 2)

- [x] Choix HNSW vs DiskANN (critère : profil mémoire vs disque pour mémoire
  d'agent locale ; libSQL utilise LM-DiskANN) — **DiskANN, variante
  LM-DiskANN sur KV** (`ADR-026`, 2026-07-04) : graphe plat, un nœud = un
  enregistrement KV dans le store LSM (pas de sidecar), mises à jour d'index
  dans le même `apply_batch` que la donnée (crash consistency héritée du
  WAL), donnée = source de vérité (rebuild possible), deletes de premier
  ordre (tombstones + réparation paresseuse + consolidation via
  `MaintenanceWorker`). Seuils de sortie posés avant mesure (ADR-026 §6) :
  requête ≤ parité libSQL (~48-49 ms), build < 78-79 ms/ligne y compris
  incrémental, recall@10 ≥ 0,9 y compris après churn insert/delete
- [x] Implémentation dans `basemyai-engine/src/idx/vector/` (découpage
  ADR-026) — étapes 1-4 (format/distance/graphe/persistance/deletes) closes
  le 2026-07-05, bench de parité M6 mesuré le même jour (voir plus bas) :
  - [x] `node.rs` — bloc nœud LM-DiskANN versionné (`VectorNode:1`, magic
    `b"VNOD"`, header u32+u16+u16+u16, vecteur f32 LE, voisins u64 LE,
    crc32 trailing, framing/erreurs alignés sur `format/{wal,sst}` ;
    `spec()` enregistré dans `format.lock`, leçon fuzzing N2 appliquée :
    `dim`/`neighbor_count` bornés contre la taille réelle du buffer AVANT
    toute allocation, tests de rejet dédiés) + cible fuzz
    `fuzz/fuzz_targets/vector_node_decode.rs` posée sur le modèle
    `sst_decode` (non exécutée — libFuzzer ne link pas sous Windows natif,
    même contrainte que N2 : run WSL à faire)
  - [x] `distance.rs` — cosine f32, dimension paramétrable (défaut 384),
    fold contigu simple (autovectorisation), zéro-norme → 1.0 jamais NaN
  - [x] `graph.rs` — Vamana en RAM : greedy beam search (L), robust prune
    (α), insert incrémental + liaisons bidirectionnelles avec re-prune des
    voisins > R ; `meta.rs` — params (dim=384, R=32, L=128, α=1.2).
    **Écart assumé vs les « classiques » L=64** : mesuré sur le harnais,
    L=64 donne recall@10 = 0.856 (< seuil) sur données iid uniformes 384d,
    L=128 → 0.988 (+~12 % de coût d'insert) ; les setups DiskANN de
    référence utilisent L_build ≈ 100-125
  - [x] Harnais recall (`tests/vector_recall.rs`, oracle brute-force exact,
    RNG xorshift64* seedé, zéro dépendance) — **recall@10 = 1.0000 mesuré à
    N=2 000 ET N=10 000** (50 requêtes, k=10, ~0.5/1.4 ms/insert en release,
    harnais de correctness, pas un bench). Données à **basse dimension
    intrinsèque** (latent 16d → map linéaire seedée vers 384d, modèle des
    embeddings MiniLM) — choix documenté dans le test : mesuré, l'iid
    uniforme 384d (concentration des distances, aucun voisinage exploitable
    par AUCUN graphe-ANN) donne 0.664 à 10k même à L=200/R=64→0.946, et 64
    clusters quasi-orthogonaux fragmentent le graphe en îlots (0.282 à
    10k) — deux pathologies notées pour le futur bench parité. N=2 000
    dans le gate (`xtask test` + step Test CI), N=10 000 en `#[ignore]`
    (exécuté réellement en release, 2026-07-04)
  - [x] persistance KV (`persistent.rs`, pas `cache.rs` — le cache borné vit
    dedans) — implémentée et vérifiée (2026-07-05) :
    `PersistentVectorIndex` (open/insert/search/rebuild), un nœud = un
    enregistrement KV sous le préfixe réservé `idx/vector/`
    (`key::vector_index`, ids BE → scan trié), lecture des blocs à la
    demande via `Engine::get` (jamais de chargement intégral ; cache
    read-through borné 4096 blocs, politique clear-when-full assumée
    jusqu'au bench). L'algorithme Vamana est **partagé** (trait
    `NodeProvider` + `plan_insert` dans `graph.rs`) entre le graphe RAM et
    le persistant — zéro dérive possible. **Atomicité** : chaque insert =
    UN `apply_batch` (nœud + voisins re-prunés + méta) ; **méta persistée**
    `VectorIndexMeta:1` (params, entry point, epoch, count) versionnée dans
    `format.lock` ; **rebuild** depuis les vecteurs des blocs (donnée =
    source de vérité) sur méta absente/corrompue/incohérente, crash-safe
    par ordre (delete méta → chunks de nœuds → méta epoch+1 en dernier).
    Nouveau `Engine::scan_prefix` (merge SSTs+memtable, tombstones filtrés)
    pour l'énumération du rebuild. Mesuré réellement :
    `tests/vector_persistence.rs` (gate + CI) — round-trip N=400/384d à
    travers reopen WAL-replay ET SST, résultats identiques bit-à-bit,
    recall@10 = 1.0000 ; rebuild (méta corrompue ET supprimée, N=300) →
    recall@10 = 1.0000, epoch bumpé, réouverture propre ensuite ; harnais
    crash étendu (`crash_writer` mode `vector` +
    `vector_kill_reopen_verify_loop`) exécuté réellement 2× via
    `cargo xtask test-crash-consistency` : 20 cycles kill forcé chacun
    (1162 puis 1101 vecteurs confirmés cumulés), réouverture **sans
    rebuild** à chaque cycle, chaque bloc confirmé byte-exact
    (exhaustif) + retrouvable par `search` top-10 (échantillon borné
    déterministe : 20 récents + 50 stratifiés), 0 violation. Recall RAM
    re-vérifié après le refactor partage-algorithme : 1.0000 à N=2000
    (gate) et N=10000 (release, ~1,70 ms/insert)
  - [x] deletes (tombstones + réparation paresseuse + consolidation façon
    FreshDiskANN) — implémentés et **recall re-mesuré après churn
    insert/delete** (ADR-026 §6, le critère le plus fragile), 2026-07-05 :
    - **Tombstone dans le bloc nœud** : `VectorNode` v1→v2 (byte `flags`,
      bit 0 = deleted ; bits réservés rejetés au décodage), bump
      `format.lock` délibéré (`VectorNode:2(1ba6b40f)`) — marqueur dans le
      bloc (pas de clé tombstone séparée) : bloc autonome façon LM-DiskANN,
      delete = réécriture locale d'UN bloc + méta dans UN `apply_batch`
      atomique, et le rebuild voit le marqueur en scannant la donnée qu'il
      lit déjà (les blocs tombstonés sont purgés, jamais ressuscités).
      Crate non publié → coupure nette v1 (pas de migration, aucun bloc v1
      hors des tests du repo). Les nœuds tombstonés restent des points de
      routage traversables mais sont exclus des résultats de `search`
      (`search_live`) ; les listes réécrites ne pointent plus vers eux
      (`robust_prune` les saute).
    - **Sémantique re-insert** : `DuplicateVectorId` réservé aux ids
      **vivants** ; ré-insérer un id tombstoné = **résurrection** avec le
      nouveau vecteur (update = delete + reinsert fonctionne), testée RAM +
      persistant + across reopen.
    - **`consolidate()` explicite** (pas de thread de fond — moteur sync
      mono-écrivain) : réparation FreshDiskANN de chaque vivant référençant
      des tombstones (robust-prune sur voisins ∪ voisins-de-voisins
      vivants, `plan_repair` partagé RAM/persistant), ré-ancrage de l'entry
      point sur le vivant le plus proche, puis purge physique — batch-
      atomique par tranches de 512, **crash-safe par ordre** (réparations →
      méta → purge : tout état intermédiaire est un index valide ; les
      tombstones restants d'une consolidation interrompue sont absorbés par
      la suivante ou par un rebuild).
    - **Chiffres churn réels** (`tests/vector_churn.rs`, oracle brute-force
      sur les vivants uniquement, 3 cycles de 20 % delete + réinsertion,
      50 requêtes, k=10) : N=2 000 → **recall@10 = 1.0000 après churn**
      (1 200 tombstones en place, sans consolidation) **et 1.0000 après
      `consolidate()`** ; N=10 000 en release (`#[ignore]`, exécuté
      réellement) → **1.0000 / 1.0000** (6 000 tombstones purgés). Flavor
      persistant : 1.0000/1.0000 à N=600 + réouverture propre, blocs
      tombstonés physiquement absents du store après consolidation.
    - **Harnais crash étendu au churn** (`crash_writer` mode `vector`
      réécrit : schedule déterministe pur `harness::churn_op` — inserts +
      deletes entrelacés (1/7) + `consolidate()` périodique (1/43)) :
      exécuté réellement, 20 cycles kill forcé — 1 090 steps confirmés
      (913 inserts, 152 deletes, 25 consolidations), kills tombés 1× en
      plein delete et 4× en pleine consolidation, réouverture **sans
      rebuild** à chaque cycle, ids confirmés-supprimés jamais ressortis de
      `search` (vérif exhaustive au niveau bloc purgé-ou-tombstoné +
      échantillon de vraies recherches), vivants byte-exacts (exhaustif) et
      retrouvables (échantillon), **0 violation**.
    - Cible fuzz `vector_node_decode_structured` posée (crc32 recalculé →
      le fuzzer atteint la validation des bits de flags derrière le garde
      crc) — **non exécutée** : même contrainte que N2, libFuzzer ne linke
      pas sous Windows natif, run WSL à faire.
    - Écart assumé vs ADR-026 §4 : la consolidation est une opération
      explicite `consolidate()` (appelable par le futur `MaintenanceWorker`
      du consommateur) — le branchement `MaintenanceWorker` lui-même vit
      côté `basemyai` et attend le câblage `MemoryStore` sur Native (bloqué
      N3-bench/N4).
- [x] Parité bench M6 : mêmes scénarios 10k/100k que
  `docs/benchmarks/m6-knn-results-2026-07-01.md`, chiffres archivés
  (2026-07-05) — `crates/basemyai-engine/src/bin/vector_bench.rs` (outil
  manuel, pas dans `cargo xtask`, comme `crash_writer`), exécuté réellement
  en `--release` sur la **même machine** que M6 (13th Gen Intel Core
  i7-13620H confirmé identique) : requête moyenne k=10 **7.52 ms à 10k /
  12.67 ms à 100k** (seuil ADR-026 ~48-49 ms — tenu avec large marge, 6,5×
  puis 3,8×) ; build incrémental réel (pas de bulk-load-then-index, chemin
  `remember` réel) **5.69 ms/ligne à 10k / 17.32 ms/ligne (moyenne pleine
  run) à 100k** (seuil < 78-79 ms/ligne — tenu, 13,8× puis 4,5× de marge) ;
  recall@10 **1.0000 aux deux tailles** (seuil ≥ 0,9) ; disque **22.26 MiB à
  10k / 177.28 MiB à 100k**. RAM : mesure **incomplète** — le sampler
  `Get-Process -WorkingSet64` prévu (façon stress Candle M6) est mort
  silencieusement (process de lancement PowerShell arrêté avec son job
  d'arrière-plan), seuls des snapshots ponctuels partiels existent
  (233-278 Mio, couvrant seulement la première moitié du run 100k) —
  documenté honnêtement comme plancher, pas pic, dans le rapport archivé.
  Écart de protocole assumé vs M6 : générateur de vecteurs différent
  (latent-dim bas, façon `tests/vector_recall.rs`, pas l'iid uniforme de M6
  — pathologie ANN documentée, cf. ADR-026/`tests/common`), donc le nombre
  de recall n'est comparable qu'aux autres gates ADR-026 de ce dépôt, pas à
  un hypothétique recall libSQL (jamais mesuré par M6). Détail complet,
  tableau comparatif et limites : `docs/benchmarks/n3-vector-parity-2026-07-05.md`
- [x] Cible d'amélioration identifiée sur le coût de build d'index
  (libSQL : ~78-79 ms/ligne, quasi-linéaire — c'est LE point faible mesuré
  du backend actuel, le moteur natif doit faire mieux) — **tenue aux deux
  tailles mesurées** (voir ci-dessus), avec une réserve honnête notée : le
  coût marginal natif n'est **pas** plat comme le taux de bulk-load de
  libSQL (~78-79 ms/ligne quasi-constant) — il croît en continu pendant un
  run (≈10 ms/ligne en début de run 100k, ≈24 ms/ligne en fin), la marge sous
  le seuil se réduit donc avec N même si elle reste large aux tailles
  testées ; pas extrapolé au-delà de 100k (deux points de mesure seulement,
  une courbe non plate rendrait toute extrapolation peu fiable).

**N3 clos le 2026-07-05** : choix d'algorithme (ADR-026), implémentation
complète (format/distance/graphe/persistance/deletes-consolidation) et bench
de parité M6 mesuré, les trois seuils de sortie ADR-026 §6 tenus aux deux
tailles (10k et 100k) — voir `docs/benchmarks/n3-vector-parity-2026-07-05.md`
pour le détail et les limites honnêtes (RAM incomplète, coût de build non
plat, générateur de données différent de M6). L'impl `MemoryStore` sur
`Native` (N2, item bloqué) peut reprendre une fois N4 (graphe) posé.

**N3.1 — hardening du bench (2026-07-05, suivi non-bloquant, ne rouvre pas
N3)** : `crates/basemyai-engine/src/bin/vector_bench.rs` durci —
échantillonneur RAM in-process (thread dans le même process, remplace le
poller PowerShell externe qui mourait silencieusement — voir
`docs/benchmarks/n3-vector-scale-followup-2026-07-05.md`), et
`VECTOR_BENCH_SKIP_ORACLE=1` pour caractériser le coût à plus grande échelle
(250k+) sans garder l'oracle brute-force complet en RAM. Un run à 250k a été
lancé (voir le rapport de suivi pour le résultat mesuré ou l'état en cours) ;
500k/1M restent des runs manuels à lancer localement (instructions dans le
rapport), volontairement non extrapolés depuis seulement deux points de
mesure (10k/100k) dont la courbe de coût marginal n'est pas plate. Reste
ouvert : run 500k/1M réel, isolation RAM allocateur-niveau (hors de portée
d'un outil manuel sans dépendance).

## N4 — Couche 3 : graphe natif (Phase 3) ✅ clos 2026-07-05

- [x] Stockage d'adjacence dans `idx/graph/` + traversée bornée —
  `crates/basemyai-engine/src/idx/graph/{entity,edge,traverse,ram,persistent}.rs`.
  Design de clés (`key::graph_index`, `crates/basemyai-engine/src/key/mod.rs`) :
  `idx/graph/entity/<agent_len:u32 BE><agent><id>` (un nœud = un enregistrement
  KV, `GraphEntity:1`) et `idx/graph/edge/<agent_len:u32 BE><agent><src_len:u32 BE><src><relation_len:u32 BE><relation><dst>`
  (`GraphEdge:1`) — `relation`/`dst` vivent dans la **clé**, pas la valeur :
  c'est ce qui rend « toutes les arêtes sortantes d'un nœud » un simple scan
  préfixé (`edge_src_prefix(agent, src)`), le pattern d'accès dont la BFS a
  besoin à chaque saut, sans index secondaire. Longueurs préfixées (u32 BE)
  choisies plutôt qu'une concaténation brute : sans elles, agent `"ab"` + id
  `"c"` et agent `"a"` + id `"bc"` collisionneraient — testé explicitement
  (`graph_entity_keys_do_not_collide_across_agent_id_boundary`). Isolation
  par agent **structurelle** (dans le layout de clé), jamais un filtre
  applicatif après coup. Toute mutation = **un seul `Engine::put`** (pas de
  `Batch`/`apply_batch`) — justifié dans le doc du module `persistent.rs` :
  contrairement au vecteur (qui touche systématiquement plusieurs
  enregistrements — nœud + voisins re-prunés + méta), un upsert de graphe ne
  touche jamais plus que l'unique enregistrement qu'il nomme, et `Engine::put`
  seul est déjà durable/atomique par clé (WAL fsync avant memtable). Tous les
  comptages lus du wire (`kind_len`, `label_len`, `relation_len` dans la clé)
  sont bornés contre la taille réelle du buffer avant toute allocation (leçon
  fuzzing N2/N3), testé (`lying_kind_len_is_rejected_not_panicking`,
  `lying_label_len_is_rejected_not_panicking`,
  `graph_edge_relation_dst_rejects_truncated_and_foreign_keys`). Traversée :
  BFS **itérative** (`VecDeque`, pas de récursion — pas de stack overflow sur
  un graphe profond), ensemble visité garantissant à la fois la
  cycle-safety et la propriété « première visite = profondeur minimale »
  (`idx/graph/traverse.rs`, `run()`), filtrage temporel `valid_until`
  (délibérément **pas** `valid_from` — voir ci-dessous), isolation par
  `agent` structurelle dans le layout de clés. `format.lock` : `GraphEntity:1`
  et `GraphEdge:1` ajoutés (`crates/basemyai-engine/format.lock`), vérifiés
  par `cargo xtask format-lock`.
- [x] Parité `tests/graph.rs` (scoping agent, cycle-safety, profondeur) —
  **portage littéral, pas une réinvention** : `idx/graph/traverse.rs` est un
  port comportemental 1:1 de `LibsqlMemoryStore::graph_traverse` (la CTE
  récursive de `crates/basemyai/src/storage/libsql_store.rs`), y compris un
  détail qui ressemble à un bug de l'original mais est délibérément reproduit
  à l'identique : la CTE ne filtre **que** sur `valid_until` (jamais
  `valid_from`), pour les entités comme pour les arêtes — reproduit tel quel
  plutôt que « corrigé », pour ne pas changer silencieusement le
  comportement d'un appelant qui compterait sur la parité. **Les 5 scénarios
  de `crates/basemyai/tests/graph.rs` sont portés fidèlement** (mêmes
  graphes, mêmes profondeurs, mêmes assertions) dans
  `crates/basemyai-engine/tests/graph_parity.rs` :
  `traverses_multiple_hops`, `isolation_hides_other_agents_edges`,
  `agents_can_reuse_same_graph_ids_without_conflict`,
  `excludes_expired_entities_and_edges`, `terminates_on_cycle` — **les 5
  passent contre les deux flavors** (`RamGraph` in-RAM d'abord, comme oracle,
  puis `PersistentGraph` KV-persisté, `ram_graph_matches_every_ported_scenario`
  / `persistent_graph_matches_every_ported_scenario`), plus un test dédié de
  round-trip close/reopen (`persistent_graph_survives_close_reopen_round_trip`)
  et un test d'exactitude bit-à-bit après réouverture
  (`persistent_entity_is_byte_identical_after_reopen`). L'algorithme BFS est
  **partagé** (trait `GraphProvider` + `traverse::run`, `idx/graph/traverse.rs`)
  entre `RamGraph` et `PersistentGraph` — zéro dérive possible entre les deux
  flavors, même discipline que le partage Vamana de N3.

**Pas de métadonnée/rebuild** (contrairement à l'index vectoriel) — décision
documentée dans le doc du module `idx/graph/persistent.rs`, pas un oubli :
(1) aucun état de navigation global à mettre en cache — une traversée reçoit
son nœud de départ explicitement à chaque appel, contrairement à la
recherche vectorielle qui a besoin d'un point d'entrée fixe précalculé ; (2)
aucune structure dérivée à désynchroniser de la donnée — les blocs
entité/arête **sont** la donnée, pas un graphe Vamana construit par-dessus
des vecteurs ; un bloc absent ou corrompu se comporte exactement comme à la
toute première lecture d'un store neuf (« absent », ou une erreur de decode
franche pour un bloc corrompu — jamais silencieusement ignoré), il n'existe
aucune génération d'index séparée qui pourrait être périmée par rapport à la
donnée puisqu'il n'existe aucune génération d'index du tout.

**Crash consistency réelle** : `crash_writer` mode `graph` ajouté
(entités/arêtes construisant une chaîne linéaire `0 -> 1 -> 2 -> ...`,
schedule déterministe `harness::graph_op`, un `Engine::put` par étape — donc
reprise trivialement idempotente, pas de suite delete/consolidate à gérer
contrairement au mode `vector`). `graph_kill_reopen_verify_loop`
(`tests/crash_consistency.rs`) exécuté **réellement** via
`cargo xtask test-crash-consistency` : **20 cycles de kill forcé réel**,
11 528 à 12 692 steps confirmés selon les runs, réouverture à chaque cycle,
vérification exhaustive des entités/arêtes confirmées durables (byte-exact,
`Engine::get` direct) **et** vérification par vraie traversée
(`PersistentGraph::traverse` depuis la racine sur un échantillon borné de la
chaîne confirmée) — 0 violation observée sur l'ensemble des runs.

**Wiring** : module `idx::graph` exposé depuis `lib.rs`
(`GraphEdgeMeta`/`GraphEntity`/`PersistentGraph`/`RamGraph`/`Reached`
re-exportés, comme le vecteur). `xtask/src/main.rs` (`TEST`) et
`.github/workflows/ci.yml` (job `gate`) mis à jour en miroir strict pour
inclure `--test graph_parity` à côté des tests vecteur existants (aucune
divergence introduite, règle en tête de `xtask/src/main.rs` respectée).
Cibles fuzz posées sur le modèle des existantes,
`graph_entity_decode`/`graph_edge_decode` (`fuzz/fuzz_targets/`,
`fuzz/Cargo.toml`, `fuzz/README.md` mis à jour) — **non exécutées**, même
contrainte Windows/WSL déjà documentée pour N2/N3 (libFuzzer ne linke pas en
natif sous Windows).

**Écarts au brief** : aucun écart de fond. `MemoryStore` sur `Native` reste
hors périmètre (N5, bloqué par ce jalon désormais levé). La traversée est
strictement dirigée (aucune arête entrante suivie), fidèle à la CTE
d'origine qui ne suit que `edge.src` — `tests/graph.rs` ne teste que des
arêtes dirigées, donc rien n'indique qu'une traversée bidirectionnelle soit
attendue.

## N5 — Parité complète (Phase 4)

Découpage et décisions structurantes actés par `ADR-027` (2026-07-05) :
mapping `MemoryStore`→Native (module `idx/memory` moteur, compteur monotone
`vec_id`, atomicité par fusion de batch `insert_with`/`delete_with`, pont
sync↔async mono-écrivain sérialisé, écarts de parité assumés et documentés).

### N5.1 — `NativeMemoryStore` hors FTS/crypto ✅ clos 2026-07-05

- [x] Moteur : `idx/memory/` (`MemoryRecord:1`, `MemoryVecMap:1`,
  `MemoryIndexMeta:1` dans `format.lock`, hashes vérifiés par
  `cargo xtask format-lock`), clés `key::memory_index` (préfixe réservé
  `idx/memory/`, longueurs préfixées u32 BE anti-collision comme N4,
  isolation agent structurelle), `PersistentMemoryIndex`
  (put/get/update/forget/scan/résolution/touch/purge — composition
  crash-critique côté moteur, testable par le futur harnais N5.5). Codecs à
  la discipline N2/N3 : comptages bornés contre la taille réelle du buffer
  avant toute allocation, tests de troncature à chaque coupure, bit-flip,
  longueurs mensongères, version inconnue (2026-07-05)
- [x] Moteur : `PersistentVectorIndex::insert_with`/`delete_with` — les ops
  du consommateur (`Batch::extend_from`) fusionnées dans le **même**
  `apply_batch` que les blocs d'index : un `remember` natif = UN
  enregistrement WAL (compteur + vecmap + record + nœud + voisins re-prunés
  + méta), présent ou absent en bloc (ADR-027 §3). `delete_with` applique
  l'extra même sur tombstone no-op (les compagnons ne survivent pas à un
  forget interrompu). `search_scored` expose les distances déjà calculées
  par le beam search (RAM et persistant, même surface). `PersistentGraph` :
  `entities()` (listing par agent), `edge_meta()` (parité upsert
  `weight`-only), `purge_agent()` (batch atomique). Allocateur `vec_id`
  monotone persistant, guéri depuis la donnée si méta absente/corrompue —
  test dédié anti-réutilisation après forget+consolidate+reopen
  (`allocator_is_monotonic_across_reopen_and_forgets`) (2026-07-05)
- [x] `basemyai` : `NativeMemoryStore` derrière la feature `engine-native`
  (`storage/native_store.rs`) — pont `spawn_blocking` + verrou jamais tenu
  à travers un `.await`, oversampling ×8 + post-filtre
  agent/validité/couche (ADR-012), parité requête par requête avec
  `LibsqlMemoryStore` y compris ses non-filtres (`hydrate` et
  `exact_fact_exists` sans filtre de validité, `graph_upsert_edge`
  préservant `valid_from`), `keyword_ranking_ids` et métriques non-cosinus
  en **erreur franche** (N5.2/N5.3), `open_ephemeral()` sous `test-util`
  (équivalent natif d'`open_in_memory`) (2026-07-05)
- [x] `backend_suite!(native)` vert — le diff multi-backend du runner N2
  enfin prouvé : les 5 scénarios déclaratifs rejoués contre `Libsql` ET
  `Native`, zéro divergence (`cargo test -p basemyai --features
  test-util,engine-native --test memory_tests`) ; matrice `xtask`/`ci.yml`
  étendue en miroir strict (clippy + test `engine-native`) ; harnais
  crash-consistency re-exécuté après le refactor `insert_with` : 4 modes
  (base/batch/vector/graph), 0 violation (2026-07-05)
- [x] `EngineCapabilities::native()` mis à jour honnêtement :
  `vectors`/`recursive_queries` → `true` (N3/N4 les fournissent, N5.1 les
  câble), `full_text`/`encrypted` restent `false` (N5.2/N5.4) ;
  `native_engine_reports_honest_capabilities` mis à jour. `cargo xtask ci`
  vert au 2026-07-05 (seul incident : le flake connu et pré-existant
  `basemyai-mcp --test sampling` STATUS_ACCESS_VIOLATION, re-passé vert 3×
  d'affilée sans modification — sans rapport avec ce travail, mcp ne
  compile pas `engine-native`)

### N5.2 — FTS/BM25 natif ✅ clos 2026-07-06

Décisions structurantes actées par `ADR-028` (2026-07-06) : périmètre borné
au sous-ensemble de `match_expr` réellement produit par `fts_match_expr()`
(tokens cités joints par ` OR `, jamais la syntaxe FTS5 complète), tokenizer
casefold+pliage d'accents par table figée (racinisation Porter différée,
gap documenté), troisième index moteur `idx/fts` (postings + docterms +
stats par agent), atomicité par fusion de batch comme ADR-027 §3, BM25
Okapi `k1=1.2`/`b=0.75` (défauts FTS5).

- [x] Moteur : `key::fts_index` (`idx/fts/postings/...`,
  `idx/fts/docterms/...`, `idx/fts/meta/...`, longueurs préfixées u32 BE,
  isolation agent structurelle, même discipline anti-collision que
  `graph_index`/`memory_index`) (2026-07-06)
- [x] Moteur : `idx/fts/tokenizer.rs` (découpe non-alphanumérique identique
  à `fts_match_expr`, minuscule Unicode, table de pliage d'accents
  Latin-1/Latin Extended-A courants — zéro dépendance nouvelle) (2026-07-06)
- [x] Moteur : codecs `FtsPosting:1` (tf), `FtsDocTerms:1` (liste
  `(term, tf)` bornée), `FtsStats:1` (`doc_count`/`total_terms` par agent)
  dans `format.lock` — discipline N2/N3/N5.1 complète (comptages bornés,
  troncature à chaque coupure, bit-flip, longueur mensongère, version
  inconnue) (2026-07-06)
- [x] Moteur : `PersistentFts::stage_insert`/`stage_delete` (composent dans
  le `Batch` de l'appelant, jamais leur propre `apply_batch` — même couture
  qu'ADR-027 §3) ; `df(t)` dérivé du scan `postings`, jamais un compteur
  caché séparé ; stats par agent healées à la demande (paresseux, pas au
  niveau `open()` global comme `MemoryIndexMeta`) (2026-07-06)
- [x] Moteur : `search_bm25(engine, agent, match_expr, k)` — scoring Okapi,
  parsing strict du sous-ensemble `match_expr` (erreur franche hors
  périmètre, jamais un résultat partiel silencieux) (2026-07-06)
- [x] `PersistentMemoryIndex::put`/`forget`/`purge_agent` gagnent un
  paramètre `PersistentFts`, empilé dans le même `extra: Batch` que
  `insert_with`/`delete_with` — un `remember` natif reste UN enregistrement
  WAL, étendu au troisième index ; re-vérifié sous le harnais
  crash-consistency (4 modes base/batch/vector/graph, 0 violation) (2026-07-06)
- [x] `basemyai` : `NativeMemoryStore::keyword_ranking_ids` branché sur
  `search_bm25` (fin de l'erreur franche N5.2) ; oversampling ×8 (ADR-012)
  + filtre agent/validité après coup — bug de parité trouvé et corrigé en
  cours de route (l'implémentation initiale ignorait `now`, un
  `#[allow(unused)]` silencieux aurait laissé passer un souvenir invalidé/
  expiré dans le classement BM25) ; `NativeInner`/`put_one`/`forget`/
  `purge_agent` mis à jour en miroir (2026-07-06)
- [x] `backend_suite!` : deux scénarios de parité ajoutés à
  `tests/memory_tests/scenarios.rs`
  (`keyword_ranking_orders_by_relevance_and_truncates`,
  `keyword_ranking_respects_temporal_validity_and_forget` — validité
  temporelle + forget, portés depuis `temporal_validity_boundary`/
  `forget_deletes_physically`), rejoués contre `Libsql` ET `Native`, zéro
  divergence (`cargo test -p basemyai --features test-util,engine-native
  --test memory_tests`). Classements BM25 conçus pour être robustes entre
  implémentations (`tf` croissant à `df`/longueur/`idf` égaux — propriété
  monotone de BM25 — plutôt que des scores exacts entre termes différents,
  plus sensibles aux détails d'implémentation) (2026-07-06)
- [x] `EngineCapabilities::native().full_text` → `true` (honnête, plus
  d'erreur franche pour ce chemin) (2026-07-06)
- [x] `xtask`/`ci.yml` : aucune modification nécessaire — le nouveau module
  vit sous des entrées déjà couvertes (`--lib` pour `basemyai-engine`,
  `--test memory_tests` pour `basemyai --features test-util,engine-native`)
  ; `cargo xtask ci` vert (18 étapes), harnais crash-consistency re-exécuté
  (4 modes, 0 violation) — le triplet postings/docterms/stats n'a pas son
  propre mode dédié dans le harnais (reste un item de suivi non bloquant,
  N5.5) mais est exercé indirectement par le mode `batch`/`vector` puisqu'il
  chevauche le même `apply_batch` (2026-07-06)
- [ ] Item de suivi séparé, non bloquant : racinisation Porter (gap assumé
  par ADR-028 §2) — à instruire seulement si mesuré comme un manque de
  recall significatif en usage réel
- [ ] Item de suivi séparé, non bloquant : mode `memory`/`fts` dédié du
  harnais crash-consistency (couverture directe du triplet
  postings/docterms/stats, comme les modes `vector`/`graph` existants) —
  N5.5

### N5.3 — 100 % des contrats sur Native

- [ ] 100 % de `storage_contract.rs` + `contracts.rs` verts sur `Native`
  (portage des scénarios restants dans le runner déclaratif)

### N5.4 — Chiffrement au repos natif

- [ ] Chiffrement au repos (équivalent ADR-007 — chantier crypto sérieux,
  WAL + SST + blocs d'index) + rotation de clé (parité `rotate_key` M6)

### N5.5 — Barre hardening M6

- [ ] Modèle de concurrence au-delà du mono-écrivain sérialisé de N5.1
  (pool/lecteurs, équivalent ADR-021), mesuré
- [ ] Bench KNN via le chemin `MemoryStore` complet (pas l'index nu),
  stress long, harnais crash étendu mode `memory`
- [ ] `put_memory_batch` tout-ou-rien (composition multi-plans Vamana dans
  un seul batch — écart assumé d'ADR-027 §6 à résorber ou re-documenter)

### N5.6 — Bascule du défaut

- [ ] ADR de bascule du défaut libSQL→Native (chiffres à l'appui) —
  décision séparée et humaine, jamais prise en passant

## N6 — Aval (après Phase 4)

- [ ] Sync P2P (change-capture du WAL comme primitive ; VISION §5.6)
- [ ] Couche 4 : langage de requête (micro-crates `token`→`parser`→`ast`) —
  **décision produit préalable** : outil interne/CLI, pas surface agent
  (l'avantage `remember`/`recall` sans langage est documenté et se protège)

## Parallèle — indépendant du moteur

- [ ] Multi-modèles d'embedding (catalogue `EMBED_KNOWN_MODELS`, `schema(dim)`
  paramétré, `setup --model`) — ne dépend d'aucun backend, peut démarrer
  n'importe quand
