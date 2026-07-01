# TODO — BaseMyAI : chemin vers un produit livrable

> **Statut au 12 juin 2026.** Phase 1 (socle libSQL + vecteur + embeddings) et
> Phase 2 (graphe + RRF + oubli adaptatif + consolidation + LLM provision)
> sont implémentées et testées. Ce document recense tout ce qui reste pour
> publier `basemyai` comme un produit réel : `pip install basemyai` qui marche,
> un SDK TypeScript, un SDK Rust publiable sur crates.io, une CLI, et la CI
> cross-platform.
>
> Ordre de livraison : **M0 → M1 (Rust) → M2 (TypeScript) → M3 (Python) → M4+ (sidecar, CLI, hardening)**.

---

## M0 — Fondations manquantes (avant tout SDK)

Les surfaces de binding n'ont aucune valeur si le cœur a des trous. Ces items
doivent être clos avant d'attaquer M1.

### M0.0 — Restructuration architecture ✅ (12 juin 2026)

Reorganisation des modules par domaine sémantique (au lieu de par artefact).

- [x] Organiser `basemyai-core/src/` : `storage/` (store + vector), `embed/` (Embedder + Candle)
- [x] Organiser `basemyai/src/` : `memory/`, `cognition/`, `provision/`, `maintenance/` + 3 utilitaires (retrieval, temporal, error)
- [x] Mettre à jour tous les `lib.rs` avec les nouveaux chemins d'import
- [x] Vérifier compilation : `cargo check --workspace` ✅ 6.66s, zéro avertissement
- [x] Documenter l'architecture (ARCHITECTURE.md) : flux d'exécution, interfaces, invariants critiques
- [x] Mettre à jour CLAUDE.md : layout + organisation par domaine sémantique

### M0.1 — Méthodes `Memory` incomplètes ✅ (12 juin 2026)

| # | Méthode | Priorité | Notes |
|---|---------|----------|-------|
| [x] | `Memory::invalidate(id)` | P0 | `valid_until = now()` — fait ne peut plus être rappelé mais reste en base. |
| [x] | `Memory::recall_by_layer(query, layer, k)` | P0 | Filtre SQL `layer = ?` + met à jour `last_access`. |
| [x] | `Memory::recall` → met à jour `last_access` | P0 | UPDATE post-KNN sur chaque souvenir retourné. |
| [x] | `Memory::forget(id)` | P1 | Suppression physique (RGPD). |
| [x] | `Memory::stats() -> AgentStats` | P1 | GROUP BY layer, souvenirs valides uniquement. |
| [x] | `Memory::search_graph(query, k)` | P2 | KNN + EXISTS sur `entity.label` via `instr`. |

### M0.2 — Wiring `MaintenanceWorker` ✅ (12 juin 2026)

| # | Tâche | Notes |
|---|-------|-------|
| [x] | Câbler `AdaptiveForgetting` dans `MaintenanceWorker` | `PARTITION BY agent_id` dans le SQL — pas besoin d'`agent_id` injecté. S'enregistre directement dans `MaintenanceWorker`. |
| [x] | Câbler `consolidate()` en tâche de fond | `ConsolidationTask { Arc<Memory>, Arc<dyn LlmInference> }` implémente `MaintenanceTask`, ignore `_store`. |
| [x] | `MaintenanceWorker::start()` — décision d'archi | Gardé `Arc<Store>`. `ConsolidationTask` est auto-suffisant (porte son propre store via `Arc<Memory>`). |

### M0.3 — Setup réel du modèle ✅ (12 juin 2026)

| # | Tâche | Notes |
| --- | ----- | ----- |
| [x] | Fetch HTTP du modèle `all-MiniLM-L6-v2` depuis HuggingFace | `provision(true)` → `fetch_model_files` → `download_and_verify` par fichier. `reqwest` + `sha2`. |
| [x] | Vérification SHA-256 après download | Hard-check si hash ancré dans `EXPECTED_SHA256` ; sinon companion `.sha256` (confiance HTTPS 1ᵉʳ DL, vérif dès le 2ᵉ). |
| [x] | Anchrage des SHA-256 officiels | `EXPECTED_SHA256` avec les 3 hashes vérifiés (config/tokenizer/model.safetensors, révision main 12 juin 2026). |
| [x] | Persistance de la config `{ model_id, dim, device }` | `PersistedProvision` → `~/<data_dir>/basemyai/provision.json`. Rechargée au démarrage via `load_persisted_provision()`. |
| [x] | Détection VRAM GPU réelle | NVIDIA via `nvidia-smi --query-gpu=memory.total` (subprocess, cross-plateforme). macOS via `system_profiler SPDisplaysDataType -json`. Zéro dep supplémentaire. |
| [x] | Barre de progression lors du fetch | `provision_with_progress(consent, cb)` — streaming par chunks `response.chunk()`, callback `cb(bytes_reçus, total_opt)` par fichier. |

### M0.4 — Chiffrement obligatoire dans `Memory` ✅ (12 juin 2026)

| # | Tâche | Notes |
|---|-------|-------|
| [x] | `Memory::open` doit échouer si `Store` est ouvert sans clé | `store.path().is_some() && !store.is_encrypted()` → `EncryptionRequired`. Stores `:memory:` exemptés (éphémères). `Store::is_encrypted()` existait déjà dans le core. |
| [x] | Test `open_without_key_fails` | `open_without_key_fails_for_file_store` + `open_in_memory_store_bypasses_encryption_requirement` dans `contracts.rs`. |

### M0.5 — CI GitHub Actions ✅ (12 juin 2026)

| # | Tâche | Notes |
|---|-------|-------|
| [x] | Workflow `ci.yml` : `cargo test` + `cargo clippy -- -D warnings` | `.github/workflows/ci.yml` — push + PR. Tests par crate (évite OOM Windows du build workspace). |
| [x] | Matrice OS : Ubuntu, Windows, macOS | 3 jobs × 3 OS : `ci`, `embed`, `crypto`. |
| [x] | Feature flags dans la matrice : `default`, `embed`, `crypto` | Job `crypto` installe CMake (apt/brew) + workaround `cp` Git sur PATH Windows (bug libsql-ffi). |
| [x] | Cache des artefacts Rust (`.cargo` + `target`) | `Swatinem/rust-cache@v2` avec `key:` distinct par feature. |
| [x] | Badge CI dans le README | Lien vers `https://github.com/basemyai/basemyai/actions/workflows/ci.yml`. |

---

## M1 — Rust SDK (crates.io)

La surface la plus simple : le crate `basemyai` est déjà en Rust. Il s'agit
de le rendre **publiable et utilisable** comme dépendance.

| # | Tâche | Notes |
|---|-------|-------|
| [x] | Compléter `Cargo.toml` des deux crates : `keywords`, `categories`, `documentation` | Requis par crates.io. Fait le 12 juin 2026. |
| [x] | Fixer la version semver à `0.1.0` et définir la stabilité API | `version = "0.1.0"` dans le workspace. `#[non_exhaustive]` sur `Device`, `MemoryLayer`, `Value`, `BackendKind`. Documenté dans `CHANGELOG.md`. |
| [x] | `cargo doc --no-deps --all-features` sans warning | `///` ajoutés sur `EncryptionKey::new`, `MaintenanceWorker::new`, champs `Filter`/`Neighbor`/`Value`. |
| [x] | `examples/rust/memory_basic.rs` | `crates/basemyai/examples/memory_basic.rs` — FakeEmbedder + in-memory store. Compile sans feature flag. |
| [x] | `examples/rust/llm_consolidation.rs` | `crates/basemyai/examples/llm_consolidation.rs` — FakeLlm + consolidation. |
| [~] | `cargo publish --dry-run` sur les deux crates | Historique : `basemyai-core` dry-run vert le 2026-06-20. Les crates sont désormais **publiées** ; garder le dry-run comme garde-fou avant la prochaine release. |
| [x] | Publier `basemyai-core` sur crates.io | Confirmé le 2026-06-22 via `cargo search basemyai --limit 10`. |
| [x] | Publier `basemyai` sur crates.io | Confirmé le 2026-06-22 via `cargo search basemyai --limit 10`. |
| [x] | Workflow CI `publish.yml` déclenché sur tag `v*` | Le tag `v0.1.0` existe ; la publication crates.io a effectivement eu lieu. |

---

## M2 — SDK TypeScript / Node.js (NAPI-RS)

> **⚠️ Désynchronisé avec le code (voir `docs/status.md`).** Le binding existe
> déjà sous `bindings/basemyai-node` (pas `crates/`), avec classe `Memory`
> complète, tests roundtrip et workflow `node-prebuilds.yml`. Reste réellement
> ouvert : **vérification finale de la publication npm** + wrappers d'intégration. Le tableau ci-dessous
> est l'ancien plan, conservé pour historique.

| # | Tâche | Notes |
|---|-------|-------|
| [x] | Créer le binding Node avec NAPI-RS | Fait sous `bindings/basemyai-node`. |
| [x] | Wrapper `Memory` → classe JS `Memory` | Fait dans `bindings/basemyai-node/src/memory.rs` ; ouverture async exposée côté JS. |
| [x] | Méthodes : `remember`, `recallByLayer`, `recall`, `invalidate`, `forget` | API complète exposée ; voir aussi `recallHybrid`, `stats`, graphe. |
| [x] | Wrapper `Graph` → classe JS `Graph` | Surface graphe exposée sur `Memory` (`addGraphEntity`, `addGraphEdge`, `recallGraph`). |
| [x] | Types TypeScript générés automatiquement | `index.d.ts` présent ; qualité encore à surveiller en publication. |
| [x] | Package npm `basemyai` : `package.json`, `index.js`, `index.d.ts` | Présents sous `bindings/basemyai-node/`. |
| [x] | Tests Jest | `bindings/basemyai-node/__tests__/roundtrip.test.js`. |
| [ ] | CI prebuild matrix : `linux-x64`, `win32-x64`, `darwin-x64`, `darwin-arm64` | GitHub Actions `@napi-rs/cli`, upload artefacts. |
| [~] | Publish npm | Le workflow et le README ciblent `basemyai`, mais `npm view basemyai` renvoie `404` depuis cette machine au 2026-06-22 ; vérifier le nom/scope réellement publié avant de clore. |
| [x] | `examples/node/memory_basic.ts` | Présent sous `examples/node/`. |
| [ ] | `examples/node/llm_consolidation.ts` | |

---

## M3 — SDK Python (PyO3)

> **⚠️ Désynchronisé avec le code (voir `docs/status.md`).** Le binding existe
> déjà sous `bindings/basemyai-py` (pas `crates/`), avec classe `Memory` async,
> stubs `.pyi`, `py.typed`, tests et workflow `python-wheels.yml`. Reste
> réellement ouvert : wrappers LangChain/LlamaIndex. **Publication PyPI confirmée** le 2026-06-22.

| # | Tâche | Notes |
|---|-------|-------|
| [x] | Créer le binding Python avec PyO3 | Fait sous `bindings/basemyai-py`. |
| [x] | Wrapper `Memory` → classe Python `Memory` | Classe async présente dans `bindings/basemyai-py/src/memory.rs`. |
| [x] | Méthodes : `remember`, `recall`, `recall_by_layer`, `invalidate`, `forget` | API complète exposée ; voir aussi `recall_hybrid`, `stats`, graphe. |
| [x] | Wrapper `Graph` | Surface graphe exposée par la classe `Memory`. |
| [x] | Stubs `.pyi` générés | `python/basemyai/__init__.pyi` + `py.typed` présents. |
| [x] | Tests pytest | `bindings/basemyai-py/tests/test_roundtrip.py`. |
| [x] | CI manylinux wheel matrix | Workflow `python-wheels.yml` présent ; publication PyPI observée avec wheels manylinux/macOS/Windows. |
| [x] | Publish PyPI | Confirmé le 2026-06-22 via `python -m pip index versions basemyai` et la page PyPI. |
| [ ] | Compat LangChain : `BasemyaiMemory(BaseMemory)` wrapper | Rend `basemyai` utilisable dans n'importe quelle chaîne LangChain en 2 lignes. |
| [ ] | Compat LlamaIndex : `BasemyaiMemoryBuffer` | |
| [ ] | `examples/python/memory_basic.py` | |
| [ ] | `examples/python/langchain_agent.py` | |

---

## M4 — Sidecar REST (axum)

Pour les langages sans binding natif (Go, Ruby, etc.) et pour les tests d'intégration
multi-langages.

> **⚠️ Désynchronisé avec le code (voir `docs/status.md`).** Le sidecar REST
> existe déjà sous `crates/basemyai-rest` (axum, routes `/v1`, auth Bearer
> constant-time, tests `tests/api.rs`). De plus, un sidecar **MCP**
> (`crates/basemyai-mcp`, 8 outils, stdio+HTTP, sampling) — non prévu dans ce
> plan — est la surface la plus aboutie. Reste réellement ouvert pour REST :
> **image Docker + push registry CI**.

| # | Tâche | Notes |
|---|-------|-------|
| [x] | Crate binaire REST (axum) | Fait sous `crates/basemyai-rest`. |
| [x] | Auth basique (Bearer token, configurable) | Fait : Bearer constant-time, mode dev explicite, tests API. |
| [x] | OpenAPI 3.1 spec source de vérité | `openapi-sidecar.yaml` présent ; source canonique V1. |
| [x] | Config complète | `bind`/`port`, db path, encryption key, model path, auth Bearer, agent policy. |
| [ ] | Image Docker (`FROM scratch` ou `alpine`) | Binaire statique — possible avec `musl`. |
| [ ] | CI build + push Docker Hub / GHCR | |
| [ ] | `examples/go/memory_client.go` | Démo cross-langage. |

---

## M5 — CLI `basemyai`

> **Statut au 2026-06-20 : surface complète livrée et testée end-to-end.**
> Crate `crates/basemyai-cli` (binaire `basemyai`), build + `clippy -D
> warnings` verts. Smoke test validé : `init → remember → recall (+ --hybrid)
> → stats → inspect → verify`, **isolation agent vérifiée** (autre agent voit
> 0) et **chiffrement appliqué** (mauvaise clé → refus d'ouverture). Clé via
> `BASEMYAI_DB_KEY`. Référence complète des commandes : `docs/cli.md`.

| # | Tâche | Notes |
|---|-------|-------|
| [x] | Crate binaire `basemyai-cli` (clap) | Binaire `basemyai`. Features `embed`+`crypto` (set par défaut), miroir de `basemyai-mcp`. |
| [x] | Commandes V1 indispensables (`init`, `inspect`, `stats`, `recall`, `verify`, `migrate`) | + `remember` (pour alimenter `recall`). `recall --hybrid` (RRF), `--layer`, `--graph`. |
| [x] | Cycle de vie mémoire (`list`, `forget`, `invalidate`, `purge --yes`, `export`, `import`) | `commands/memory.rs`. `list`/`forget`/`invalidate`/`purge` n'embedent pas (passent par `basemyai::storage::MemoryStore`). **Non listé ici avant 2026-06-20 — code en avance sur ce plan.** |
| [x] | Graphe (`graph add-entity`, `graph add-edge`, `graph traverse`) | `commands/graph.rs`. **Non listé ici avant 2026-06-20.** |
| [x] | Maintenance (`maintenance gc`, `maintenance forget-adaptive`) et `consolidate` | `commands/maintenance.rs`. `gc` était la ligne « reste à faire » ci-dessous — **fait**. `maintenance gc` n'est pas scopé `--agent-id` (tourne sur tout le conteneur) ; `consolidate` exige un LLM local détecté. |
| [x] | `basemyai config show/set/unset`, `basemyai completions <shell>` | `commands/config.rs` + `persisted_config.rs`. Résolution `--db`/`--agent` : flag > env > `~/.basemyai/config.toml` > erreur explicite. |
| [x] | Erreurs CLI centralisées, exit codes stables, JSON `{"error":{"code","message"}}` | `error.rs`/`exit.rs`/`output.rs`. Voir `docs/cli.md` §Exit codes & error shape. |
| [x] | Tests d'intégration CLI (`assert_cmd`) | `tests/cli.rs`, 12 tests, commandes sans embedder. Pas encore en CI (aucun job `crypto`+`embed` combiné). |
| [x] | `--format json` global (sortie machine-readable) | Toutes les commandes. Pensé pour qu'un agent IA appelle le CLI comme un outil. |
| [x] | `basemyai setup [--fetch]` | Détecte le matériel, provisionne le modèle (consentement explicite via `--fetch`, ADR-010), barre de progression. Persistance via `provision.json` (pas `config.json`). |
| [x] | `basemyai status` | Affiche matériel détecté + modèle provisionné + présence des fichiers. |
| [x] | `basemyai llm detect` | Serveurs LLM locaux + meilleur modèle pour la machine. |
| [x] | `basemyai llm suggest` | Modèles installables (`ollama pull <tag>`). |
| [ ] | `basemyai gc [--agent-id <id>]` | `maintenance gc` existe mais n'est pas scopé par agent. **Reste à faire si le besoin se confirme.** |
| [ ] | Distribution : binaire unique dans le release GitHub | Via `cargo-dist` ou release action. **Reste à faire.** |
| [ ] | Tests d'intégration CLI (`assert_cmd` / `trycmd`) | Le smoke test est manuel ; à automatiser en CI. |

---

## M6 — Hardening & performance

| # | Tâche | Notes |
|---|-------|-------|
| [x] | Harnais stress inférence Candle (ADR-003) | `crates/basemyai-core/tests/candle_stress.rs`, ignoré par défaut, exige `BASEMYAI_MODEL_DIR`. Voir `docs/benchmarks/m6-knn-and-candle-stress.md`. |
| [ ] | Exécution stress 1h + mémoire | Produire un run `BASEMYAI_CANDLE_STRESS_SECS=3600` sur machine cible, idéalement avec DHAT/Valgrind ou monitoring OS, puis archiver le brut avant claim public. |
| [x] | Harnais benchmark KNN | `criterion` : `cargo bench -p basemyai-core --bench knn_scalability`. Tailles via `BASEMYAI_KNN_BENCH_SIZES=10000,100000,1000000`. |
| [ ] | Résultats KNN 10k/100k/1M | Générer et archiver les sorties Criterion full-scale sur machine cible avant claim public. |
| [x] | Multi-connexions libSQL (pool) | ✅ ADR-021 : pool de lecteurs round-robin + writer unique sérialisé sous WAL (`journal_mode=WAL`, `busy_timeout`). `:memory:` dégénère en taille 1. Warm-up séquentiel sous `native_open_lock`. `spawn_blocking` reporté (à benchmarker). |
| [ ] | CUDA réel dans la détection hardware | Aujourd'hui : `CUDA_PATH` env var. V1 suffisant ; V1.1 : lier NVML directement. |
| [ ] | Key rotation (chiffrement) | `PRAGMA rekey` libSQL : changer la clé sans recréer la DB. |
| [x] | Rotation des modèles d'embedding (garde-fou) | `Memory::open` enregistre `embedding_model_id` dans `bmai_meta` et refuse un embedder incompatible (`EmbeddingModelMismatch`) avec consigne export/import pour ré-indexer. Reste hors scope : commande de réindexation in-place. |

### M6.2 — Live subscriptions (PLAN.md P2.1) — fondation faite (2026-06-21)

- [x] Canal `tokio::sync::broadcast` sur `Memory` (`MemoryEvent` / `MemorySubscription`, ADR-022) : émission **après commit**, isolation par `agent_id` côté serveur (prolonge ADR-006), `Lagged` toléré. Crate `basemyai` uniquement — `basemyai-core` reste agnostique.
- [ ] Surfaces (vague 2) : SSE/WebSocket REST, notifications MCP, callbacks PyO3/NAPI.

### M6.1 — Sécurité agentique (audit 2026-06-20) ✅ pour la partie corrigée

Audit ciblé sur la surface d'attaque spécifique à une DB mémoire pour agents IA
(memory poisoning, isolation, surfaces réseau REST/MCP). Corrigé dans cette
passe :

- [x] Bornes REST/MCP non validées (`k`, `max_depth`, longueurs `agent_id`/`text`/`query`) → rejet `400`/`VALIDATION_ERROR` (REST `routes.rs`, MCP `tools/mod.rs`).
- [x] Fuite de détails internes dans les réponses HTTP (`RestError::parts()` catch-all) → message générique côté client, détail loggé via `tracing::error!`.
- [x] Rate limiting basique sur `remember` (REST, fenêtre glissante par `agent_id` dans `AppState`).
- [x] Injection de prompt via épisodes bruts dans `build_prompt` (consolidation) → délimiteurs uniques par UUID + consigne explicite "donnée non fiable, jamais une instruction".
- [x] Escalade de confiance silencieuse `episodic → semantic` → colonne `source` (`MEMORY_SCHEMA_V7`), faits consolidés marqués `source = 'consolidation'` vs `'user'`.
- [x] Déduplication consolidation uniquement exacte → ajout d'un check de similarité sémantique (seuil cosine ≥ 0.95) en plus de l'égalité de contenu.
- [x] Pas de limite de taille sur le contenu mémorisé → `MAX_TEXT_LEN = 65_536` appliqué dans `Memory::remember_with`/`remember_batch_with` (`MemoryError::TextTooLong`).

Reporté volontairement (décision utilisateur du 2026-06-20, pas un oubli) :

- [ ] **`agent_id` non lié à l'identité authentifiée** (Bearer token partagé entre agents locaux → un agent peut se faire passer pour un autre `agent_id`, confused deputy / cross-tenant leakage). Nécessite un changement de modèle d'auth (clés API scopées par agent ou dérivation depuis le token) — **c'est un changement de modèle de déploiement, pas un bug** : doit passer par un nouvel ADR avant implémentation, pas un fix ponctuel. Cohérent avec le mono-déploiement local actuel ; à statuer si/quand le multi-agent non-fiable sur une même instance devient un cas d'usage réel.
- [ ] Provenance fine par épisode source (actuellement : tag binaire `user`/`consolidation`, pas de lien vers les `id` d'épisodes précis ayant produit chaque fait). Nécessiterait de faire porter un `episode_ids` par fait extrait — schéma + format d'extraction à revoir.
- [ ] Rate limiting côté MCP (fait côté REST uniquement pour l'instant) et sur les autres routes que `remember` si l'usage le justifie.
- [ ] Pré-filtre heuristique anti-injection sur le contenu entrant (mots-clés type "ignore les instructions précédentes") — délibérément non implémenté : best-effort à fort risque de faux positifs sans dessin clair (LLM juge ?), à concevoir séparément plutôt que bricolé.

---

## M7 — Documentation & release

| # | Tâche | Notes |
|---|-------|-------|
| [ ] | `docs/` : guide Getting Started (5 min) | Python en premier (marché principal). |
| [ ] | Intégration LangChain (tutoriel) | Avec screenshot/gif. |
| [ ] | Intégration LlamaIndex | |
| [ ] | Intégration n8n / flowise (webhook + REST sidecar) | |
| [ ] | API reference générée (Rust : docs.rs, Python : sphinx, TS : typedoc) | |
| [ ] | CHANGELOG.md + politique semver | |
| [ ] | Landing page basemyai.com | Simple GitHub Pages pour commencer. |
| [ ] | Post de lancement HN / Reddit r/rust / r/LocalLLaMA | Après M1 minimum. |

---

## V2 — Roadmap post-launch (ne pas implémenter avant M3)

> Ces items sont documentés dans `VISION.md` et les ADR comme objectifs V2.
> Ne pas les toucher tant que le produit V1 n'est pas livré.

| # | Item | Référence |
|---|------|-----------|
| [ ] | Multi-modèles d'embedding (sélection hardware-aware au runtime) | ADR-010, ADR-003 |
| [ ] | Migration Turso DB (pur Rust, zéro C) | ADR-011 |
| [ ] | Sync multi-device (Turso managed) | VISION §7 |
| [ ] | Mémoire partagée inter-agents (opt-in) | ADR-006 |
| [ ] | Explicabilité / provenance des recalls | VISION §5.4 |
| [ ] | ForgeMyAI : scaffold du moteur de contexte de code | ECOSYSTEM_ARCHITECTURE.md |

---

## Récapitulatif prioritaire

```
M0  Fondations manquantes          ~1-2 sem    ← BLOQUER tout le reste
M1  Rust SDK + crates.io           ~1 sem
M2  TypeScript / NAPI-RS           ~2-3 sem
M3  Python / PyO3 + LangChain      ~2-3 sem
M4  Sidecar REST                   ~1-2 sem
M5  CLI                            ~1 sem
M6  Hardening                      continu
M7  Docs + launch                  ~1 sem
```

**Le premier `pip install basemyai` marche déjà** (PyPI `0.1.0` confirmé le 2026-06-22). Le reste du travail porte désormais surtout sur npm, la distribution binaire CLI, les intégrations framework et le hardening.
