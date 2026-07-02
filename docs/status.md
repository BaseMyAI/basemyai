# BaseMyAI — Implementation Status Matrix

**Date : 2026-06-22**
**Statut : SOURCE DE VÉRITÉ.** Ce fichier réconcilie les contradictions entre les
docs internes (TODO.md, CLAUDE.md, VISION.md, ADR-019, la recherche stratégique
2026-06-18). Il a été recommandé par la recherche stratégique
(`docs/strategy/2026-06-18-agent-memory-database-research.md`, « Concrete Next
Steps Before Refactor », item 3) précisément parce que certaines docs disent
« Phase 2 implémentée » tandis que d'autres parlent de roadmap.

**Méthode :** chaque ligne est ancrée dans une vérification du code réel, pas
dans une déclaration de doc. Quand une doc et le code divergent, le **code fait
foi** et l'écart est noté.

**Légende statut :**

- ✅ **Implemented** — code présent ET testé dans le repo.
- 🟡 **Partial** — code présent mais incomplet, non testé end-to-end, ou
  dépendant d'un chemin non couvert par la CI.
- 📋 **Planned** — pas encore de code ; tâche ouverte dans `TODO.md`.
- ⏸️ **Deferred** — explicitement repoussé en V1.5 / V2 par ADR-019 ou VISION.

**Distinction critique :** « le code existe » ≠ « publié / testé cross-platform /
prêt prod ». La colonne Notes le précise systématiquement.

---

## 1. Core storage / engine (`basemyai-core`)

| Domaine / Feature | Statut | Preuve (vérifiée) | Notes |
|---|---|---|---|
| `Store` libSQL async (open, migrate, txn) | ✅ | `crates/basemyai-core/src/storage/store.rs` ; tests `tests/store.rs`, `tests/libsql_smoke.rs` | Backend ADR-011. Connexion partagée clonée (pas de pool — voir M6). |
| Recherche vectorielle native (`vector_knn`, cosine, `F32_BLOB`) | ✅ | `storage/store.rs`, `storage/vector.rs` ; `tests/store.rs` | In-DB, pas d'extension. Oversampling ×n pour métriques non-cosine. |
| `Filter` paramétré (fragment SQL + valeurs liées) | ✅ | `storage/vector.rs` (`Filter`, `Value`, `Neighbor`) | Anti-injection ADR-006. Confiné depuis ADR-020 à `basemyai::storage::LibsqlMemoryStore` — plus aucun consommateur de `basemyai` (memory/cognition) ne le manipule directement. |
| `StorageEngine` trait + `EngineCapabilities` | ✅ | `storage/engine.rs` (`StorageEngine`, `EngineCapabilities`, `EngineKind::Libsql`) ; `basemyai::storage::{MemoryStore, LibsqlMemoryStore}` (ADR-020) ; `tests/storage_contract.rs` | Contrat d'identité/capacités (core, inchangé) **+** contrat d'opérations mémoire (`put_memory`, `recall_vector`, `graph_upsert_entity`…) dans `basemyai` (ADR-020, suivi ADR-019). Tests de contrat pilotés par le trait. Reste hors périmètre : `memory/porting.rs` (export/import bas niveau) et `maintenance/{gc,forgetting}` (raison documentée dans ADR-020). |
| Chiffrement au repos (feature `crypto`) | ✅ | `storage/store.rs` (`is_encrypted`, `EncryptionKey`) ; job CI `crypto` | Optionnel au core, obligatoire dans `basemyai`. Exige CMake. |
| Key rotation (`PRAGMA rekey`) | ✅ | `storage/store.rs` (`Store::rotate_key`) ; `basemyai/src/memory/mod.rs` (`Memory::rotate_key`) ; `tests/store.rs`, `tests/key_rotation.rs` | Ajouté 2026-07-02. Rotation exige de rouvrir `Store`/`Memory` (le pool de lecteurs et `libsql::Database` figent la clé à l'ouverture — pas de rafraîchissement en place dans libsql 0.9.30). Bascule temporaire WAL→DELETE→rekey→WAL (SQLite3MultipleCiphers refuse `rekey` en WAL). |
| FTS5 / full-text (mécanisme) | ✅ | utilisé via `Store::connect()` dans `basemyai` ; schéma `memory_fts` | Le core expose la connexion ; le schéma FTS vit dans `basemyai`. |
| Migrations (`Migration`, `migrate`) | ✅ | `storage/store.rs` ; `tests/store.rs` | Versionnées, idempotentes. |
| `MaintenanceWorker` + tâches injectées | ✅ | `maintenance.rs` ; `tests/maintenance_worker.rs` (dans `basemyai`) | Mécanisme d'injection ; le sens (GC, oubli, consolidation) vit dans `basemyai`. |
| Embedder trait (object-safe, sync) | ✅ | `embed/mod.rs` (`Embedder`, `Device`) ; `tests/embed.rs` | Ne télécharge jamais (invariant ADR-010). |
| Candle BERT (`CandleEmbedder`, `all-MiniLM-L6-v2`, 384d) | ✅ | `embed/candle.rs` (feature `embed`) ; job CI `embed` ; `tests/candle_stress.rs` ; `docs/benchmarks/m6-candle-stress-results-2026-07-01.md` | Stress 1h (3300s) exécuté sur machine cible le 2026-07-02 : `ok`, mémoire stable (min 61.2 MB / max 193.1 MB / moy. 88.6 MB sur 102 échantillons, pas de tendance de croissance), pas de fuite observée. Lourd (Candle). |
| Agnosticité du core (zéro `agent_id`/`Symbol`/`Edge`) | ✅ | `tests/agnosticity.rs`, `tests/contracts.rs` | Invariant ADR-001 testé. |

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
| `recall_hybrid` (vecteur + BM25 fusionnés RRF) | ✅ | `memory/mod.rs:309` ; FTS5 `memory_fts` | ADR-014. La recherche stratégique le classait V1.5 — **déjà livré en V1**. |
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
| Graphe entités/relations (`add_entity`, `add_edge`, `traverse` CTE récursive) | ✅ | `cognition/graph.rs` ; `tests/graph.rs` | Scopé agent + profondeur, cycle-safe (UNION). Tables `entity`/`edge` (schéma V2/V6). |
| Consolidation épisodes→faits (`consolidate`) | ✅ | `cognition/consolidation.rs` ; `tests/consolidation.rs`, `tests/consolidation_e2e.rs` | Idempotente. `LlmInference` injecté. ADR-012/ADR-018. |
| Trait `LlmInference` (object-safe, injecté) | ✅ | `cognition/inference.rs` | Modèle jamais codé en dur. |
| Oubli adaptatif (`AdaptiveForgetting`, décroissance hyperbolique) | ✅ | `maintenance/forgetting.rs` ; `tests/forgetting.rs` | `H/(H+age)` (libSQL n'a pas `exp`). |
| GC mémoires expirées (`ExpiredMemoryGc`) | ✅ | `maintenance/gc.rs` | ADR-005. |
| Wiring consolidation dans `MaintenanceWorker` | ✅ | `maintenance/mod.rs` (`ConsolidationTask`) ; `tests/maintenance_worker.rs` | **CLAUDE.md dit « reste ouvert » — c'est obsolète.** Le code et TODO M0.2 le marquent fait. Voir Contradictions. |

> **Note de positionnement.** La recherche stratégique 2026-06-18 (Risks) avertit
> que « trop de LLM/consolidation en V1 peut détourner du noyau memory DB » et
> classe la consolidation/provenance avancée en V2. Le code Phase 2 **existe et
> est testé** ; c'est une décision produit, pas un manque technique, de savoir si
> on l'expose comme feature V1 phare ou comme capacité avancée.

---

## 4. Provisioning hardware-aware

| Feature | Statut | Preuve (vérifiée) | Notes |
|---|---|---|---|
| Détection hardware (RAM, cœurs, VRAM) | 🟡 | `provision/embedder.rs:335` (`nvidia-smi`), macOS `system_profiler` | NVIDIA/macOS best-effort. **CUDA réel = `CUDA_PATH` env var seulement** (M6 : lier NVML repoussé). |
| Fetch HTTP du modèle + vérif SHA-256 | ✅ | `provision/embedder.rs:225` (`reqwest`), `download_and_verify`, `EXPECTED_SHA256` (3 hashes ancrés) ; `tests/provisioning.rs` | Jamais d'auto-download silencieux (consentement explicite). |
| Persistance config (`provision.json`) | ✅ | `provision/embedder.rs` (`PersistedProvision`) | Rechargée au démarrage. |
| Détection LLM locale (`KNOWN_MODELS`, backends) | ✅ | `provision/llm.rs` ; `tests/llm_provision.rs` | `choose_llm()` hardware-aware, `OpenAiCompatBackend` (alias Ollama). ADR-013. |

---

## 5. Surfaces / SDK

| Surface | Statut | Preuve (vérifiée) | Notes |
|---|---|---|---|
| **Rust SDK** (crate `basemyai`) | ✅ | `Cargo.toml` (`version = 0.1.0`, keywords/categories) ; `examples/rust/*` ; `cargo search basemyai --limit 10` | API complète, examples présents, `cargo doc` propre. **Publication confirmée sur crates.io** le 2026-06-22 (`basemyai = "0.1.0"` et `basemyai-core = "0.1.0"`). |
| **MCP server** (`basemyai-mcp`) | ✅ | `crates/basemyai-mcp/` ; outils `remember/recall/recall_hybrid/recall_graph/invalidate/consolidate/consolidate_apply/stats` ; `tests/server.rs`, `tests/sampling.rs` | Transports stdio + HTTP, auth, audit, sampling (ADR-018). **Surface la plus aboutie** — cohérent avec « MCP prioritaire » de la recherche stratégique. Non listé comme milestone TODO (TODO ne mentionne que REST en M4). |
| **REST sidecar** (`basemyai-rest`) | ✅ | `crates/basemyai-rest/src/routes.rs` ; `tests/api.rs` | axum, `/v1/remember,recall,recall_hybrid,recall_graph`, delete memory/agent, stats ; auth Bearer (constant-time), request-id, body limit. **Plus avancé que TODO M4 (tout non coché).** `openapi-sidecar.yaml` = spec source. Pas d'image Docker (M4 ouvert). |
| **Node binding** (`bindings/basemyai-node`, NAPI-RS) | 🟡 | `bindings/basemyai-node/src/memory.rs`, `index.d.ts` ; `__tests__/roundtrip.test.js` ; workflow `node-prebuilds.yml` | Classe `Memory` complète (remember, recall, recallByLayer, recallHybrid, invalidate, forget, stats, addGraphEntity/Edge, recallGraph). **Publication npm non confirmée depuis cette machine** au 2026-06-22 : `npm view basemyai` et le registre public renvoient `404` pour `basemyai`. Vérifier le nom/scope final si besoin. |
| **Python binding** (`bindings/basemyai-py`, PyO3) | ✅ | `bindings/basemyai-py/src/memory.rs`, `python/basemyai/__init__.pyi` ; `tests/test_roundtrip.py` ; workflow `python-wheels.yml` ; `python -m pip index versions basemyai` | Classe `Memory` async complète + stubs `.pyi` + `py.typed`. **Publication confirmée sur PyPI** (`basemyai 0.1.0` vu le 2026-06-22). Wrappers LangChain/LlamaIndex toujours absents. |
| **Live subscriptions** (ADR-022 vague 2 : SSE/WS REST, notifications MCP, callbacks PyO3/NAPI) | 🟡 | `basemyai-rest/src/routes.rs` (`GET /v1/watch`, SSE) ; `basemyai-mcp/src/tools/watch.rs` (notification `notifications/message`) ; `bindings/basemyai-py/src/memory.rs` (`Memory.watch` → `async for`) ; tests adversariaux d'isolation par surface | Fait 2026-07-02, par-dessus `Memory::watch`/ADR-022 (mécanisme déjà en place). REST et MCP testés avec isolation adversariale agent A/B. PyO3 vérifié via `maturin develop` + pytest réel (pas juste `cargo build`) — un vrai bug Windows trouvé et documenté (crash access-violation en annulant un future en attente sur `broadcast::Receiver::recv()` via `asyncio.wait_for`, cf. `docs/TODO.md`). **NAPI/Node non fait** : pas d'équivalent direct du protocole itérateur async Python en napi-rs, nécessiterait une conception distincte (ThreadsafeFunction/EventEmitter). |

> **Écart TODO.** `TODO.md` décrit M2 (Node) et M3 (Python) comme « à créer »
> sous `crates/basemyai-node` / `crates/basemyai-python`. En réalité les deux
> bindings **existent déjà**, sous `bindings/`, avec méthodes, tests et workflows
> de prebuild. Le plan M0→M7 est en retard sur le code.

---

## 6. CLI (`basemyai`)

| Feature | Statut | Preuve (vérifiée) | Notes |
|---|---|---|---|
| Crate `basemyai-cli` (clap) | ✅ | `crates/basemyai-cli/` (binaire `basemyai`) dans `Cargo.toml` members ; build + `clippy --workspace --all-targets -D warnings` verts (2026-06-20) | Features `embed`+`crypto` (défaut), miroir `basemyai-mcp`. Clé via `BASEMYAI_DB_KEY`. Référence complète : `docs/cli.md`. |
| Commandes V1 indispensables (`init`, `inspect`, `stats`, `recall`, `verify`, `migrate`) | ✅ | smoke test end-to-end : init→remember→recall(+`--hybrid`)→stats→inspect→verify ; isolation agent vérifiée ; mauvaise clé → refus | Couvre exactement les *indispensables V1* de la recherche stratégique. + `remember`. |
| Cycle de vie mémoire complet (`list`, `forget`, `invalidate`, `purge --yes`, `export`, `import`) | ✅ | `commands/memory.rs` | `list`/`forget`/`invalidate`/`purge` passent par `basemyai::storage::MemoryStore` directement (pas de chargement Candle pour des mutations sans embedding). **Non listé dans `TODO.md` M5** — code plus avancé que le plan. |
| Graphe (`graph add-entity`, `graph add-edge`, `graph traverse`) | ✅ | `commands/graph.rs` | Miroir CLI de `basemyai::Graph`. **Non listé dans `TODO.md` M5.** |
| Maintenance one-shot (`maintenance gc`, `maintenance forget-adaptive`) et `consolidate` | ✅ | `commands/maintenance.rs` | `gc` était listé comme restant — **fait**. `consolidate` exige un LLM local détecté (`llm detect`). **`maintenance gc` n'est pas scopé par agent** (tourne sur tout le conteneur) — pas de `--agent-id` comme envisagé dans `TODO.md`. |
| `config show/set/unset`, `completions` | ✅ | `commands/config.rs`, `persisted_config.rs` | Résolution `--db`/`--agent` : flag > env (`BASEMYAI_DB_PATH`/`BASEMYAI_AGENT`) > `~/.basemyai/config.toml` > erreur explicite. `--format json` sur toutes les commandes (agent-as-tool). |
| `setup [--fetch]`, `status`, `llm detect`, `llm suggest` | ✅ | `commands/provision.rs` ; testé contre modèle provisionné + détection LLM locale | `setup` respecte le consentement explicite (ADR-010). Persistance via `provision.json`. |
| Erreurs/exit codes stables (`error.rs`/`exit.rs`), JSON `{"error":{"code","message"}}` | ✅ | `error.rs`, `exit.rs`, `output.rs` | Voir `docs/cli.md` §Exit codes & error shape. |
| Distribution binaire (cargo-dist) | 🟡 | — | Reste ouvert (M5). |
| Tests CLI automatisés | ✅ (partiel) | `tests/cli.rs` (`assert_cmd`, 12 tests) | Couvre les commandes sans embedder. Pas encore wiré en CI (aucun job `crypto`+`embed` combiné) ; `remember`/`recall`/`stats`/`export`/`import`/`consolidate` non couverts (nécessitent le modèle Candle). |

---

## 7. Format `.bmai`

| Feature | Statut | Preuve (vérifiée) | Notes |
|---|---|---|---|
| Table `bmai_meta` (format, version, engine, dim) | ✅ | `memory/schema.rs:15` (`BMAI_META_SCHEMA_V5`, migration v5) ; `tests/format.rs` (`schema_writes_bmai_container_metadata`) | `format=basemyai-memory`, `format_version=1`, `storage_engine=libsql`, `embedding_dim=384`. |
| `BMAI_FORMAT_VERSION` constante | ✅ | `memory/schema.rs:13` (`= 1`) | |
| Spec format documentée | ✅ | `docs/format/bmai-v1.md` ; ADR-019 | Conforme à la recherche stratégique (item 2). |
| Extension `.bmai` (vs magic header) | ✅ | conteneur libSQL chiffré + metadata | Choix assumé ADR-019 : pas de header custom en V1 (casserait l'ouverture SQLite). |

---

## 8. Tests / CI

| Domaine | Statut | Preuve (vérifiée) | Notes |
|---|---|---|---|
| Isolation agent (adversarial) | ✅ | `tests/contracts.rs`, `tests/memory.rs` | Indispensable V1 couvert. |
| Validité temporelle | ✅ | `tests/contracts.rs` (`validity_*`), `temporal.rs` | Horloge implicite `now_unix`. |
| Anti-injection SQL / isolation adversariale | ✅ | `Filter` paramétré + `AgentId` newtype ; `tests/contracts.rs` ; `tests/p1_isolation_adversarial.rs` | Le test adversarial dédié réclamé par la recherche stratégique (item 6) existe désormais : `agent_id` hostile façon `"agent-b' OR '1'='1"`, texte/requêtes FTS hostiles, ids connus d'un autre agent — vecteur, hybride BM25, invalidate/forget/traverse scopés vérifiés. Documenté publiquement dans `SECURITY.md` (commande `cargo test -p basemyai --features test-util --test p1_isolation_adversarial`). |
| Migration idempotente | ✅ | `tests/store.rs`, `tests/format.rs` | |
| Format / metadata | ✅ | `tests/format.rs` | |
| Encryption required (product-level) | ✅ | `tests/contracts.rs` | |
| Graphe / consolidation / oubli | ✅ | `tests/graph.rs`, `tests/consolidation*.rs`, `tests/forgetting.rs` | |
| Contrats core | ✅ | `crates/basemyai-core/tests/contracts.rs` | |
| Roundtrip bindings | ✅ | Node `__tests__/roundtrip.test.js`, Py `tests/test_roundtrip.py` | |
| CI multi-OS × features | ✅ | `.github/workflows/ci.yml` | CI actuelle : `gate` sur Ubuntu + Windows, `embed` sur Ubuntu, `crypto` sur Ubuntu + Windows. Tests par crate (évite OOM Windows et coût macOS). |
| Workflows release / prebuild | 🟡 | `release.yml`, `node-prebuilds.yml`, `python-wheels.yml`, `codeql.yml`, `supply-chain.yml` | Workflows présents. **Publication effective confirmée pour crates.io et PyPI** le 2026-06-22 ; **npm reste à re-vérifier** car le registre public ne résout pas `basemyai` depuis cette machine. |
| Bench KNN (10k/100k/1M), stress 1h | 🟡 | `crates/basemyai-core/benches/knn_scalability.rs`, `crates/basemyai-core/tests/candle_stress.rs`, `docs/benchmarks/m6-knn-results-2026-07-01.md`, `docs/benchmarks/m6-candle-stress-results-2026-07-01.md` | Stress 1h fait (✅, voir §1). KNN : 10k et 100k réels archivés le 2026-07-02 (latence quasi stable ~40-58ms entre les deux tailles, confirme un vrai ANN sous-linéaire côté requête) ; **1M non exécuté** — la construction de `libsql_vector_idx` est linéaire en coût absolu (~78-79 ms/ligne aux deux échelles mesurées), soit ~22h extrapolées pour 1M, jugé non réalisable en session. Caractéristique backend documentée, pas juste un aléa de bench. |

---

## 9. Studio / UI

| Feature | Statut | Preuve | Notes |
|---|---|---|---|
| `basemyai studio` (Web UI locale) | ⏸️ | — | Reporté **V1.5** par la recherche stratégique (read-only d'abord). |
| Recall Lab / Memory Timeline / Isolation Viewer | ⏸️ | — | V1.5. |
| Tauri desktop | ⏸️ | — | Reporté **V2** (ADR-019 / recherche stratégique). |

---

## 10. Backend natif / extensions futures

| Feature | Statut | Preuve | Notes |
|---|---|---|---|
| Backend natif `.bmai` append-only | ⏸️ | — | **V2 uniquement**, et seulement si libSQL bloque une exigence réelle (recherche stratégique). Préparé par `StorageEngine`/`EngineCapabilities` + doc, pas implémenté. |
| Migration Turso DB (pur Rust, zéro C) | ⏸️ | — | V2 (ADR-011, chemin futur). |
| Multi-modèles d'embedding | ⏸️ | — | V2 (baseline unique en V1, compat `.idx`). |
| Sync multi-device | ⏸️ | — | V2 (VISION §7). |
| Mémoire partagée inter-agents | ⏸️ | — | V2 (ADR-006). |
| Key rotation (`PRAGMA rekey`) | ✅ | voir §1 (`storage/store.rs`) | Fait 2026-07-02. |
| Pool de connexions libSQL | ✅ | `storage/store.rs` (pool lecteurs round-robin + writer sérialisé, ADR-021) | Fait — cette ligne était périmée : `TODO.md` M6 le coche déjà avant cette révision. `:memory:` dégénère en taille 1. |

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
| Benchmark concurrentiel BaseMyAI vs Mem0+Qdrant (`benchmarks/p1-market/`, `docs/benchmarks/local-memory-vs-mem0-qdrant.md`) | 🟡 | harnais Python (`run.py`/`summarize.py`/`docker-compose.qdrant.yml`) | **Harnais ajouté, aucun chiffre publié.** Distinct du bench KNN scalabilité (10k/100k/1M, M6 §8) — celui-ci compare au marché, pas la scalabilité interne. Critères de publication documentés (hardware, versions, cold/warm) mais résultats `out/*.json` pas encore générés/commités. |

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
   (`tests/api.rs`). Manque uniquement l'image Docker et la CI push registry.

5. **MCP : absent du plan TODO, mais c'est la surface la plus aboutie.**
   La recherche stratégique recommande « MCP probablement plus stratégique que
   REST » pour 2026. Le code le reflète : `basemyai-mcp` est complet (8 outils,
   2 transports, auth, audit, sampling) alors que `TODO.md` ne mentionne que REST
   en M4. Le plan documentaire n'a jamais intégré MCP.

6. **`StorageEngine` : suivi ADR-019 fait (ADR-020, 2026-06-20).** Le trait
   d'opérations mémoire (`put_memory`/`recall_vector`/`graph_upsert_entity`/…)
   décrit par la recherche existe désormais : `basemyai::storage::MemoryStore`
   + `LibsqlMemoryStore`, avec tests de contrat
   (`crates/basemyai/tests/storage_contract.rs`). `Filter`/`Value` ne fuient
   plus dans `memory/mod.rs` ni `cognition/{graph,consolidation}.rs` — confinés
   à `LibsqlMemoryStore`. Restent volontairement hors périmètre (raison
   documentée ADR-020) : `memory/porting.rs` (export/import bas niveau) et
   `maintenance/{gc,forgetting}` (signature `MaintenanceTask::run` du core
   agnostique). Donc ✅ pour le gap documenté, avec deux zones résiduelles
   connues et assumées.

7. **« indispensables V1 » de la recherche stratégique — état réel.**
   Présents et testés : `.bmai`, libSQL chiffré, `remember/recall/invalidate/forget/stats`,
   couches, `valid_from/until`, isolation `agent_id`, embedding explicite,
   MCP **et** REST. **Mis à jour 2026-06-20 :** le **CLI** et le **test
   d'injection SQL adversarial dédié** — les deux derniers manquants listés
   ici — sont désormais livrés (CLI §6, test §8/§11). Reste un écart
   « indispensable V1 » documenté vs code réel : aucun, sur ce point précis.

---

## Synthèse exécutive

- **Moteur mémoire (core + Phase 1 + Phase 2) : ✅ implémenté et testé.** Plus
  avancé que ce que CLAUDE.md et TODO.md laissent croire.
- **Surfaces d'intégration : largement en place** (MCP ✅, REST ✅, bindings Node
  🟡, Python ✅). **Publication confirmée pour crates.io et PyPI** ; **npm reste
  à re-vérifier** pour `basemyai` depuis cette machine.
- **CLI (M5) : surface complète livrée** (2026-06-20) — `basemyai-cli` couvre
  les indispensables V1 (`init/inspect/stats/recall/verify/migrate`) **et**
  le cycle de vie complet (`list/forget/invalidate/purge/export/import`), le
  graphe (`graph add-entity/add-edge/traverse`), la maintenance
  (`maintenance gc/forget-adaptive`, `consolidate`), `config`, `completions`.
  Référence : `docs/cli.md`. Restent : distribution binaire (cargo-dist),
  tests CLI automatisés en CI (`TODO.md` M5 sous-documente cette surface —
  à mettre à jour).
- **Publication : crates.io et PyPI confirmés** le 2026-06-22 (`basemyai`,
  `basemyai-core`, `basemyai 0.1.0` sur PyPI). **Le point résiduel est npm** :
  le README et les workflows visent `basemyai`, mais le registre public renvoie
  encore `404` depuis cette machine.
- **`StorageEngine` : ✅ fait (ADR-020, 2026-06-20)** — `basemyai::storage::MemoryStore`
  cache désormais `Filter`/SQL derrière un trait d'opérations mémoire, avec
  tests de contrat. Zones résiduelles assumées : `memory/porting.rs`,
  `maintenance/{gc,forgetting}`.
- **Preuves publiques P1 : ✅ ajoutées** (§11) — positionnement « pas une
  vector DB », zéro réseau après setup, test d'isolation adversarial, démos
  de remplacement temporel (Rust/Node/Python). Benchmark concurrentiel vs
  Mem0+Qdrant : harnais prêt, **chiffres pas encore publiés**.
- **Studio, Tauri, backend natif, Turso, sync, multi-modèles : ⏸️ correctement
  reportés** (V1.5 / V2), pas de dette cachée.
- **Hardening (M6) : 🟡 en cours, avancée majeure le 2026-07-02.** Pool lecteur
  libSQL (ADR-021) ✅, key rotation (`PRAGMA rekey`) ✅, stress Candle 1h ✅
  (stable, pas de fuite), bench KNN 10k/100k ✅ (avec un vrai finding : coût de
  build de l'index natif ~78-79 ms/ligne, quasi-linéaire — 1M jugé irréaliste
  en session et documenté comme tel plutôt qu'exécuté). Live subscriptions
  vague 2 (ADR-022) : REST SSE ✅, MCP notifications ✅, PyO3 ✅, NAPI reporté.
  Restent : CUDA/NVML réel, résultats KNN 1M (nécessite une machine dédiée sur
  plusieurs heures), NAPI live subscriptions.
