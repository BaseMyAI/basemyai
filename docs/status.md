# BaseMyAI — Implementation Status Matrix

**Date : 2026-07-04** (dernière mise à jour : clôture N2 moteur natif)
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
| **REST sidecar** (`basemyai-rest`) | ✅ | `crates/basemyai-rest/src/routes.rs` ; `tests/api.rs` | axum, `/v1/remember,recall,recall_hybrid,recall_graph`, delete memory/agent, stats ; auth Bearer (constant-time), request-id, body limit. **Plus avancé que TODO M4 (tout non coché).** `crates/basemyai-rest/openapi.yaml` = spec source. Pas d'image Docker (M4 ouvert). |
| **Node binding** (`bindings/basemyai-node`, NAPI-RS) | 🟡 | `bindings/basemyai-node/src/memory.rs`, `index.d.ts` ; `__tests__/roundtrip.test.js` ; workflow `node-prebuilds.yml` | Classe `Memory` complète (remember, recall, recallByLayer, recallHybrid, invalidate, forget, stats, addGraphEntity/Edge, recallGraph). **Publication npm non confirmée depuis cette machine** au 2026-06-22 : `npm view basemyai` et le registre public renvoient `404` pour `basemyai`. Vérifier le nom/scope final si besoin. |
| **Python binding** (`bindings/basemyai-py`, PyO3) | ✅ | `bindings/basemyai-py/src/memory.rs`, `python/basemyai/__init__.pyi` ; `tests/test_roundtrip.py` ; workflow `python-wheels.yml` ; `python -m pip index versions basemyai` | Classe `Memory` async complète + stubs `.pyi` + `py.typed`. **Publication confirmée sur PyPI** (`basemyai 0.1.0` vu le 2026-06-22). Wrappers LangChain/LlamaIndex toujours absents. |
| **Live subscriptions** (ADR-022 vague 2 : SSE/WS REST, notifications MCP, callbacks PyO3/NAPI) | 🟡 | `basemyai-rest/src/routes.rs` (`GET /v1/watch`, SSE) ; `basemyai-mcp/src/tools/watch.rs` (notification `notifications/message`) ; `bindings/basemyai-py/src/memory.rs` (`Memory.watch` → `async for`) ; tests adversariaux d'isolation par surface | Fait 2026-07-02, par-dessus `Memory::watch`/ADR-022 (mécanisme déjà en place). REST et MCP testés avec isolation adversariale agent A/B. PyO3 vérifié via `maturin develop` + pytest réel (pas juste `cargo build`) — un vrai bug Windows trouvé et documenté (crash access-violation en annulant un future en attente sur `broadcast::Receiver::recv()` via `asyncio.wait_for`, cf. `docs/archive/TODO-2026-06.md`). **NAPI/Node non fait** : pas d'équivalent direct du protocole itérateur async Python en napi-rs, nécessiterait une conception distincte (ThreadsafeFunction/EventEmitter). |

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

> **Items ouverts (repris de TODO.md racine, 2026-07-02)** — CI & Release,
> partiellement câblés mais non validés de bout en bout :
>
> - Ajouter `basemyai-cli` à la matrice GitHub Actions (absent de `ci.yml`).
> - Ajouter un job dédié pour le test `p1_isolation_adversarial` (isolation
>   adversariale ADR-018).
> - Valider les workflows release sur un tag staging avec de vrais secrets :
>   `release.yml` (gate crates.io présent, non prouvé sur tag live),
>   `python-wheels.yml` (build/publish PyPI, non prouvé sur tag live),
>   `node-prebuilds.yml` (prebuild/publish npm, non prouvé sur tag live).
> - Dry-run de publication vers staging avant toute annonce.
> - Cleanup optionnel : arrêter le conteneur Qdrant du bench
>   (`docker compose -f benchmarks/p1-market/docker-compose.qdrant.yml down`)
>   et décharger les modèles Ollama inutilisés (`ollama rm`).

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
| Moteur natif BaseMyAI (stockage/vecteur/graphe/langage maison) | 🟡 | `docs/adr/ADR-024-native-engine.md` ; `docs/adr/ADR-025-native-engine-storage-foundation.md` ; `docs/PLAN-NATIVE-ENGINE.md` ; `docs/TODO-NATIVE-ENGINE.md` ; `docs/benchmarks/n1-storage-engine-spike-2026-07-04.md` | Pari long terme **acté** (ADR-024, 2026-07-02) : remplace « migration Turso » — on construit notre propre moteur pur Rust plutôt que d'adopter celui d'un tiers. Strangler fig : libSQL reste le défaut jusqu'à parité prouvée. Chantier 0 (DX) ✅. Spike N1 (Couche 1) ✅ clos 2026-07-04 : LSM bat B-tree CoW (débit 4,1×, lecture 2,5×, amplification ×1,05 vs ×14,3, 10/10 crash-consistency) → fondation maison famille LSM actée (ADR-025), pas de fork `redb`/`fjall`. **N2 (Couche 1 : store durable) ✅ clos 2026-07-04** : `crates/basemyai-engine` (WAL+memtable+SST+recovery, batches atomiques multi-clés `apply_batch` — `WAL_RECORD_VERSION` 2), harnais crash-consistency 20 cycles kill réel en CI (0 corruption, atomicité batch prouvée sous kill), fuzzing 4 cibles (1 vrai panic capacity-overflow trouvé/corrigé dans `format/sst.rs`), `format.lock` anti-drift gaté CI, `EngineKind::Native` + wrapper `NativeEngine` capability-only dans `basemyai-core` (feature `engine-native`), runner déclaratif multi-backend `memory-tests` (5 scénarios, vert sur Libsql). **N3 (index vectoriel natif) ✅ clos 2026-07-05** : décision LM-DiskANN/Vamana actée sans spike (ADR-026, évidence gap-analysis/M6/N1 suffisante) ; `crates/basemyai-engine/src/idx/vector/` (node/distance/graph/meta/persistent) — recall@10 = 1.0 en RAM, persistant, et après churn insert/delete (tombstones + `consolidate()` FreshDiskANN, batch-atomique) ; bench de parité M6 (`docs/benchmarks/n3-vector-parity-2026-07-05.md`) tient les 3 seuils avec grande marge : requête 7,5 ms/12,7 ms (10k/100k) vs plafond ~48-49 ms libSQL, build incrémental réel 5,7/17,3 ms/ligne vs 78-79 ms/ligne libSQL (jamais fini en incrémental à 100k) ; crash harness étendu deletes/consolidate, 0 violation. **N4 (graphe natif) ✅ clos 2026-07-05** : `idx/graph/{entity,edge,traverse,ram,persistent}.rs` — nœud/arête = un enregistrement KV (`relation`/`dst` dans la clé pour scan préfixé par nœud à chaque saut BFS), isolation agent structurelle, `GraphEntity:1`/`GraphEdge:1` dans `format.lock` ; traversée = portage 1:1 de la CTE récursive SQL, les 5 scénarios de `tests/graph.rs` verts contre RAM et persistant (BFS partagé, zéro dérive) ; pas de méta/rebuild nécessaire (aucun état de navigation global) ; crash harness mode `graph` 20 cycles réels, 0 violation. **N5.1 (`NativeMemoryStore` hors FTS/crypto) ✅ clos 2026-07-05** (découpage N5 acté par ADR-027) : `idx/memory/` moteur (`MemoryRecord:1`/`MemoryVecMap:1`/`MemoryIndexMeta:1` dans `format.lock`, allocateur `vec_id` monotone auto-guérissant), `insert_with`/`delete_with` (un `remember` natif = UN enregistrement WAL : record + vecmap + compteur + nœud vectoriel + voisins + méta, atomicité transaction-libSQL retrouvée), `NativeMemoryStore` dans `basemyai` (feature `engine-native`, parité requête par requête, oversampling ×8 ADR-012, FTS et métriques non-cosinus en erreur franche) — **le diff multi-backend du runner N2 enfin prouvé** : `backend_suite!` vert sur Libsql ET Native, matrice xtask/CI étendue en miroir, capacités `NativeEngine` mises à jour honnêtement (`vectors`/`recursive_queries` true). **N5.2 (FTS/BM25 natif) ✅ clos 2026-07-06** (ADR-028) : `idx/fts/` moteur — index inversé (`FtsPosting:1`) + index direct (`FtsDocTerms:1`, nécessaire au delete précis + longueur de document sans dépendre d'`idx::memory`) + stats BM25 par agent healables (`FtsStats:1`), tokenizer casefold+pliage d'accents par table figée (racinisation Porter différée, gap documenté) ; `PersistentFts::stage_insert`/`stage_delete` composent dans le `Batch` de l'appelant (jamais leur propre `apply_batch`) — `PersistentMemoryIndex::put`/`forget`/`purge_agent` les fusionnent dans le même `extra` batch que vecteur+mémoire, un `remember` natif reste UN enregistrement WAL étendu au troisième index ; scoring Okapi `k1=1.2`/`b=0.75` (défauts FTS5), `df` dérivé du scan des postings (pas de compteur caché). `NativeMemoryStore::keyword_ranking_ids` branché (fin de l'erreur franche) — un bug de parité (filtre de validité temporelle absent) trouvé et corrigé pendant l'implémentation, avant tout commit. Deux scénarios de parité `backend_suite!` (classement par pertinence + validité temporelle/forget) rejoués contre Libsql ET Native, zéro divergence ; `EngineCapabilities::native().full_text` → `true` ; aucune extension xtask/CI nécessaire (le nouveau module vit sous des entrées déjà couvertes) ; `cargo xtask ci` vert (18 étapes) + crash-consistency re-exécuté (4 modes, 0 violation). **N5.3 (100 % des contrats sur Native) ✅ clos 2026-07-06** : les 12 scénarios `storage_contract.rs` restants (isolation multi-agent recall/hydrate/purge/exact-fact, batch atomique+vide, filtre de couche, expiration/pas-encore-valide, stats par couche, classement vecteur+mot-clé isolé, traversée graphe scopée agent, épisodes récents) portés dans le runner déclaratif (`memory_tests/scenarios.rs`, 19 scénarios au total) — le `Step`/`Scenario` du harnais gagne un champ `agent: Option<&'static str>` par étape (`None` = agent par défaut du scénario, `Some(id)` l'outrepasse) pour rejouer des séquences multi-agent dans un seul scénario, plus 6 nouvelles variantes (`RememberBatch`, `PurgeAgent`, `ExpectVectorRankingIds`, `ExpectHydrate`, `ExpectAgentStatsByLayer`, `ExpectRecentEpisodes`, `ExpectExactFactExists`). 100 % de la surface de `storage_contract.rs` (16/16 tests) désormais rejouée verbatim contre Libsql ET Native, zéro divergence (`cargo test -p basemyai --features test-util,engine-native --test memory_tests`) ; `contracts.rs` reste hors scope (teste `Memory`/`Store` libSQL directement, aucune variation par backend). `cargo xtask check` + `cargo xtask test` verts ; seul incident rencontré : le flake connu et pré-existant `basemyai-mcp --test server` (`isolation_between_agents`, `STATUS_ACCESS_VIOLATION`), re-passé vert isolément, sans rapport avec ce travail. **N5.4 (chiffrement au repos natif) ✅ clos 2026-07-06** (ADR-030) : AEAD XChaCha20-Poly1305 pur Rust (aucune feature gate — contrairement au `crypto` libSQL/CMake), enveloppe DEK/KEK — la clé utilisateur n'encrypte jamais la donnée, elle dérive une KEK (SHA-256 salée+domaine) qui scelle une DEK aléatoire dans `crypto.meta` ; WAL scellé par enregistrement (`WalEnvelope:1`, torn-tail préservé, un batch = une enveloppe), SST scellé fichier entier (`SstEnvelope:1`) — tous les index (vecteur/graphe/mémoire/FTS) transitent par WAL+SST donc sont couverts mécaniquement. Erreurs franches typées à l'ouverture (mauvaise clé via descellement de la DEK, clé absente, clé sur store en clair — pas de chiffrement a posteriori). `Engine::rotate_key` = re-scellement O(1) commit atomique un-fichier, **instance utilisable après rotation** (mieux que `PRAGMA rekey` : pas de réouverture) ; écart assumé documenté : la DEK ne change pas (ADR-030 §4, ré-encryption complète = chantier de suivi explicite). Surfaces : `EngineCapabilities::native(encrypted)` (dernière capacité `false` du backend natif levée), `NativeEngine::open_encrypted`, `NativeMemoryStore::{open_encrypted,rotate_key}`. 3 formats de plus dans `format.lock`. Vérifié : les 19 scénarios `backend_suite!` rejoués contre un 3e backend `native_encrypted` (zéro divergence), rotation roundtrip niveau `MemoryStore` (ancienne clé rejetée, données intactes, instance vivante), crash harness étendu d'un 5e mode `encrypted_batch` — 5 modes × 20 cycles kill réels, 0 violation. `cargo xtask check`/`test`/`test-crash-consistency` verts. **N5.5 (barre hardening M6) ✅ clos 2026-07-07** : `put_memory_batch` tout-ou-rien (`PersistentVectorIndex::insert_many_with` via un `OverlayProvider` qui planifie chaque insert du groupe contre l'état que les inserts précédents produiront + `PersistentFts::stage_insert_many` une seule mise à jour BM25 agrégée + `PersistentMemoryIndex::put_many` qui vérifie tous les doublons — contre le store et intra-lot — avant d'écrire quoi que ce soit, un seul enregistrement WAL pour tout le groupe ; résorbe l'écart initial d'ADR-027 §6, `put` devient un `put_many` à un item). Mode `memory` du harnais crash-consistency (schéma déterministe put/forget exerçant le triplet record+vecteur+FTS sous kill réel, plus variante chiffrée) : 20 cycles × 2 (clair/chiffré), 0 violation. Concurrence mesurée : cache de `PersistentVectorIndex` devenu interior-mutable (`Mutex<HashMap>`, `search`/`search_scored` passent à `&self`), `NativeMemoryStore` passe d'`Arc<Mutex>` à `Arc<RwLock>` — lectures pures sous verrou de lecture concurrent, chemins hybrides (`recall_vector`/`hydrate`) en deux passes (recherche en lecture, `touch` en écriture brève séparée), écritures restent sérialisées (`Engine` mono-écrivain sync inchangée) ; mesuré ~3× plus rapide en concurrent sur 64 lectures mixtes (`native_concurrent_reads_are_correct_and_faster_than_sequential`). Bench KNN via le chemin `MemoryStore` complet (`docs/benchmarks/n5.5-memorystore-knn-bench-2026-07-07.md`) : à N=10 000 `--release`, insert ~12.96 ms/`put_memory` (cohérent avec la fourchette N3 sur l'index nu, 5.7–17.3 ms/ligne) et `recall_vector` ~17.03 ms/requête contre ~48.98 ms pour le `vector_top_k` nu libSQL — ~2.9× plus rapide malgré strictement plus de travail par requête. `cargo xtask check`/`test`/`test-crash-consistency` verts. Reste N5.6 : ADR de bascule du défaut libSQL→Native (décision humaine séparée, jamais prise en passant) ; stress long à 100k+ resté hors scope de cette passe (item de suivi si nécessaire). |
| Multi-modèles d'embedding | ⏸️ | — | V2 (baseline unique en V1, compat `.idx`). |
| Sync multi-device | ⏸️ | — | V2 (VISION §7). |
| Mémoire partagée inter-agents | ⏸️ | — | V2 (ADR-006). |
| Key rotation (`PRAGMA rekey`) | ✅ | voir §1 (`storage/store.rs`) | Fait 2026-07-02. |
| Pool de connexions libSQL | ✅ | `storage/store.rs` (pool lecteurs round-robin + writer sérialisé, ADR-021) | Fait — cette ligne était périmée : `TODO.md` M6 le coche déjà avant cette révision. `:memory:` dégénère en taille 1. |
| Licence BUSL-1.1 unifiée + politique de marque | ✅ | `docs/adr/ADR-031-unified-busl-license.md` (remplace `docs/adr/ADR-029-license-split-and-trademark-policy.md`) ; `LICENSE` ; `TRADEMARK_POLICY.md` | Clos 2026-07-06 : décision initiale (ADR-029, open-core — `basemyai-engine` seul en BUSL, reste du workspace en MIT/Apache) révisée en ADR-031 après clarification que le risque à couvrir est le fork-produit-concurrent de `basemyai-core`/`basemyai` eux-mêmes, pas seulement du moteur natif. **Toute la surface publiable** (`basemyai-core`, `basemyai`, CLI, MCP, REST, `basemyai-engine`, bindings Python/Node) passe sous **un seul** BUSL-1.1 (conversion Apache-2.0 à 4 ans, `Cargo.toml` racine + `license.workspace = true` partout, un seul `LICENSE` consolidé, `crates/basemyai-engine/LICENSE` supprimé). Additional Use Grant à test fonctionnel (pas de clause anti-concurrence subjective) : libre pour dépendance/embarquement dans un produit tiers même commercial, usage interne, recherche, et usage intra-écosystème (ForgeMyAI) ; bloqué seulement pour republication/fork sous n'importe quel nom comme substitut de BaseMyAI/ForgeMyAI, ou hébergement SaaS sans contribution. 125 fichiers source uniformément tagués `SPDX-License-Identifier: BUSL-1.1` ; `cargo check --workspace` vert après coup. Le `0.1.0` déjà publié crates.io/PyPI le 2026-06-22 reste MIT pour toujours pour qui l'a déjà récupéré (limite structurelle du droit d'auteur, pas un oubli). Marque « BaseMyAI »/« ForgeMyAI » toujours couverte séparément par `TRADEMARK_POLICY.md`. DCO (`git commit -s`) dans `CONTRIBUTING.md`, désormais sur tout le workspace. Reste ouvert : dépôt formel de la marque (USPTO/INPI) et provisionnement réel de `licensing@basemyai.com` (contact temporaire : `security@basemyai.com`) — décisions/démarches humaines séparées. |

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
