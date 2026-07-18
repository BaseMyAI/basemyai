# BaseMyAI — guide agent

Moteur de mémoire local pour agents IA. Le cœur du workspace est le **duo**
`basemyai-core` (socle agnostique) + `basemyai` (sémantique mémoire), posée dessus.
Le workspace compte **6 crates membres** (`crates/` : `basemyai-core`, `basemyai`,
`basemyai-cli`, `basemyai-mcp`, `basemyai-rest`, `basemyai-engine` — moteur de
stockage maison, ADR-024/025, non publié) + **2 bindings**
(`bindings/basemyai-py`, `bindings/basemyai-node`) + l'outil DX `xtask/`.
S'y ajoute `crates/basemyai-eval` (Recall Quality Lab, éval déterministe offline) :
**volontairement HORS workspace racine** (son `Cargo.toml` déclare son propre
`[workspace]`, `publish = false`) tant que le câblage workspace/xtask/CI n'est
pas acté — cf. `eval/README.md` ; il se lance via `--manifest-path`
(`docs/recall-quality-lab.md`).
Décisions : `docs/ADR.md` (index, un fichier par ADR sous `docs/adr/`). Relation d'écosystème : `../ECOSYSTEM_ARCHITECTURE.md`.

**État 2026-07-08 (ADR-033)** : workspace **100 % moteur natif**.
libSQL/V1, `Store`, `LibsqlMemoryStore`, feature `crypto` libSQL et
double-backend sont retirés du code actif.

## Commandes

**LE point d'entrée qui reproduit la CI est `cargo xtask`** (matrice exacte de
`.github/workflows/ci.yml` : par-crate, avec les bonnes combinaisons de features).

```bash
cargo xtask check        # fmt --check + clippy par crate (features CI), -D warnings
cargo xtask test         # tests par crate, config légère (sans embed)
cargo xtask ci           # check + test — LE gate avant commit
cargo xtask test-embed   # job CI `embed` (Candle, lourd)
cargo xtask format-lock  # anti-drift basemyai-engine/format.lock (inclus dans check/ci)
cargo xtask test-crash-consistency  # kill-loop réel basemyai-engine (~20 cycles, job CI dédié)
```

⚠ **`cargo clippy --workspace` ne reproduit pas la CI** (la CI clippy/teste chaque
crate avec des features précises, ex. `-p basemyai-mcp --no-default-features
--features stdio,http,test-util`) — utiliser `cargo xtask check`. Commandes cargo
brutes, gardées en référence :

```bash
cargo test --workspace                                 # async (tokio)
cargo clippy --workspace --all-targets -- -D warnings  # utile en local, mais ≠ matrice CI
cargo build -p basemyai-core --features embed          # active Candle (lourd)
cargo build --profile profiling -p basemyai-rest       # binaire optimisé MAIS symbolisable (perf/flamegraph)
```

Le backend est **natif BaseMyAI** (`basemyai-engine`) ; `embed` tire Candle (lourd).

**Profils** (définis dans le `Cargo.toml` racine) : `dev` est allégé (`debug = "line-tables-only"`) pour itérer vite malgré Candle — backtraces panic conservées ; pour debugger sous gdb/lldb : `cargo build --config 'profile.dev.debug=2'`. `profiling` = release symbolisable (perf, flamegraphs).

**Bindings** : le code *test-only* (constructeurs `open_in_memory`) est gardé par `#[cfg(feature = "test-util")]` **avec sa registration** (bloc `#[napi]`/`#[pymethods]` séparé, ou helper gardé) — sinon le build par défaut casse (E0425 napi / `dead_code`). Le gate `--all-targets` ci-dessus couvre les deux bindings en config défaut.

## Invariants — NE JAMAIS violer

- **`basemyai-core` est agnostique métier.** Aucun `agent_id`, `valid_from/until`, couche mémoire, ni `Symbol/Edge` dedans. Ces concepts vivent dans `basemyai`. Un `grep -rE 'agent_id|valid_until|episodic|Symbol|Edge' crates/basemyai-core/src` doit retourner **zéro** (test d'agnosticité, ADR-001).
- **Mécanisme au core, sens au consommateur.** Le core expose les primitives moteur (`StorageEngine`, capacités, chiffrement natif) et un worker de tâches *injectées*. Le sens (temps, agent) reste côté `basemyai`.
- **L'`Embedder` ne télécharge jamais** et ne détecte jamais le matériel. Il reçoit chemin + `Device` résolus par `setup` (ADR-010). Zéro réseau hors setup explicite.
- **Pas de surface SQL-leaky dans le produit** : ni `Filter`, ni `Value`, ni `Store`, ni `LibsqlMemoryStore`.
- **Chiffrement obligatoire dans `basemyai`** via l'enveloppe native ADR-030 (pas de dépendance CMake).
- **Backend unique = moteur natif BaseMyAI** (ADR-024/025/026/027/028/030/032). Pas de fallback libSQL.

## Style Rust (2026, édition 2024)

- `thiserror` en lib, `#[non_exhaustive]` sur les enums d'erreur publiques.
- **Jamais `unwrap()`/`expect()` sans message** en code lib. Pas de `static mut`. Pas de `Mutex` std tenu à travers un `.await`.
- Getters sans préfixe `get_`. `&str` en paramètre plutôt que `String`.
- **`Arc::clone(&x)` plutôt que `x.clone()`** sur un ref-counted (duplication de pointeur explicite).
- Tout doit passer le gate clippy ci-dessus avant commit.

**Politique de lints** : curée et commentée dans `[workspace.lints]` du `Cargo.toml` racine ; chaque crate l'hérite via `[lints] workspace = true`. Elle **encode ces règles dans le compilateur** : `unwrap_used`, `await_holding_lock` (= « pas de `Mutex` à travers `.await` »), `todo`, `clone_on_ref_ptr` + famille clonage/perf. Les tests sont exemptés de `unwrap_used` via `clippy.toml` (`allow-unwrap-in-tests`). **`expect_used` n'est volontairement PAS activé** : la règle autorise `expect("message")`, or ce lint interdit tout `expect`. Ne pas l'ajouter.

## Layout (restructuré 12 juin 2026, mis à jour ADR-033)

### `basemyai-core/src/`

```text
storage/          ← Primitives moteur natif (ADR-024/025/030)
  ├─ mod.rs       ← re-exports
  ├─ engine.rs    ← StorageEngine, EngineCapabilities, EngineKind::Native
  ├─ native.rs    ← NativeEngine (wrapper capability-only)
  ├─ key.rs       ← EncryptionKey
  └─ vector.rs    ← Metric (cosine/euclidean/hamming — mécanisme pur)

embed/            ← Embeddings in-process
  ├─ mod.rs       ← Device enum, trait Embedder (object-safe)
  └─ candle.rs    ← CandleEmbedder BERT (feature "embed")

error.rs          ← CoreError (thiserror, #[non_exhaustive])
maintenance.rs    ← MaintenanceTask, MaintenanceWorker (injection)
lib.rs            ← re-exports publics
```

### `basemyai-engine/src/` (non publié, BUSL-1.1)

```text
store/            ← WAL + memtable + SST, apply_batch
idx/vector/       ← LM-DiskANN (ADR-026)
idx/graph/        ← entités/arêtes + BFS (ADR-027/N4)
idx/memory/       ← records mémoire + vecmap
idx/fts/          ← BM25 inversé (ADR-028)
format/           ← wire formats versionnés + format.lock
```

### `basemyai/src/`

```text
memory/           ← Domaine mémoire (4 couches, isolation, validité)
  ├─ mod.rs       ← façade Memory (remember, recall, recall_by_layer, invalidate, forget, stats, search_graph, compile_context)
  ├─ layer.rs     ← MemoryLayer enum, Record, AgentStats
  ├─ porting.rs   ← export/import JSONL
  ├─ event.rs     ← MemoryEvent (ADR-022)
  ├─ trust.rs     ← TrustLevel (provenance ADR-036)
  ├─ testutil.rs  ← HashEmbedder (feature test-util)
  └─ isolation.rs ← AgentId newtype (isolation structurelle ADR-006)

cognition/        ← Pipeline Phase 2 (graphe + consolidation + inférence)
  ├─ mod.rs
  ├─ inference.rs ← trait LlmInference (object-safe, injecté)
  ├─ consolidation.rs ← consolidate(memory, llm) → faits + graphe
  └─ graph.rs     ← Graph {entity, edge}, traverse (BFS natif)

context/          ← Context Engine (PLAN-CONTEXT-ENGINE.md — ⚠ non committé)
  ├─ mod.rs       ← façade + validation, re-exports
  ├─ types.rs     ← ContextRequest/ContextBundle/citations/exclusions
  ├─ token.rs     ← estimation de tokens (budget dur)
  ├─ compile.rs   ← filtres, normalisation, déduplication
  ├─ selection.rs ← utility ranking, quotas, sélection sous budget
  ├─ temporal.rs  ← statut de validité, fraîcheur
  └─ render.rs    ← sections, Markdown, citations, métriques

storage/          ← Contrat MemoryStore
  ├─ native_store/ ← NativeMemoryStore, module-répertoire (mod.rs, trait_impl.rs, inner.rs, porting.rs)
  └─ integrity.rs ← verify/repair/rebuild-indexes/reembed (ADR-040)

provision/        ← Provisioning hardware-aware
  ├─ mod.rs       ← re-exports
  ├─ embedder.rs  ← detect_hardware(), provision(), SHA256 verification
  └─ llm.rs       ← KNOWN_MODELS, detect_llm_options(), choose_llm()

maintenance/      ← Tâches de fond injectées
  ├─ mod.rs       ← ConsolidationTask + AdaptiveForgettingTask + ExpiredMemoryGcTask (ADR-037/038)
  ├─ adaptive_forgetting.rs ← oubli adaptatif borné (ADR-037/041)
  └─ expired_gc.rs ← GC temporel (ADR-038/041)

retrieval.rs      ← Racine : Ranking, Fused, rrf_fuse (pur méchanisme)
temporal.rs       ← Racine : Validity (valid_from/until), temporal_filter()
config.rs         ← Racine : ConfigDefaults (⚠ non committé)
error.rs          ← Racine : MemoryError (thiserror)
lib.rs            ← re-exports
```

**Organisation par domaine sémantique, pas par artefact.** Chaque dossier
regroupe un concept métier et expose un API cohérent via `mod.rs`.

## Statut (2026-07-17)

**Source de vérité détaillée : `docs/status.md`.**

- **Moteur natif : ✅ clos jusqu'à N11 inclus** — N0→N5.6 (ADR-033, backend unique :
  LSM, vecteur LM-DiskANN, graphe, FTS/BM25, chiffrement ADR-030), hardening
  N7→N10 (SST par blocs ADR-039, verify/repair/reembed ADR-040, maintenance
  scalable ADR-041), N11 (fuzz 24 cibles, model-based, pannes I/O, soak 1M
  archivé 2026-07-15). HEAD = clôture N11.
- **Chantier actif : N12/ADR-042** (passphrase Argon2id, zeroization, rotation
  complète DEK) — code présent **non committé** dans le working tree, non clos
  (voir `docs/status.md` §10).
- **Nouveaux, non committés :** Context Engine (`basemyai::context`,
  `compile_context` — plan `docs/PLAN-CONTEXT-ENGINE.md`, R1.4 + socle R1.5 +
  SDK py/node livrés) et Recall Quality Lab (`crates/basemyai-eval`, standalone
  hors workspace — `docs/recall-quality-lab.md`).
- **Phase 1 + 2 mémoire/cognition : ✅** — API `Memory`, graphe, RRF, consolidation,
  oubli adaptatif + GC temporel (ADR-037/038), provisioning LLM.
- **Surfaces : ✅** — MCP, REST, CLI, bindings PyO3/NAPI (live watch inclus sur
  les quatre surfaces).
- **CI :** `cargo xtask ci` (+ `test-embed`, `test-crash-consistency` séparés ;
  workflows `nightly.yml` + `soak-campaign.yml`).
- **Publication :** `0.1.0` crates.io/PyPI (libSQL). **Prochaine : `0.2.0` native-only**
  (breaking — version workspace déjà bumpée, release pas faite). npm à vérifier ;
  cargo-dist configuré, `dist build`/publication = décision humaine.
- **Reste ouvert :** committer/stabiliser N12 + context + eval (tests cassés au
  2026-07-17, réparation en cours), release 0.2.0, R1.6→R1.8 + R2.x du plan
  context, build Docker REST à vérifier, validation CUDA/NVML sur GPU réel.
- **V2 :** sync P2P, langage de requête, multi-modèles, Studio/Tauri.
