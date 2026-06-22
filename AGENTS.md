# BaseMyAI — guide agent

Moteur de mémoire local pour agents IA. **Deux crates** dans ce workspace :
`basemyai-core` (socle agnostique) + `basemyai` (sémantique mémoire), posée dessus.
Décisions : `ADR.md`. Relation d'écosystème : `../ECOSYSTEM_ARCHITECTURE.md`.

## Commandes

```bash
cargo test --workspace                                 # async (tokio) ; libSQL compile via bindgen/cc
cargo clippy --workspace --all-targets -- -D warnings  # LE gate qualité : doit passer
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
- **Backend = libSQL** (ADR-011), **async**. Vecteur **natif** (`vector_top_k`, pas d'extension), chiffrement intégré. `Store` est async ; l'`Embedder` reste **sync** (CPU-bound). Pas de DB externe. **Candle**, pas ONNX/fastembed. Chemin futur : Turso DB (pur Rust).

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

**État réel : voir `docs/status.md` (source de vérité, 2026-06-22).** Le moteur (Phase 1 + 2)
et les surfaces (MCP/REST/bindings/CLI) sont en place ; **crates.io et PyPI sont publiés**
(`0.1.0` confirmé le 2026-06-22), tandis que la publication npm de `basemyai` reste à re-vérifier
depuis cette machine (`npm view basemyai` renvoie `404`). Pas de binaire CLI distribué. CLI
`basemyai-cli` ✅ : cycle de vie mémoire complet
(`remember/recall/list/forget/invalidate/purge/export/import`), graphe, maintenance (`gc`/
`forget-adaptive`/`consolidate`), `config`, `completions` — voir `docs/cli.md`. Reste ouvert (M5) :
distribution binaire (cargo-dist), tests CLI en CI. `StorageEngine` : trait d'opérations mémoire
`basemyai::storage::MemoryStore` + `LibsqlMemoryStore` fait (ADR-020, 2026-06-20), `Filter` confiné,
tests de contrat ajoutés. Hardening (M6 : bench KNN, stress test, pool, key rotation) non commencé.
