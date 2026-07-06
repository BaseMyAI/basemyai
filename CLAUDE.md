# BaseMyAI — guide agent

Moteur de mémoire local pour agents IA. Le cœur du workspace est le **duo**
`basemyai-core` (socle agnostique) + `basemyai` (sémantique mémoire), posée dessus.
Le workspace compte en tout **6 crates** (`crates/` : `basemyai-core`, `basemyai`,
`basemyai-cli`, `basemyai-mcp`, `basemyai-rest`, `basemyai-engine` — moteur de
stockage maison, ADR-024/025, non publié) + **2 bindings**
(`bindings/basemyai-py`, `bindings/basemyai-node`) + l'outil DX `xtask/`.
Décisions : `docs/ADR.md` (index, un fichier par ADR sous `docs/adr/`). Relation d'écosystème : `../ECOSYSTEM_ARCHITECTURE.md`.

## Commandes

**LE point d'entrée qui reproduit la CI est `cargo xtask`** (matrice exacte de
`.github/workflows/ci.yml` : par-crate, avec les bonnes combinaisons de features).

```bash
cargo xtask check        # fmt --check + clippy par crate (features CI), -D warnings
cargo xtask test         # tests par crate, config légère (sans embed/crypto)
cargo xtask ci           # check + test — LE gate avant commit
cargo xtask test-embed   # job CI `embed` (Candle, lourd)
cargo xtask test-crypto  # job CI `crypto` — EXIGE CMake installé
cargo xtask format-lock  # anti-drift basemyai-engine/format.lock (inclus dans check/ci)
cargo xtask test-crash-consistency  # kill-loop réel basemyai-engine (~20 cycles, job CI dédié)
```

⚠ **`cargo clippy --workspace` ne reproduit pas la CI** (la CI clippy/teste chaque
crate avec des features précises, ex. `-p basemyai-mcp --no-default-features
--features stdio,http,test-util`) — utiliser `cargo xtask check`. Commandes cargo
brutes, gardées en référence :

```bash
cargo test --workspace                                 # async (tokio) ; libSQL compile via bindgen/cc
cargo clippy --workspace --all-targets -- -D warnings  # utile en local, mais ≠ matrice CI
cargo build -p basemyai-core --features embed          # active Candle (lourd)
cargo build -p basemyai-core --features crypto         # chiffrement libSQL — EXIGE CMake installé
cargo build --profile profiling -p basemyai-rest       # binaire optimisé MAIS symbolisable (perf/flamegraph)
```

Le **vecteur est natif libSQL** (compile sans CMake). Seule la feature `crypto` (chiffrement au repos) exige CMake. `embed` tire Candle (lourd).

**Profils** (définis dans le `Cargo.toml` racine) : `dev` est allégé (`debug = "line-tables-only"`) pour itérer vite malgré libSQL+Candle — backtraces panic conservées ; pour debugger sous gdb/lldb : `cargo build --config 'profile.dev.debug=2'`. `profiling` = release symbolisable (perf, flamegraphs).

**Bindings** : le code *test-only* (constructeurs `open_in_memory`) est gardé par `#[cfg(feature = "test-util")]` **avec sa registration** (bloc `#[napi]`/`#[pymethods]` séparé, ou helper gardé) — sinon le build par défaut casse (E0425 napi / `dead_code`). Le gate `--all-targets` ci-dessus couvre les deux bindings en config défaut.

## Invariants — NE JAMAIS violer

- **`basemyai-core` est agnostique métier.** Aucun `agent_id`, `valid_from/until`, couche mémoire, ni `Symbol/Edge` dedans. Ces concepts vivent dans `basemyai`. Un `grep -rE 'agent_id|valid_until|episodic|Symbol|Edge' crates/basemyai-core/src` doit retourner **zéro** (test d'agnosticité, ADR-001).
- **Mécanisme au core, sens au consommateur.** Le core expose `knn(q, k, filter?)` et un worker de tâches *injectées*. Le sens (temps, agent) se passe via `Filter` paramétré, jamais en dur dans le core.
- **L'`Embedder` ne télécharge jamais** et ne détecte jamais le matériel. Il reçoit chemin + `Device` résolus par `setup` (ADR-010). Zéro réseau hors setup explicite.
- **`Filter` est paramétré** : fragment SQL + valeurs liées (`?`). Les inputs d'agent vont dans `params`, jamais interpolés (anti-injection, ADR-006).
- **Chiffrement obligatoire dans `basemyai`** (libSQL feature `crypto`), optionnel dans `basemyai-core`.
- **Backend = libSQL** (ADR-011), **async**. Vecteur **natif** (`vector_top_k`, pas d'extension), chiffrement intégré. `Store` est async ; l'`Embedder` reste **sync** (CPU-bound). Pas de DB externe. **Candle**, pas ONNX/fastembed. Chemin futur : **moteur natif BaseMyAI** (ADR-024, remplace le chemin Turso — voir `docs/PLAN-NATIVE-ENGINE.md`) ; libSQL reste le défaut jusqu'à parité prouvée.

## Style Rust (2026, édition 2024)

- `thiserror` en lib, `#[non_exhaustive]` sur les enums d'erreur publiques.
- **Jamais `unwrap()`/`expect()` sans message** en code lib. Pas de `static mut`. Pas de `Mutex` std tenu à travers un `.await`.
- Getters sans préfixe `get_`. `&str` en paramètre plutôt que `String`.
- **`Arc::clone(&x)` plutôt que `x.clone()`** sur un ref-counted (duplication de pointeur explicite).
- Tout doit passer le gate clippy ci-dessus avant commit.

**Politique de lints** : curée et commentée dans `[workspace.lints]` du `Cargo.toml` racine ; chaque crate l'hérite via `[lints] workspace = true`. Elle **encode ces règles dans le compilateur** : `unwrap_used`, `await_holding_lock` (= « pas de `Mutex` à travers `.await` »), `todo`, `clone_on_ref_ptr` + famille clonage/perf. Les tests sont exemptés de `unwrap_used` via `clippy.toml` (`allow-unwrap-in-tests`). **`expect_used` n'est volontairement PAS activé** : la règle autorise `expect("message")`, or ce lint interdit tout `expect`. Ne pas l'ajouter.

## Layout (restructuré 12 juin 2026)

### `basemyai-core/src/`

```text
storage/          ← Recherche vectorielle + store libSQL
  ├─ mod.rs       ← re-exports
  ├─ store.rs     ← Store async, migrations, chiffrement
  └─ vector.rs    ← Filter, Value, Neighbor (types paramétrables)

embed/            ← Embeddings in-process
  ├─ mod.rs       ← Device enum, trait Embedder (object-safe)
  └─ candle.rs    ← CandleEmbedder BERT (feature "embed")

error.rs          ← CoreError (thiserror, #[non_exhaustive])
maintenance.rs    ← MaintenanceTask, MaintenanceWorker (injection)
lib.rs            ← re-exports + libsql
```

### `basemyai/src/`

```text
memory/           ← Domaine mémoire (4 couches, isolation, validité)
  ├─ mod.rs       ← façade Memory (remember, recall, recall_by_layer, invalidate, forget, stats, search_graph)
  ├─ layer.rs     ← MemoryLayer enum, Record, AgentStats
  ├─ schema.rs    ← Migrations SQL V1/V2/V3 (memory + graph), EMBEDDING_DIM
  └─ isolation.rs ← AgentId newtype (isolation SQL ADR-006)

cognition/        ← Pipeline Phase 2 (graphe + consolidation + inférence)
  ├─ mod.rs
  ├─ inference.rs ← trait LlmInference (object-safe, injecté)
  ├─ consolidation.rs ← consolidate(memory, llm) → faits + graphe
  └─ graph.rs     ← Graph {entity, edge}, traverse (CTE récursive)

provision/        ← Provisioning hardware-aware
  ├─ mod.rs       ← re-exports
  ├─ embedder.rs  ← detect_hardware(), provision(), SHA256 verification
  └─ llm.rs       ← KNOWN_MODELS, detect_llm_options(), choose_llm(), OpenAiCompatBackend (alias OllamaBackend)

maintenance/      ← Tâches de fond
  ├─ mod.rs       ← ConsolidationTask (Arc<Memory> + Arc<dyn LlmInference>)
  ├─ gc.rs        ← ExpiredMemoryGc (ADR-005)
  └─ forgetting.rs ← AdaptiveForgetting (importance × récence hyperbolique)

retrieval.rs      ← Racine : Ranking, Fused, rrf_fuse (pur méchanisme)
temporal.rs       ← Racine : Validity (valid_from/until), temporal_filter()
error.rs          ← Racine : MemoryError (thiserror)
lib.rs            ← re-exports
```

**Organisation par domaine sémantique, pas par artefact.** Chaque dossier
regroupe un concept métier et expose un API cohérent via `mod.rs`.

`crates/basemyai/tests/` : TBD (sera restructuré en accord avec src/).

## Statut (juin 2026)

Phase 1 (socle) ✅ et Phase 2 (Cognition) ✅ implémentées :

- **KNN réel** : distance cosine, oversampling ×8 quand filtre présent (ADR-012).
- **Graphe** : entités/relations, CTE récursive `UNION` (cycle-safe), scopée `agent_id` + profondeur (ADR-012).
- **RRF** : `rrf_fuse` multi-signal, k=60, déterministe (ADR-012).
- **Oubli adaptatif** : `AdaptiveForgetting`, décroissance hyperbolique `H/(H+age)` (pas exponentielle — libSQL n'a pas `pow`/`exp`) (ADR-012).
- **Consolidation** épisodes→faits : `consolidate(memory, llm)`, idempotente, `LlmInference` injecté (ADR-012).
- **LLM provision** : `KNOWN_MODELS` 20 modèles juin 2026, 8 backends détectés, `choose_llm()` hardware-aware (ADR-013).

Wiring consolidation dans `MaintenanceWorker` ✅ (`ConsolidationTask`, `maintenance/mod.rs`),
bindings PyO3/NAPI ✅ (`bindings/basemyai-py`, `bindings/basemyai-node`), sidecar MCP ✅
(`crates/basemyai-mcp`) et REST ✅ (`crates/basemyai-rest`) : tous implémentés et testés.

**État réel : voir `docs/status.md` (source de vérité, 2026-07-04).** Le moteur (Phase 1 + 2)
et les surfaces (MCP/REST/bindings/CLI) sont en place ; **crates.io et PyPI sont publiés**
(`0.1.0` confirmé le 2026-06-22), tandis que la publication npm de `basemyai` reste à re-vérifier
depuis cette machine (`npm view basemyai` renvoie `404`). Pas de binaire CLI distribué. CLI
`basemyai-cli` ✅ : cycle de vie mémoire complet
(`remember/recall/list/forget/invalidate/purge/export/import`), graphe, maintenance (`gc`/
`forget-adaptive`/`consolidate`), `config`, `completions` — voir `docs/cli.md`. Reste ouvert (M5) :
distribution binaire (cargo-dist), tests CLI en CI. `StorageEngine` : trait d'opérations mémoire
`basemyai::storage::MemoryStore` + `LibsqlMemoryStore` fait (ADR-020, 2026-06-20), `Filter` confiné,
tests de contrat ajoutés. Hardening (M6, 2026-07-02) : pool lecteur ✅ (ADR-021), bench KNN ✅
(10k/100k réels archivés, `docs/benchmarks/m6-knn-results-2026-07-01.md` — 1M documenté comme
non exécuté, coût de build de l'index natif ~78-79 ms/ligne), stress Candle 1h ✅ (stable, pas de
fuite, `docs/benchmarks/m6-candle-stress-results-2026-07-01.md`), key rotation ✅ (`PRAGMA rekey`,
`Store::rotate_key`/`Memory::rotate_key`). Live subscriptions vague 2 (ADR-022) : REST SSE ✅, MCP
notifications ✅, PyO3 callback ✅, NAPI/Node reporté (pas d'équivalent direct de l'itérateur async
Python en napi-rs). Reste ouvert : CUDA/NVML réel (M6), résultats KNN 1M, NAPI live subscriptions.

**Moteur natif (ADR-024/025, `docs/TODO-NATIVE-ENGINE.md`)** : N1 (spike LSM vs B-tree) ✅ et
**N2 (store durable) ✅ clos 2026-07-04** — `crates/basemyai-engine` (WAL+memtable+SST+recovery,
batches atomiques `apply_batch`, `WAL_RECORD_VERSION` 2), harnais crash-consistency kill réel en CI,
fuzzing (1 panic réel trouvé/corrigé), `format.lock` anti-drift, `EngineKind::Native` +
`NativeEngine` capability-only dans `basemyai-core` (feature `engine-native`), runner déclaratif
multi-backend `crates/basemyai/tests/memory_tests/` (vert sur Libsql, prêt pour Native).
**N3 (index vectoriel natif) ✅ clos 2026-07-05** : LM-DiskANN/Vamana pur Rust
(`idx/vector/{node,distance,graph,meta,persistent}.rs`, ADR-026), insert/delete/consolidate en
`apply_batch` atomiques, tombstones + réparation FreshDiskANN, rebuild depuis la donnée. Recall@10 =
1.0 partout (RAM, persistant, après churn, 10k/100k). Bench de parité M6
(`docs/benchmarks/n3-vector-parity-2026-07-05.md`) : les 3 seuils ADR-026 tenus avec grande marge —
requête 7.5 ms (10k) / 12.7 ms (100k) vs plafond ~48-49 ms libSQL ; build incrémental réel 5.7 ms/ligne
(10k) / 17.3 ms/ligne (100k) vs 78-79 ms/ligne libSQL (qui n'a jamais fini son build incrémental 100k
en 3h+, ce chiffre étant son taux de bulk-load, pas incrémental). Crash harness étendu aux deletes/
consolidate : 0 violation sur plusieurs runs de 20 cycles. **N4 (graphe natif) ✅ clos 2026-07-05** :
`idx/graph/{entity,edge,traverse,ram,persistent}.rs` — un nœud/une arête = un enregistrement KV
(`relation`/`dst` dans la clé, pas la valeur, pour un scan préfixé par nœud source à chaque saut
BFS), isolation par agent structurelle dans le layout de clé, `GraphEntity:1`/`GraphEdge:1` dans
`format.lock`. Traversée = portage littéral 1:1 de la CTE récursive SQL, les 5 scénarios de
`tests/graph.rs` rejoués fidèlement contre RAM et persistant (`tests/graph_parity.rs`), même code BFS
partagé entre les deux (zéro dérive possible). Pas de méta/rebuild (design assumé : aucun état de
navigation global à mettre en cache). Crash harness étendu mode `graph`, 20 cycles réels, 0 violation.
**N5.1 (`NativeMemoryStore` hors FTS/crypto) ✅ clos 2026-07-05** — découpage N5 acté par ADR-027 :
`idx/memory/` moteur (`MemoryRecord:1`/`MemoryVecMap:1`/`MemoryIndexMeta:1` dans `format.lock`,
allocateur `vec_id` monotone auto-guérissant), `PersistentVectorIndex::insert_with`/`delete_with`
(les enregistrements du consommateur montent dans le même `apply_batch` que l'index — un `remember`
natif = UN enregistrement WAL, atomicité transaction-libSQL retrouvée), `NativeMemoryStore` dans
`basemyai` (feature `engine-native`, jamais défaut) : parité requête par requête avec
`LibsqlMemoryStore` (oversampling ×8 ADR-012, non-filtres préservés), FTS/BM25 et métriques
non-cosinus en erreur franche (N5.2/N5.3). **Le diff multi-backend du runner N2 est prouvé** :
`backend_suite!` vert sur Libsql ET Native (`cargo test -p basemyai --features
test-util,engine-native --test memory_tests`), matrice xtask/CI étendue en miroir strict,
capacités `NativeEngine` honnêtes (`vectors`/`recursive_queries` → true).
**N5.2 (FTS/BM25 natif) ✅ clos 2026-07-06** (ADR-028) : troisième index moteur `idx/fts/`
(inversé `FtsPosting:1` + direct `FtsDocTerms:1` + stats par agent healables `FtsStats:1`),
tokenizer casefold+pliage d'accents (racinisation Porter différée, gap assumé et documenté) ;
`PersistentFts::stage_insert`/`stage_delete` composent dans le `Batch` de l'appelant, fusionnés
par `PersistentMemoryIndex::put`/`forget`/`purge_agent` dans le même `extra` batch que
vecteur+mémoire — un `remember` natif reste UN enregistrement WAL étendu au troisième index.
Scoring Okapi (`k1=1.2`/`b=0.75`, défauts FTS5), `df` dérivé du scan des postings.
`NativeMemoryStore::keyword_ranking_ids` branché (fin de l'erreur franche) — un bug de parité
(filtre de validité temporelle absent) trouvé et corrigé pendant l'implémentation. Deux
scénarios `backend_suite!` (classement par pertinence, validité temporelle + forget) rejoués
Libsql/Native, zéro divergence ; `EngineCapabilities::native().full_text` → `true` ; aucune
extension xtask/CI nécessaire (nouveau module sous des entrées déjà couvertes) ; `cargo xtask ci`
vert (18 étapes) + crash-consistency re-exécuté (4 modes, 0 violation). Reste N5.3→N5.6 :
100 % `storage_contract.rs`+`contracts.rs`, chiffrement au repos + rotation, barre M6
(concurrence au-delà du mono-écrivain sérialisé, crash harness mode `memory` dédié au triplet
FTS, `put_memory_batch` tout-ou-rien), ADR de bascule du défaut — décision humaine séparée.
