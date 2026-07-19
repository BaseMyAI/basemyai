# BaseMyAI — Implementation Status Matrix

**Mise à jour 2026-07-19** (les entrées ci-dessous datées 2026-07-17 décrivent
un working tree non committé qui a depuis été committé — lire cette entrée
en premier, le reste du fichier garde l'historique tel quel) :

- Le working tree audité le 2026-07-17 (~111 fichiers, compilation cassée) est
  **committé** en 6 commits sur `dev` : `939c922` (eval), `fccdca2`
  (N12/ADR-042), `0312fef` (Context Engine), `a0cf48d` (bindings py/node),
  `1bc291d` (doc), `a3842e7` (fix CI, voir plus bas).
- La « compilation cassée » du 2026-07-17 était en réalité un `target/`
  périmé : artefacts compilés avant la réorganisation du dépôt vers
  `basemyai-orga` (ancien chemin `…/forgemyai/basemyai` encore embarqué dans
  des binaires de test via `CARGO_MANIFEST_DIR`, jamais invalidés par cargo).
  Un `cargo clean` complet + rebuild confirme **`cargo xtask ci` vert**.
- En nettoyant, un vrai trou de gate a été trouvé et corrigé (`a3842e7`) :
  `crates/basemyai-engine/tests/adr042_contract.rs` et le fuzz target
  `crypto_meta_decode_structured` compilaient (couverts par
  `clippy --all-targets`) mais ne s'exécutaient **jamais** — absents de la
  liste explicite `--test` de `xtask` et de la matrice `fuzz.yml`. Même
  défaut que celui déjà documenté en §8.3 (nouveaux fichiers de test/fuzz non
  auto-découverts, câblage manuel systématiquement oublié).
- **N12/ADR-042 : audit des 13 critères de sortie (§5 de l'ADR) fait
  ligne par ligne contre le code réel** — 12/13 couverts et testés. Seul gap
  réel : aucun test ne copie explicitement un ancien `crypto.meta` à côté
  d'une **nouvelle** génération pour vérifier que l'AEAD échoue sur chaque
  octet WAL/SST (le binding AAD est prouvé au niveau primitif et au niveau
  génération/pointeur, mais pas par ce scénario bout-en-bout précis). Voir
  §10 pour le détail mis à jour.
- **Context Engine R1.6/R1.7 (profils, formats de rendu, trace/explicabilité)
  sont en réalité déjà livrés et testés côté Rust** — la version du 2026-07-17
  de ce fichier et de `docs/PLAN-CONTEXT-ENGINE.md` les disait « ouverts »,
  ce qui était faux (code trouvé dans `context/types.rs`/`selection.rs`/
  `render.rs`, testé bout-en-bout par `tests/context.rs`). Ce qui est
  réellement vrai : `lib.rs` n'exporte pas ces types à la racine du crate, et
  les bindings Python/Node n'exposent que le sous-ensemble R1.0-R1.5 (aucune
  trace de profil/format de rendu/rôles/dédup complet/avertissements/trace
  détaillée). CLI, MCP et REST n'ont aucune surface `compile_context`. Voir §12.

**Date : 2026-07-17** (dernière mise à jour : clôture N11 — campagne 1M archivée
2026-07-15 —, chantier **N12/ADR-042 en cours non committé**, nouveaux Context
Engine + Recall Quality Lab non committés ; le fichier était daté 2026-07-08 et
ne couvrait rien de tout cela)
**Statut : SOURCE DE VÉRITÉ.** Ce fichier réconcilie les contradictions entre les
docs internes (TODO.md — archivé depuis sous `docs/archive/TODO-2026-06.md` —,
CLAUDE.md, VISION.md, ADR-019, la recherche stratégique
2026-06-18). Il a été recommandé par la recherche stratégique
(`docs/strategy/2026-06-18-agent-memory-database-research.md`, « Concrete Next
Steps Before Refactor », item 3) précisément parce que certaines docs disent
« Phase 2 implémentée » tandis que d'autres parlent de roadmap.

**Méthode :** chaque ligne est ancrée dans une vérification du code réel, pas
dans une déclaration de doc. Quand une doc et le code divergent, le **code fait
foi** et l'écart est noté.

**Mise à jour ADR-033 (2026-07-08)** :

- workspace actif **100 % moteur natif** ;
- libSQL/V1, `Store`, `LibsqlMemoryStore`, feature `crypto` libSQL et
  dual-backend supprimés ;
- `.bmai` actif = format natif ;
- chiffrement au repos = enveloppe native ADR-030.

**Mise à jour 2026-07-17** (raison : réconcilier status.md avec l'état réel du
code, y compris l'état **non committé** du working tree — audit fichier par
fichier, HEAD = `d58923a`, clôture N11) :

- **N7→N11 (production-hardening) : tous clos** — dernier jalon : campagne 1M
  archivée 2026-07-15 (`docs/benchmarks/n11-soak-1m-2026-07-15.md`) ;
- **chantier actif : N12/ADR-042** (passphrase Argon2id, zeroization, rotation
  complète DEK) — ADR **Accepted 2026-07-15**, code présent dans le working tree
  mais **non committé, non clos** (voir §10) ;
- **nouveaux, non committés** : **Context Engine** (`basemyai::context`,
  `compile_context`) et **Recall Quality Lab** (`crates/basemyai-eval`,
  crate **standalone hors workspace racine**) — voir §12 ;
- ⚠ **le working tree contient ~90 fichiers modifiés/ajoutés non committés**
  (N12 + context + eval + surfaces passphrase CLI/bindings). Au moment de cet
  audit (2026-07-17), **la compilation des tests du workspace était cassée**
  (réparation en cours par ailleurs) — les lignes marquées « non committé »
  ci-dessous décrivent du **code présent**, pas un état vert en CI.

**Légende statut :**

- ✅ **Implemented** — code présent ET testé dans le repo.
- 🟡 **Partial** — code présent mais incomplet, non testé end-to-end, ou
  dépendant d'un chemin non couvert par la CI.
- 📋 **Planned** — pas encore de code ; tâche ouverte (backlog moteur natif :
  `docs/TODO-NATIVE-ENGINE.md` ; historique : `docs/archive/TODO-2026-06.md`).
- ⏸️ **Deferred** — explicitement repoussé en V1.5 / V2 par ADR-019 ou VISION.

**Distinction critique :** « le code existe » ≠ « publié / testé cross-platform /
prêt prod ». La colonne Notes le précise systématiquement.

---

## 1. Core storage / engine (`basemyai-core` + `basemyai-engine`)

| Domaine / Feature | Statut | Preuve (vérifiée) | Notes |
|---|---|---|---|
| Moteur natif LSM (`basemyai-engine`) | ✅ | `crates/basemyai-engine/` ; WAL+memtable+SST, `apply_batch`, `format.lock` ; `cargo xtask test-crash-consistency` | Fondation ADR-025. Backend **unique** depuis ADR-033 (2026-07-08). |
| `NativeEngine` + `StorageEngine` trait | ✅ | `basemyai-core/src/storage/{engine,native}.rs` ; `EngineKind::Native` | Capacités honnêtes (`vectors`, `full_text`, `recursive_queries`, `encrypted`). |
| Index vectoriel LM-DiskANN | ✅ | `basemyai-engine/src/idx/vector/` ; `tests/vector_recall.rs`, `vector_churn.rs` | Recall@10 = 1.0 (10k/100k, persistant, après churn). ADR-026. |
| Index graphe natif (BFS) | ✅ | `basemyai-engine/src/idx/graph/` ; `tests/graph_parity.rs` | Isolation agent structurelle dans le layout de clé. ADR-027/N4. |
| Index FTS/BM25 natif | ✅ | `basemyai-engine/src/idx/fts/` ; scénarios `memory_tests` | Tokenizer casefold+accents ; Porter différé (gap documenté). ADR-028. |
| `MemoryStore` / `NativeMemoryStore` | ✅ | `basemyai/src/storage/native_store/` (module-répertoire : `mod.rs`, `trait_impl.rs`, `inner.rs`, `porting.rs` — committé) ; `tests/memory_tests.rs`, `storage_contract.rs` | Contrat ADR-020 ; unique implémentation active. Clair + chiffré (`native_encrypted`). |
| Chiffrement au repos natif | ✅ | ADR-030 ; `crypto.meta`, enveloppes WAL/SST ; `NativeMemoryStore::rotate_key` | XChaCha20-Poly1305 pur Rust, **pas de CMake**. Rotation O(1) (re-scellement DEK). **Extension N12/ADR-042 clos** (passphrase Argon2id, zeroization, rotation complète) — voir §10. |
| Métrique vectorielle (`Metric` enum) | ✅ | `basemyai-core/src/storage/vector.rs` | Cosine/Euclidean/Hamming — mécanisme pur, sans SQL. |
| `MaintenanceWorker` + tâches injectées | ✅ | `maintenance.rs` ; `tests/maintenance_worker.rs` (dans `basemyai`) | Mécanisme d'injection ; le sens (GC, oubli, consolidation) vit dans `basemyai`. |
| Embedder trait (object-safe, sync) | ✅ | `embed/mod.rs` (`Embedder`, `Device`) ; `tests/embed.rs` | Ne télécharge jamais (invariant ADR-010). |
| Candle BERT (`CandleEmbedder`, `all-MiniLM-L6-v2`, 384d) | ✅ | `embed/candle.rs` (feature `embed`) ; job CI `embed` ; `docs/benchmarks/m6-candle-stress-results-2026-07-01.md` | Stress 1h stable (2026-07-02). Lourd (Candle) — job CI `embed` séparé. |
| Agnosticité du core (zéro `agent_id`/`Symbol`/`Edge`) | ✅ | `tests/agnosticity.rs`, `tests/contracts.rs` | Invariant ADR-001 testé. |
| libSQL / `Store` / `Filter` SQL | ⛔ retiré | — | Supprimé du workspace actif (ADR-033). Référence historique : ADR-011 (superseded). |

---

## 2. Memory API (`basemyai`)

Toutes les méthodes listées dans `TODO.md` M0.1 sont implémentées **et dépassées**
(le code expose plus que ce que TODO annonce).

| Méthode / Feature | Statut | Preuve (vérifiée) | Notes |
|---|---|---|---|
| `remember`, `remember_with` | ✅ | `memory/mod.rs:103,117` ; `tests/memory.rs` | Insertion atomique memory + miroir FTS. |
| `remember_batch`, `remember_batch_with` | ✅ | `memory/mod.rs:131,143` | Une passe d'embedding, une txn. **Non listé dans TODO** — code plus avancé que la doc. |
| `recall` (temporel, met à jour `last_access`) | ✅ | `memory/mod.rs:170` ; `tests/memory.rs` | Filtre agent + validité combiné. |
| `recall_by_layer` | ✅ | `memory/mod.rs:419` | |
| `recall_with_metric` (Cosine/Euclidean/Hamming) | ✅ | `memory/mod.rs:236` | **Non listé dans TODO.** |
| `recall_hybrid` (vecteur + BM25 fusionnés RRF) | ✅ | `memory/mod.rs` ; index FTS natif ADR-028 | ADR-014. Hybride vecteur + BM25 natif, fusion RRF. |
| `invalidate` (soft-delete, `valid_until = now`) | ✅ | `memory/mod.rs:483` ; `tests/contracts.rs` | |
| `forget` (suppression physique, RGPD) | ✅ | `memory/mod.rs:500` | memory + FTS atomique. |
| `purge_agent` (purge totale agent) | ✅ | `memory/mod.rs:525` | **Non listé dans TODO.** memory+entity+edge+fts. |
| `stats() -> AgentStats` | ✅ | `memory/mod.rs:545` | GROUP BY layer, valides seulement. |
| `search_graph` (KNN borné aux entités) | ✅ | `memory/mod.rs:588` | |
| `graph()` façade | ✅ | `memory/mod.rs:76` | Même store, même agent. |
| Chiffrement obligatoire (`open` échoue sans clé sur fichier) | ✅ | `memory/mod.rs:50-53` ; `tests/contracts.rs` | ADR-007. `:memory:` exempté. |
| 4 couches (`short_term`, `episodic`, `procedural`, `semantic`) | ✅ | `memory/layer.rs` | `procedural` présent mais simple (cf. recherche stratégique). |
| Isolation agent (`AgentId` newtype) | ✅ | `memory/isolation.rs` ; `tests/contracts.rs`, `tests/memory.rs` | Invariant sécurité ADR-006. |
| Import / export (`porting`) | ✅ | `memory/porting.rs` (`ImportReport`) ; `tests/porting.rs` | **Non listé dans TODO.** |

---

## 3. Cognition — graphe / consolidation / oubli (Phase 2)

| Feature | Statut | Preuve (vérifiée) | Notes |
|---|---|---|---|
| Graphe entités/relations (`add_entity`, `add_edge`, `traverse` BFS natif) | ✅ | `cognition/graph.rs` ; `tests/graph.rs` ; `basemyai-engine/src/idx/graph/` | Scopé agent + profondeur, cycle-safe. Index KV natif (plus de tables SQL). |
| Consolidation épisodes→faits (`consolidate`) | ✅ | `cognition/consolidation.rs` ; `tests/consolidation.rs`, `tests/consolidation_e2e.rs` | Idempotente. `LlmInference` injecté. ADR-012/ADR-018. |
| Trait `LlmInference` (object-safe, injecté) | ✅ | `cognition/inference.rs` | Modèle jamais codé en dur. |
| Oubli adaptatif (`AdaptiveForgetting`) | ✅ | `maintenance/adaptive_forgetting.rs` ; CLI `forget-adaptive` ; `tests/maintenance_worker.rs` | Porté sur le moteur natif par ADR-037, **borné en mémoire** par ADR-041 §7.3 (deux passes paginées sur `Engine::scan_range_page`, tas de survivants `O(capacity)` — plus aucune matérialisation de l'agent complet). Périmètre affiné : n'agit que sur les souvenirs **actifs** (les invalidés/expirés relèvent du GC temporel, ADR-038 — ensembles disjoints par construction). Éviction par lots atomiques bornés depuis ADR-041 §7.4. |
| GC mémoires expirées (`ExpiredMemoryGc`) | ✅ | `maintenance/expired_gc.rs` ; CLI `gc` ; `tests/maintenance_worker.rs` | Porté sur le moteur natif par ADR-038, requête d'index temporel ADR-041 §7.2, éviction **par lots atomiques bornés** ADR-041 §7.4 (`MemoryStore::forget_many`). Idempotent, reprennable après interruption. |
| Wiring consolidation/oubli/GC dans `MaintenanceWorker` | ✅ | `maintenance/mod.rs` (`ConsolidationTask`, `AdaptiveForgettingTask`, `ExpiredMemoryGcTask`) ; `tests/maintenance_worker.rs` | Trois tâches de maintenance actives post-ADR-037/038, même pattern d'injection (`Arc<Memory>` auto-suffisant). |

> **Note de positionnement.** La recherche stratégique 2026-06-18 (Risks) avertit
> que « trop de LLM/consolidation en V1 peut détourner du noyau memory DB » et
> classe la consolidation/provenance avancée en V2. Le code Phase 2 **existe et
> est testé** ; c'est une décision produit, pas un manque technique, de savoir si
> on l'expose comme feature V1 phare ou comme capacité avancée.

---

## 4. Provisioning hardware-aware

| Feature | Statut | Preuve (vérifiée) | Notes |
|---|---|---|---|
| Détection hardware (RAM, cœurs, VRAM) | 🟡 | `provision/embedder.rs` : `nvidia-smi`/`system_profiler` (toujours compilé) + NVML via `nvml-wrapper` derrière la feature optionnelle `cuda-detect` (`detect_nvml_gpus`, `HardwareProfile.gpus: Vec<GpuInfo>`) | NVML donne nombre de GPU + VRAM totale/libre par device, prime sur `nvidia-smi` quand actif. Best-effort : NVML/driver absent (poste sans GPU NVIDIA) ne panique jamais, `gpus` reste vide. Feature hors gate CI léger (`cargo xtask ci`) par choix — compilée/testée via `cargo test -p basemyai --features cuda-detect`. **Non validé sur GPU NVIDIA réel** (dev sans matériel dispo) : seuls compilation + chemin fallback sans GPU sont couverts en CI. |
| Fetch HTTP du modèle + vérif SHA-256 | ✅ | `provision/embedder.rs:225` (`reqwest`), `download_and_verify`, `EXPECTED_SHA256` (3 hashes ancrés) ; `tests/provisioning.rs` | Jamais d'auto-download silencieux (consentement explicite). |
| Persistance config (`provision.json`) | ✅ | `provision/embedder.rs` (`PersistedProvision`) | Rechargée au démarrage. |
| Détection LLM locale (`KNOWN_MODELS`, backends) | ✅ | `provision/llm.rs` ; `tests/llm_provision.rs` | `choose_llm()` hardware-aware, `OpenAiCompatBackend` (alias Ollama). ADR-013. |

---

## 5. Surfaces / SDK

| Surface | Statut | Preuve (vérifiée) | Notes |
|---|---|---|---|
| **Rust SDK** (crate `basemyai`) | ✅ | `Cargo.toml` (`version = 0.1.0`, keywords/categories) ; `examples/rust/*` ; `cargo search basemyai --limit 10` | API complète, examples présents, `cargo doc` propre. **Publication confirmée sur crates.io** le 2026-06-22 (`basemyai = "0.1.0"` et `basemyai-core = "0.1.0"`). |
| **MCP server** (`basemyai-mcp`) | ✅ | `crates/basemyai-mcp/` ; outils `remember/recall/recall_hybrid/recall_graph/invalidate/consolidate/consolidate_apply/stats` ; `tests/server.rs`, `tests/sampling.rs` | Transports stdio + HTTP, auth, audit, sampling (ADR-018). **Surface la plus aboutie** — cohérent avec « MCP prioritaire » de la recherche stratégique. Non listé comme milestone TODO (TODO ne mentionne que REST en M4). |
| **REST sidecar** (`basemyai-rest`) | ✅ | `crates/basemyai-rest/src/routes.rs` ; `tests/api.rs` ; `crates/basemyai-rest/Dockerfile` | axum, `/v1/remember,recall,recall_hybrid,recall_graph`, delete memory/agent, stats ; auth Bearer (constant-time), request-id, body limit. **Plus avancé que TODO M4.** `crates/basemyai-rest/openapi.yaml` = spec source. **Image Docker ajoutée** (2026-07-10) : multi-stage `rust:1.95-slim-bookworm` (build) → `debian:bookworm-slim` (runtime), `docker-compose.yml` à la racine. Dockerfile écrit et revu (build essentiel pour `tokenizers`/onig-sys) mais **build Docker non exécuté** dans cet environnement (Docker indisponible) — à vérifier par l'utilisateur avec `docker build -f crates/basemyai-rest/Dockerfile -t basemyai-rest:latest .`. |
| **Node binding** (`bindings/basemyai-node`, NAPI-RS) | 🟡 | `bindings/basemyai-node/src/memory.rs`, `index.d.ts` ; `__tests__/roundtrip.test.js`, `__tests__/watch.test.js` ; workflow `node-prebuilds.yml` | Classe `Memory` complète (remember, recall, recallByLayer, recallHybrid, invalidate, forget, stats, addGraphEntity/Edge, recallGraph, **watch**). **`index.d.ts` était périmé** (committé avant `watch`/`WatchHandle`/`MemoryEventPayload` **et** avant `includeProcedural`/`excludeImported` sur `recall`/`recallHybrid`, ADR-034/035/036) — régénéré 2026-07-10 via `npm run build` (release, `--features embed`), `npm run typecheck` + `npm test` verts derrière. **Publication npm non confirmée depuis cette machine** au 2026-06-22 : `npm view basemyai` et le registre public renvoient `404` pour `basemyai`. Vérifier le nom/scope final si besoin. |
| **Python binding** (`bindings/basemyai-py`, PyO3) | 🟡 | `bindings/basemyai-py/src/memory.rs`, `python/basemyai/__init__.pyi` ; `tests/test_roundtrip.py` ; workflow `python-wheels.yml` ; `python -m pip index versions basemyai` | Classe `Memory` async complète + stubs `.pyi` + `py.typed`. **`__init__.pyi` était périmé** : `watch()`/`MemoryWatch`/`WatchEvent` existent côté Rust (`src/memory.rs`, `src/types.rs`) depuis le live-subscriptions PyO3 mais étaient **absents du stub** — ajoutés 2026-07-10, alignés sur la forme réelle (`agent_id`/`kind`/`layer`/`id`). **Publication confirmée sur PyPI** (`basemyai 0.1.0` vu le 2026-06-22). Wrappers LangChain/LlamaIndex toujours absents. |
| **Live subscriptions** (ADR-022 vague 2 : SSE/WS REST, notifications MCP, callbacks PyO3/NAPI) | ✅ | `basemyai-rest/src/routes.rs` (`GET /v1/watch`, SSE) ; `basemyai-mcp/src/tools/watch.rs` (notification `notifications/message`) ; `bindings/basemyai-py/src/memory.rs` (`Memory.watch` → `async for`) ; `bindings/basemyai-node/src/memory.rs` (`Memory.watch(agentId, layer, callback) -> Promise<WatchHandle>`, `ThreadsafeFunction`) ; tests adversariaux d'isolation par surface | Fait 2026-07-02 (REST/MCP/Py) puis 2026-07-10 (Node). REST, MCP et Node testés avec isolation adversariale agent A/B (Node : `__tests__/watch.test.js`, `npm test` vert). PyO3 vérifié via `maturin develop` + pytest réel — un vrai bug Windows trouvé et documenté (crash access-violation en annulant un future en attente sur `broadcast::Receiver::recv()` via `asyncio.wait_for`, cf. `docs/archive/TODO-2026-06.md`). **NAPI** : pas d'équivalent direct du protocole itérateur async Python en napi-rs — conception distincte via `ThreadsafeFunction` (callback JS) + `WatchHandle` napi (poignée d'annulation, `close()` idempotent + `Drop`) pour ne pas fuir la tâche tokio de relais. |

> **Écart TODO.** `TODO.md` décrit M2 (Node) et M3 (Python) comme « à créer »
> sous `crates/basemyai-node` / `crates/basemyai-python`. En réalité les deux
> bindings **existent déjà**, sous `bindings/`, avec méthodes, tests et workflows
> de prebuild. Le plan M0→M7 est en retard sur le code.

---

## 6. CLI (`basemyai`)

| Feature | Statut | Preuve (vérifiée) | Notes |
|---|---|---|---|
| Crate `basemyai-cli` (clap) | ✅ | `crates/basemyai-cli/` (binaire `basemyai`) ; `cargo xtask check` vert (2026-07-08) | Feature `embed` (défaut), backend natif unique (ADR-033). Clé via `BASEMYAI_DB_KEY`. Référence : `docs/cli.md`. |
| Commandes V1 indispensables (`init`, `inspect`, `stats`, `recall`, `verify`, `migrate`) | ✅ | smoke test end-to-end : init→remember→recall(+`--hybrid`)→stats→inspect→verify ; isolation agent vérifiée ; mauvaise clé → refus | Couvre exactement les *indispensables V1* de la recherche stratégique. + `remember`. |
| Cycle de vie mémoire complet (`list`, `forget`, `invalidate`, `purge --yes`, `export`, `import`) | ✅ | `commands/memory.rs` | `list`/`forget`/`invalidate`/`purge` passent par `basemyai::storage::MemoryStore` directement (pas de chargement Candle pour des mutations sans embedding). **Non listé dans `TODO.md` M5** — code plus avancé que le plan. |
| Graphe (`graph add-entity`, `graph add-edge`, `graph traverse`) | ✅ | `commands/graph.rs` | Miroir CLI de `basemyai::Graph`. **Non listé dans `TODO.md` M5.** |
| `consolidate` (commande racine) | ✅ | `commands/maintenance.rs` ; `cli.rs` (`Command::Consolidate`) | Exige un LLM local (`llm detect`). |
| `forget-adaptive`, `gc` (commandes racine) | ✅ | `commands/maintenance.rs` ; `cli.rs` (`Command::ForgetAdaptive`/`Command::Gc`) ; `tests/cli.rs` | Réintroduites par ADR-037/ADR-038 (retirées en tant que `maintenance gc`/`maintenance forget-adaptive` par ADR-033, jamais rétablies sous ce sous-groupe — commandes racine, cohérent avec `consolidate`). `open_engine` (store nu) : aucun chargement Candle, testées en CI. `--dry-run` sur les deux, rapport JSON structuré. |
| `config show/set/unset`, `completions` | ✅ | `commands/config.rs`, `persisted_config.rs` | Résolution `--db`/`--agent` : flag > env (`BASEMYAI_DB_PATH`/`BASEMYAI_AGENT`) > `~/.basemyai/config.toml` > erreur explicite. `--format json` sur toutes les commandes (agent-as-tool). |
| `setup [--fetch]`, `status`, `llm detect`, `llm suggest` | ✅ | `commands/provision.rs` ; testé contre modèle provisionné + détection LLM locale | `setup` respecte le consentement explicite (ADR-010). Persistance via `provision.json`. |
| Erreurs/exit codes stables (`error.rs`/`exit.rs`), JSON `{"error":{"code","message"}}` | ✅ | `error.rs`, `exit.rs`, `output.rs` | Voir `docs/cli.md` §Exit codes & error shape. |
| Distribution binaire (cargo-dist) | 🟡 | `dist-workspace.toml` ; `.github/workflows/cli-release.yml` ; `crates/basemyai-cli/Cargo.toml` | Config posée 2026-07-10 : `dist init` ciblant **uniquement** `basemyai-cli` (bin `basemyai`) — `basemyai-mcp`/`basemyai-rest` (qui ont aussi un `[[bin]]`) opt-out via `package.metadata.dist.dist = false`. 4 targets (msvc/linux-gnu/apple x86_64+aarch64, alignés sur `bindings/basemyai-node/package.json` napi targets). `tag-namespace = "cli"` : tags/CI (`cli-v*`) et fichier workflow dédié (`cli-release.yml`) n'entrent pas en collision avec `.github/workflows/release.yml` (publish crates.io + GH Release, `v*`, non géré par `dist`). Binaire distribué compile avec `embed` (Candle) **activé par défaut** — c'est déjà le default-feature de `basemyai-cli`, et ça ne viole pas l'invariant « jamais d'auto-download » : `Candle` ne télécharge rien, seul `setup --fetch` le fait (consentement explicite, ADR-010). `dist plan` validé (dry-run) ; **`dist build`/publication réelle non exécutés — décision humaine séparée** (M5). |
| Tests CLI automatisés | ✅ (partiel) | `tests/cli.rs` (`assert_cmd`, 12 tests) | Couvre les commandes sans embedder, câblé dans le gate CI (`cargo test -p basemyai-cli`). `remember`/`recall`/`stats`/`export`/`import`/`consolidate` hors CI (nécessitent Candle provisionné). |

---

## 7. Format `.bmai`

| Feature | Statut | Preuve (vérifiée) | Notes |
|---|---|---|---|
| Métadonnées conteneur (`bmai_meta` KV) | ✅ | `basemyai/src/storage/native_store/` ; `tests/format.rs` (`native_store_writes_bmai_container_metadata`) | `format=basemyai-memory`, `format_version=1`, `storage_engine=native`, `embedding_dim=384`. |
| `BMAI_FORMAT_VERSION` constante | ✅ | `basemyai::storage::BMAI_FORMAT_VERSION` | |
| Spec format documentée | ✅ | `docs/format/bmai-v1.md` ; ADR-019/ADR-033 | Spec native (répertoire moteur, `format_version=2`, `storage_engine=native`). **Statut : expérimental** — aucune compatibilité entre revisions internes garantie avant le gel du format (`docs/format/bmai-v1.md` §Format stability), voir `PLAN-NATIVE-ENGINE.md` pour la politique de remplacement. |
| Extension `.bmai` | ✅ | répertoire moteur natif (WAL/SST/`crypto.meta`) | Conteneur natif chiffré ADR-030. **Pas de compatibilité** avec les `.bmai` V1/libSQL (export JSONL avant migration). |

---

## 8. Tests / CI

| Domaine | Statut | Preuve (vérifiée) | Notes |
|---|---|---|---|
| Isolation agent (adversarial) | ✅ | `tests/contracts.rs`, `tests/memory.rs` | Indispensable V1 couvert. |
| Validité temporelle | ✅ | `tests/contracts.rs` (`validity_*`), `temporal.rs` | Horloge implicite `now_unix`. |
| Anti-injection / isolation adversariale | ✅ | `AgentId` newtype + isolation structurelle clés KV ; `tests/p1_isolation_adversarial.rs` | Test adversarial dédié : `agent_id` hostile, requêtes FTS hostiles, ids d'un autre agent — vecteur, hybride, invalidate/forget/traverse scopés. **Câblé dans le gate CI.** Voir `SECURITY.md`. |
| Migration idempotente | ⛔ N/A | — | Plus de migrations SQL ; format natif versionné via `format.lock`. |
| Format / metadata | ✅ | `tests/format.rs` | |
| Encryption required (product-level) | ✅ | `tests/contracts.rs` | Enveloppe native ADR-030. |
| Graphe / consolidation / oubli / GC | ✅ | `tests/graph.rs`, `tests/consolidation*.rs`, `tests/maintenance_worker.rs` (19 scénarios oubli adaptatif + GC : capacité, isolation, ensembles disjoints, résilience aux interruptions, pagination — dont le contrat paginé `scan_for_forgetting` —, cohérence des index) | Consolidation, oubli adaptatif (ADR-037, borné ADR-041 §7.3) et GC temporel (ADR-038, indexé ADR-041 §7.2) tous actifs et testés. |
| Contrats `MemoryStore` | ✅ | `tests/memory_tests.rs` (19 scénarios), `tests/storage_contract.rs` | Runner déclaratif : `native` + `native_encrypted`. |
| Contrats core | ✅ | `crates/basemyai-core/tests/contracts.rs` | |
| Roundtrip bindings | ✅ | Node `__tests__/roundtrip.test.js`, Py `tests/test_roundtrip.py` | |
| CI multi-OS × features | ✅ | `.github/workflows/ci.yml` ; `cargo xtask ci` | Gate : Ubuntu + Windows (clippy + test). Jobs séparés : `embed`, `crash-consistency`. Plus de job `crypto` libSQL. |
| Workflows release / prebuild | 🟡 | `release.yml`, `node-prebuilds.yml`, `python-wheels.yml` | crates.io + PyPI publiés en `0.1.0` (2026-06-22). **Release 0.2.0 native-only à préparer** (breaking). npm toujours à vérifier. |
| Bench KNN natif (10k/100k) | ✅ | `docs/benchmarks/n3-vector-parity-2026-07-05.md`, `n5.5-memorystore-knn-bench-2026-07-07.md` ; `tests/native_memory_store_bench.rs` | N=10k mesuré (~17 ms `recall_vector` bout-en-bout). **100k+ hors scope** ; bench libSQL M6 archivé (référence historique). |

> **Items ouverts — CI & Release (2026-07-08)** :
>
> - Valider les workflows release sur un tag staging (`release.yml`, wheels, prebuilds npm).
> - Dry-run publication **0.2.0** (breaking : native-only).
> - Bench Mem0+Qdrant : harnais prêt, chiffres pas publiés.
> - (optionnel) Faire appeler `ci.yml` par `cargo xtask` — single source of truth (N0).

---

## 9. Studio / UI

| Feature | Statut | Preuve | Notes |
|---|---|---|---|
| `basemyai studio` (Web UI locale) | ⏸️ | — | Reporté **V1.5** par la recherche stratégique (read-only d'abord). |
| Recall Lab / Memory Timeline / Isolation Viewer | ⏸️ | — | V1.5. |
| Tauri desktop | ⏸️ | — | Reporté **V2** (ADR-019 / recherche stratégique). |

---

## 10. Extensions futures (post N5.6)

| Feature | Statut | Preuve | Notes |
|---|---|---|---|
| Moteur natif BaseMyAI (N0→N5.6) | ✅ | `docs/TODO-NATIVE-ENGINE.md` (toutes cases cochées) ; ADR-024→033 | **Clos 2026-07-08** (ADR-033). Chantier complet : LSM, vecteur, graphe, FTS, chiffrement, hardening M6. Historique détaillé dans `TODO-NATIVE-ENGINE.md`. |
| N7 — Observabilité + banc d'essai moteur | ✅ | `Engine::stats()` (`tests/engine_stats.rs`) ; failpoints (`src/failpoint.rs`, `tests/failpoints.rs`) ; `engine_bench` + `cargo xtask engine-*` ; `tests/corruption_smoke.rs` ; baseline `docs/benchmarks/n7-engine-baseline-2026-07-10.md` | **Clos 2026-07-10** (programme production-hardening, `PLAN-NATIVE-ENGINE.md` §4). Baseline mesurée : amplification d'écriture ×80 (memory) / ×14 (KV) de la compaction naïve, flush p95 91,8 ms à 100k, ouverture = chargement SST intégral — le dossier chiffré justifiant N8 (ADR-039). Gap connu pinné par test : SST supprimée = perte silencieuse (pas de manifest avant N9). |
| N8 — SST par blocs (ADR-039) | ✅ | `store/sst_block.rs`, `format/{sst_block,store_meta,crypto}.rs` ; baseline `docs/benchmarks/n8-block-sst-baseline-2026-07-11.md` | **Clos 2026-07-11** (N8.1→N8.11). Ouverture O(métadonnées) (~65× plus vite à 1M), AEAD par bloc anti-permutation, bloom + block cache, `store.meta` (rejet propre des anciens stores), scan préfixé par l'index (N8.11 : ×88-157 de régression → plus rapide que N7.5). |
| N9 — Verify/repair/rebuild/compact/reembed (ADR-040) | ✅ | `store/{verify,repair}.rs` ; `basemyai/src/storage/integrity.rs` ; `basemyai-cli` (`verify`/`repair`/`rebuild-indexes`/`compact`/`reembed`) ; `docs/adr/ADR-040-integrity-and-repair.md` | N9.1 (ADR) + N9.2 (verify Quick/FullPhysical, strictement read-only) + N9.4 (`Engine::compact_now()`) livrés 2026-07-11 ; N9.3 (`FullLogical`) livré 2026-07-12. N9.5 clos 2026-07-12 : plan `repair --dry-run` pur et rebuild moteur de dérivées (vecmap/allocateur, FTS, connectivité DiskANN), signal explicite de ré-embedding requis, test d'intangibilité des primaires. N9.6 clos 2026-07-12 : surface CLI branchée sur `basemyai::storage::integrity` (wrapper `spawn_blocking`, chiffrement obligatoire) — `verify [--physical\|--logical]` (audit moteur strictement read-only lancé *avant* toute ouverture normale, donc avant qu'un `open` ne recouvre une queue WAL déchirée), `repair [--dry-run]` (audite en `FullLogical`, applique `rebuild-indexes` seulement si `plan.can_apply_derived_only()`, sinon refuse avec l'exit code dédié `REPAIR_REFUSED`), `rebuild-indexes` (application inconditionnelle, sans audit préalable), `compact` (`Engine::compact_now`, rapport stats avant/après). 4 tests d'intégration CLI ajoutés. **N9 entièrement clos 2026-07-12** avec l'ajout de `reembed` : `basemyai::storage::integrity::{reembed_missing_container, reembed_ids_container, reembed_all_container}` recomputent le vecteur au `vec_id` existant du souvenir (delete idempotent + insert — jamais de nouvel id alloué, jamais touché record/vecmap/FTS) via l'`Embedder` réel injecté par le CLI. `basemyai reembed` (sans flag) relance `rebuild_indexes` pour une liste `reembedding_required` à jour puis la corrige, portée = tout le conteneur ; `basemyai reembed --agent X --ids a,b` / `--all` réembed sans condition (ex. changement de modèle), portée = un agent. Hors suite CI gatée (charge Candle, comme `remember`/`recall`) — vérifié manuellement de bout en bout sur le vrai modèle local (no-op sain, réembed ciblé/`--all`, `recall` retrouve toujours la bonne mémoire après, `--all --ids` rejeté comme erreur d'usage). |
| N10 — Maintenance scalable (ADR-041) | ✅ | `docs/adr/ADR-041-native-maintenance-indexes.md` ; `idx/memory/persistent.rs` (`forget_many`, `list_agents`), `idx/fts` (`stage_delete_many`), `idx/vector` (`delete_many_with`), `key::{temporal_index,agent_registry}` ; `basemyai` (`remember_with_importance`/`set_importance`, `MemoryStore::{scan_for_forgetting,forget_many}`, `NativeMemoryStore::list_agents`) ; tests à chaque couche (SST/engine/index/store/maintenance) | **Clos 2026-07-13** (§7.1→§7.5). §7.1 API d'importance ; §7.2 index temporel d'expiration + `Engine::scan_range` (GC en range query, plus de scan complet par agent) ; §7.3 oubli adaptatif à mémoire bornée `O(capacity)` (deux passes paginées, `Engine::scan_range_page`) ; §7.4 `forget_many` par lots atomiques bornés (items + octets, reprise idempotente entre lots, FTS agrégé + tombstones groupées) câblé dans GC et oubli adaptatif (CLI + `Memory` événementiel) ; §7.5 registre d'agents `meta/agents/` (identifiants seuls, inscrit au premier put, désinscrit à la purge, `list_agents`). Benchmark 1M archivé le 2026-07-15 avec la campagne N11 (`docs/benchmarks/n11-soak-1m-2026-07-15.md`) — plus de suivi ouvert. |
| N11.1 — Fuzz targets, couverture complète des décodeurs | ✅ | `crates/basemyai-engine/fuzz/fuzz_targets/*.rs` (24 cibles) ; `fuzz/README.md` | **Clos 2026-07-12** : les 9 décodeurs sans cible identifiés par audit direct (`grep` de chaque `pub fn decode`/`decode_*`, pas une supposition) — les trois de chiffrement au repos (`crypto_meta`/`wal_envelope`/`encrypted_sst_block`, via des wrappers `pub fn fuzz_decode_*` ajoutés dans `format/crypto.rs` pour ne pas exposer `CryptoMeta`/`Nonce` dans l'API publique) et six décodeurs `idx::{fts,memory}` (`docterms`/`postings`/`stats`/allocateur/**record primaire**/`vecmap`). 24 cibles au total, une par décodeur du crate sans exception. Exécution réelle faite le même jour sous WSL/Kali (toolchain provisionné : rustup nightly, build-essential/clang/llvm, cargo-fuzz — libFuzzer ne tourne pas nativement sous Windows, `STATUS_DLL_NOT_FOUND` au lancement) : les 24 cibles ont tourné 30s chacune, plusieurs millions à dizaines de millions d'exécutions par cible, **zéro crash sur les 24**, `exit=0` confirmé pour chacune. |
| N11.2 — Tests model-based | ✅ | `crates/basemyai-engine/tests/model_based.rs` | **Clos 2026-07-13** : `BTreeMap` comme modèle de référence, PRNG maison xorshift64* (même construction que `src/harness.rs`, pas de dépendance `rand`), séquences pondérées (put/get/delete/batch/flush/compact/reopen/crash/prefix_scan/rotate_key) rejouées clair+chiffré. Chaque propriété §8.1 épinglée par une assertion attachée à l'opération qui l'exerce : last-write-wins, suppression persistante, batch tout-ou-rien (versant succès), aucun record ressuscité après compaction, scan ordonné, réouverture identique (gracieuse et arrêt sale). 7+3 seeds, ~2s. |
| N11.3 — Tests de panne I/O | ✅ | `crates/basemyai-engine/tests/io_faults.rs` | **Clos 2026-07-13** : la majorité de §8.2 était déjà couverte (`failpoints.rs`/`crash_consistency.rs`/`corruption_smoke.rs`) — ce chantier ferme accès refusé (tmp cible en lecture-seule, `EngineError::Io` typé, retry propre après levée de l'obstruction) et fichier temporaire déjà présent (tmp périmé écrasé proprement, `create+truncate` jamais `create_new`). Corrige au passage le commentaire pré-N9 obsolète de `corruption_smoke.rs` : vérifié empiriquement que `verify_store` en `FullLogical` ne détecte pas non plus une SST vivante supprimée (aucun manifest) — gap toujours ouvert, candidat naturel N13/ADR-043. |
| §8.3 — Matrice de tests (gate/nightly/campagne) | ✅ partiel | `.github/workflows/{ci,fuzz,nightly,soak-campaign}.yml` ; `BASEMYAI_CRASH_CYCLES` (`tests/crash_consistency.rs`) | **2026-07-13** : 3 bugs CI trouvés et corrigés (gate n'exécutait ni `model_based` ni `io_faults` ; `fuzz.yml` référençait une cible supprimée en N8.5 et ne couvrait que 5/24 cibles ; `engine-soak` documenté "nightly" depuis N7.3 mais jamais câblé). `nightly.yml` (crash loops 200 cycles via `BASEMYAI_CRASH_CYCLES`, défaut gate inchangé à 20 ; bench 100k clair+chiffré archivé en artefact). `soak-campaign.yml` (hebdomadaire, Linux/Windows/macOS, `engine-soak`, `workflow_dispatch` pour un run 1M à la demande). **Non fait, documenté comme tel** : rotation de clé pendant soak, disque presque plein, comptage de handles — nécessitent une instrumentation moteur nouvelle, pas de la config CI. |
| N11 — Campagne 1M réellement exécutée et archivée | ✅ | `docs/benchmarks/n11-soak-1m-2026-07-15.md` ; `docs/benchmarks/data/n11-soak-1m/*.json` (8 rapports) + `campaign.log` ; nouveau flag `--verify` sur `engine_bench` (`crates/basemyai-engine/src/bin/engine_bench.rs`) | **Clos 2026-07-15** : le "reste de suivi hors clôture" laissé par N10/§8.3 (`soak-campaign.yml` jamais réellement déclenché à 1M) est maintenant fait pour de vrai — 4 cycles à `n=1 000 000`, clair **et** chiffré à chaque cycle (8 invocations), plus `cargo xtask test-crash-consistency` (7 variantes × 20 cycles kill réel) dans la même session. `verify_store(FullLogical)` (le mode le plus profond, ADR-040 §2 — le plan dit `--deep`, le code/CLI disent `FullLogical`/`--logical`) branché directement dans `engine_bench` via `--verify` pour auditer chaque store juste après sa fermeture, avant suppression du répertoire temporaire (les stores de `engine_bench` sont des répertoires `Engine` bruts, pas des conteneurs `.bmai` — `verify_store` s'applique directement dessus, pas besoin de la surface CLI). **14/14 audits `healthy=true`, 0 erreur, 0 warning** ; RSS peak stable 553,5-561,7 Mo sur les 8 runs (aucune tendance monotone) ; `sst_bytes`/`wal_bytes`/`tombstone_count`/`compaction_count` identiques bit-pour-bit entre cycles clairs entre eux et cycles chiffrés entre eux (workload déterministe, aucune dérive) ; `test-crash-consistency` 7/7 vert. Aucun bug trouvé. Seul critère de sortie R1 non vérifié positivement : comptage de handles fichier (pas d'instrumentation existante, documenté comme tel plutôt que déclaré clos par omission — voir le rapport). **N11 formellement clos** : les 4 chantiers (N11.1 fuzz, N11.2 model-based, N11.3 pannes I/O, campagne 1M) sont maintenant tous archivés avec preuve mesurée. |
| N12 — Passphrase KDF + zeroization + rotation complète DEK (ADR-042) | ✅ | ADR **Accepted**, committé : `docs/adr/ADR-042-passphrase-kdf-zeroization-full-rotation.md` (§5, 13/13 critères de sortie cochés et sourcés 2026-07-19). Code : `basemyai-engine/src/crypto.rs` (Argon2id via `argon2` 0.5.3 + `zeroize`, profils `Default`/`LowMemory`), `format/crypto.rs` + `format/generation_meta.rs` (générations logiques, legacy = génération 0), `Engine::{open_with_passphrase*, rotate_passphrase*, rotate_key_full, rotate_passphrase_full*}` ; produit : `EncryptionKeyMode::Passphrase`, `NativeMemoryStore::{open_with_passphrase*, rotate_passphrase*, rotate_key_full}` ; surfaces CLI/bindings py-node (passphrase exposée) ; tests `adr042_contract.rs` + `old_crypto_meta_copied_beside_a_new_generation_cannot_open_its_wal_or_sst` (ajouté 2026-07-19, ferme le dernier gap du §5) + `full_rotation_abort_boundaries_keep_the_published_generation_healthy` (crash-consistency, 7 sites × 2 générations) ; fuzz `crypto_meta_decode_structured.rs` ; baseline `docs/benchmarks/n12-argon2id-baseline-2026-07-15.md` | **Clos 2026-07-19.** `cargo xtask ci` vert (avec `adr042_contract` désormais câblé dans le gate, cf. §8.3bis) et `cargo xtask test-crash-consistency` vert (8/8, y compris le mode `--full`). |
| Sync P2P (change-capture WAL) | 📋 | `TODO-NATIVE-ENGINE.md` §N6 | V2. Primitive WAL posée. |
| Langage de requête (Couche 4) | 📋 | ADR-024 §vision | Décision produit préalable. |
| Multi-modèles d'embedding | ⏸️ | — | V2 (baseline unique en V1). |
| Key rotation native (`rotate_key`) | ✅ | ADR-030 ; `NativeMemoryStore::rotate_key` | Re-scellement O(1). Ré-encryption complète DEK = **N12/ADR-042, clos** (voir ligne N12 ci-dessus). |
| Bench KNN 100k+ (MemoryStore) | 🟡 | `native_memory_store_bench.rs` (`#[ignore]` N=10k) | Item de suivi post-N5.5. |
| Licence BUSL-1.1 unifiée | ✅ | ADR-031 | Tout le workspace. `0.1.0` crates.io/PyPI reste MIT pour les early adopters. |

---

## 11. Preuves publiques P1 (différenciation marché)

Artefacts ajoutés pour étayer publiquement le positionnement « base mémoire
agent locale, pas une base vectorielle de plus », liés depuis `README.md`
(§ P1 Public Proofs) et `SECURITY.md`.

| Artefact | Statut | Preuve | Notes |
|---|---|---|---|
| `docs/not-a-vector-db.md` | ✅ | doc de positionnement | Comparaison face Qdrant/Chroma/LanceDB/pgvector/FAISS, Mem0/LangMem, Graphiti. |
| `docs/zero-network-after-setup.md` | ✅ | doc + commande de preuve manuelle (proxy invalide) | Renvoie au test `provision_without_consent_fails_when_model_absent`. **CI dédiée pas encore ajoutée** (job `zero-network-after-setup` proposé, pas câblé). |
| Test adversarial d'isolation (`tests/p1_isolation_adversarial.rs`) | ✅ | voir §8 | Ferme le gap « pas de test d'injection dédié » de la recherche stratégique. |
| Démo remplacement temporel (`crates/basemyai/examples/temporal_replacement.rs` + `examples/node/temporal_replacement.ts` + `examples/python/temporal_replacement.py`) | ✅ | trois langages, même scénario (invalidate ancien fait → nouveau fait seul rappelé) | Pas encore dans une suite d'exemples testée en CI (risque de dérive si l'API SDK bouge). |
| Benchmark concurrentiel BaseMyAI vs Mem0+Qdrant (`benchmarks/p1-market/`, `docs/benchmarks/n6-native-vs-mem0-qdrant-2026-07-10.md`) | ✅ | harnais Python (`run.py`/`summarize.py`) ; `out/basemyai-n6.json`, `out/mem0-qdrant-n6.json`, `out/mem0-noinfer-n6.json` | Rejoué 2026-07-10 sur le moteur natif (le bench 2026-06-21 mesurait encore libSQL, retiré ADR-033). Chiffres réels : `remember` BaseMyAI (317.8 ms mean) ~10.8× plus rapide que Mem0 `infer=True` (3420.6 ms mean, coût LLM par `.add()`) — le P1 claim central tient. Honnêteté : `recall` Mem0/Qdrant (~94 ms mean) est plus rapide que `recall`/`recall_hybrid` BaseMyAI (~351–358 ms) dans ce run, et `remember` Mem0 `infer=False` (133.7 ms, stockage seul) est plus rapide que `remember` BaseMyAI — écarts documentés, pas maquillés. **Déviation de protocole disclosed** : Qdrant en mode embedded local (pas de Docker dispo dans l'environnement), donc pas directement comparable au chiffre Docker de 2026-06-21 pour le coût propre de Qdrant. |

---

## 12. Context Engine & Recall Quality Lab (committés 2026-07-19, R1 clos)

Plan de référence : `docs/PLAN-CONTEXT-ENGINE.md`. Le working tree audité le
2026-07-17 (~111 fichiers non committés, compilation cassée) a été committé
le 2026-07-19 en 6 commits (`939c922` eval, `fccdca2` N12/ADR-042, `0312fef`
context engine, `a0cf48d` bindings, `1bc291d` doc, `a3842e7` fix CI). Dans la
même session, l'audit a trouvé que **R1.6/R1.7 étaient déjà livrés côté Rust**
(la version du 2026-07-17 de ce fichier et du plan les disait « ouverts »),
que `lib.rs` n'exportait qu'un sous-ensemble, et que les bindings/CLI/MCP/REST
n'exposaient que R1.0-R1.5. Tout ceci est maintenant fermé : export racine
complet, bindings à parité, et les trois surfaces manquantes construites.
**`cargo xtask ci` vert** (confirmé après nettoyage du `target/` périmé —
voir §8.3bis) et testé de bout en bout sur chaque surface, y compris
manuellement sur CLI avec le modèle Candle réel provisionné.

| Feature | Statut | Preuve (vérifiée) | Notes |
|---|---|---|---|
| Module `basemyai::context` (`Memory::compile_context`) | ✅ | `crates/basemyai/src/context/{mod,types,token,compile,selection,temporal,render}.rs` ; `crates/basemyai/tests/context.rs` | Pipeline déterministe **sans LLM** : recall hybride borné → filtres layer/provenance/validité → normalisation + estimation tokens → dédup → sélection sous budget dur (pondérée par profil, R1.6) → sections/rendu texte/Markdown/JSON (R1.7) + citations + exclusions + trace observables. **R1.0→R1.7 tous livrés et testés.** |
| Export racine (`lib.rs`) | ✅ | `pub use context::{...}` dans `crates/basemyai/src/lib.rs` | Exporte désormais aussi `ContextProfile`, `ContextRenderFormat`, `ContextTraceLevel`, `ContextRole`, `ContextTrace`, `ContextTraceEvent`, `ContextTraceSummary`, `ContextWarning`, `DedupCluster`, `InclusionReason`, `RetrievalContribution`, `MAX_CONTEXT_TRACE_EVENTS` — le commentaire dans `context/mod.rs` qui laissait ce re-export « hors périmètre » a été retiré. |
| Support côté domaine (provenance/config) | ✅ | `memory/trust.rs` (`TrustLevel`), `config.rs` (`ConfigDefaults`), `RecallOptions`/`ConversationTurn`/`SOURCE_*` exportés par `lib.rs` | Committé (`0312fef`). |
| SDK Python/Node `compile_context`/`compileContext` | ✅ | `bindings/basemyai-py/src/{memory,types}.rs` + `__init__.pyi` ; `bindings/basemyai-node/src/{memory,types}.rs` + `index.d.ts` ; `tests/test_roundtrip.py`, `__tests__/roundtrip.test.js` | **Parité complète R1.0-R1.7** : `profile`/`render_format` en options, `role`/`inclusion_reason`/`retrieval_contributions` par item, `dedup_clusters`/`warnings`/`trace` (summary + events bornés) au niveau bundle. Testé dans les deux langages (`maturin develop` + pytest ; `napi build` + jest), 15/15 et 17/18 (1 skip pré-existant) verts. |
| CLI `basemyai context` | ✅ | `crates/basemyai-cli/src/commands/compile_context.rs`, `cli.rs` (`Command::Context`, `ContextProfileArg`/`ContextRenderFormatArg`/`ContextSourcePolicyArg`), `docs/cli.md` §Context Engine | Hors gate CI (charge Candle, même convention que `remember`/`recall`) — **vérifié manuellement de bout en bout** sur le modèle réel provisionné : texte, JSON, `--profile coding --render json`, `--explain`. |
| MCP `compile_context` | ✅ | `crates/basemyai-mcp/src/tools/compile_context.rs`, `server.rs`, `tests/server.rs` (4 tests : bundle cité, profil/format, profil inconnu rejeté, agent_id trop long rejeté) | 9 outils au total (`docs/mcp-install.md` mis à jour). |
| REST `POST /v1/compile_context` | ✅ | `crates/basemyai-rest/src/routes.rs`, `openapi.yaml`, `tests/api.rs` (4 tests : bundle cité, profil/format, validation, auth) | Schéma OpenAPI complet (`CompileContextRequest`/`CompileContextResponse`) ajouté à `openapi.yaml`. |
| Crate `basemyai-eval` (Recall Quality Lab) | ✅ | `crates/basemyai-eval/` (committé `939c922`) ; assets `eval/README.md`, `eval/datasets/recall-core.jsonl`, rapports `eval/reports/{recall-core,comparison}.{json,md}` ; doc `docs/recall-quality-lab.md` | **Volontairement HORS workspace racine** : son `Cargo.toml` déclare son propre `[workspace]` et `publish = false`. Le workspace racine reste à **6 crates** + ce standalone. Câblage workspace/xtask/CI = décision d'intégration séparée, toujours ouverte (R2). |

---

## Contradictions résolues

1. **« Phase 2 implémentée » vs « roadmap ».**
   La recherche stratégique (PRD/ADR Review) signale que « les docs internes se
   contredisent sur le statut ». **Vérité du code :** la Phase 2 (graphe,
   consolidation, RRF, oubli adaptatif, LLM provision) **est bien implémentée et
   testée** (`cognition/`, `maintenance/`, tests associés). Ce n'est pas de la
   roadmap. Ce qui reste « roadmap » est la **distribution** (publication,
   CLI, Studio), pas le moteur.

2. **CLAUDE.md : « Reste ouvert : wiring consolidation dans `MaintenanceWorker` ».**
   **Obsolète.** `ConsolidationTask` est implémenté dans `maintenance/mod.rs` et
   testé (`tests/maintenance_worker.rs`) ; `TODO.md` M0.2 le coche. CLAUDE.md
   liste aussi « bindings PyO3/NAPI ; sidecar REST » comme restants : **les trois
   existent** (`bindings/basemyai-py`, `bindings/basemyai-node`,
   `crates/basemyai-rest`). CLAUDE.md (« Statut juin 2026 ») est en retard.

3. **TODO.md M2/M3 : bindings « à créer » sous `crates/`.**
   **Faux dans les deux dimensions.** Les bindings existent déjà, sous
   `bindings/basemyai-node` et `bindings/basemyai-py` (pas `crates/`), avec API
   complète, tests roundtrip et workflows de prebuild. Le plan M0→M7 décrit un
   futur déjà partiellement réalisé. Mise à jour 2026-06-22 : **crates.io et
   PyPI sont publiés** ; le point encore ouvert est surtout **npm** (package
   `basemyai` non résolu depuis cette machine) ainsi que les wrappers
   LangChain/LlamaIndex (M3).

4. **TODO.md M4 : REST « nouveau crate à créer ».**
   **Déjà fait.** `basemyai-rest` existe avec auth, routes `/v1`, tests
   (`tests/api.rs`). Image Docker écrite (2026-07-10, `crates/basemyai-rest/Dockerfile`
   et `docker-compose.yml`), build non exécuté (Docker indisponible dans l'environnement
   qui l'a produite). Manque encore la CI push registry.

5. **MCP : absent du plan TODO, mais c'est la surface la plus aboutie.**
   La recherche stratégique recommande « MCP probablement plus stratégique que
   REST » pour 2026. Le code le reflète : `basemyai-mcp` est complet (8 outils,
   2 transports, auth, audit, sampling) alors que `TODO.md` ne mentionne que REST
   en M4. Le plan documentaire n'a jamais intégré MCP.

6. **`MemoryStore` : ADR-020 + ADR-033.** Le trait d'opérations mémoire vit dans
   `basemyai::storage::MemoryStore` ; **`NativeMemoryStore` est l'unique
   implémentation** depuis ADR-033. Tests de contrat : `memory_tests.rs`
   (19 scénarios, clair + chiffré). Zones hors trait assumées :
   `memory/porting.rs` (export/import bas niveau).

7. **« indispensables V1 » — état réel post-ADR-033.**
   Présents et testés : `.bmai` natif chiffré, `remember/recall/invalidate/forget/stats`,
   couches, `valid_from/until`, isolation `agent_id`, embedding explicite,
   MCP **et** REST, CLI, test d'isolation adversarial en CI. Pas de compatibilité
   `.bmai` V1/libSQL — export JSONL avant upgrade.

---

## Synthèse exécutive

- **Moteur natif : ✅ clos jusqu'à N11 inclus.** N0→N5.6 (ADR-033, backend unique,
  libSQL/V1 retirés), production-hardening N7→N10 (observabilité, SST par blocs,
  verify/repair/reembed, maintenance scalable), campagne N11 (fuzz 24 cibles,
  model-based, pannes I/O, soak 1M archivé 2026-07-15). **HEAD = clôture N11**
  (`d58923a`).
- **N12/ADR-042 : ✅ clos 2026-07-19** (passphrase Argon2id, zeroization,
  rotation complète DEK par générations) — 13/13 critères de sortie §5
  vérifiés et sourcés, `cargo xtask ci` + `test-crash-consistency` verts. Voir §10.
- **Context Engine : ✅ R1 clos 2026-07-19** — `compile_context` (R1.0-R1.7 :
  types, profils, rendus texte/Markdown/JSON, trace/explicabilité) livré et
  testé sur les **cinq surfaces** : Rust, CLI (`basemyai context`), MCP
  (outil `compile_context`), REST (`POST /v1/compile_context`), bindings
  Python/Node à parité complète. Reste ouvert : R2 (Recall Quality Lab, déjà
  en partie livré en standalone). Voir §12.
- **Recall Quality Lab** : `crates/basemyai-eval`, standalone hors workspace,
  committé. Voir §12.
- **Mémoire (Phase 1 + 2) : ✅** — remember/recall/hybrid/graphe/consolidation/oubli,
  toutes surfaces (MCP, REST, CLI, bindings).
- **Publication : `0.1.0` sur crates.io + PyPI** (2026-06-22, encore libSQL).
  **Prochaine étape : release `0.2.0` native-only** (breaking —
  `workspace.package.version` déjà bumpé à `0.2.0` et committé, publication pas
  faite). npm à vérifier.
- **CLI : ✅ surface complète** (dont `context`) ; tests partiels en CI ;
  **cargo-dist configuré** (2026-07-10, `dist plan` vert) — `dist build`/
  publication réelle restent une décision humaine séparée (§6).
- **Reste ouvert (priorité)** : release 0.2.0. R2.x du plan Context Engine
  (câblage workspace/xtask/CI du Recall Quality Lab). Image Docker REST
  écrite (build non vérifié, cf. §5). CUDA/NVML : détection ajoutée (feature
  `cuda-detect`) — validation sur GPU NVIDIA réel encore à faire. (NAPI live
  subscriptions, bench Mem0+Qdrant, oubli adaptatif natif ADR-037 et GC
  temporel natif ADR-038 tous faits 2026-07-10 — voir §3/§5/§6/§11.)
- **V2 reporté** : Studio, Tauri, sync P2P, multi-modèles, langage de requête.
