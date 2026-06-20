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
| [~] | `cargo publish --dry-run` sur les deux crates | **`basemyai-core` : dry-run vert (2026-06-20), zéro bloqueur d'empaquetage.** `basemyai` reste à dry-run mais sera bloqué tant que `basemyai-core 0.1.0` n'est pas sur crates.io (dep `version = "0.1.0"`). Ordre de publication : **core d'abord, puis basemyai**. |
| [ ] | Publier `basemyai-core` sur crates.io | `cargo publish -p basemyai-core` |
| [ ] | Publier `basemyai` sur crates.io | `cargo publish -p basemyai` |
| [ ] | Workflow CI `publish.yml` déclenché sur tag `v*` | |

---

## M2 — SDK TypeScript / Node.js (NAPI-RS)

> **⚠️ Désynchronisé avec le code (voir `docs/status.md`).** Le binding existe
> déjà sous `bindings/basemyai-node` (pas `crates/`), avec classe `Memory`
> complète, tests roundtrip et workflow `node-prebuilds.yml`. Reste réellement
> ouvert : **publication npm** + wrappers d'intégration. Le tableau ci-dessous
> est l'ancien plan, conservé pour historique.

| # | Tâche | Notes |
|---|-------|-------|
| [x] | Créer le binding Node avec NAPI-RS | Fait sous `bindings/basemyai-node`. |
| [ ] | Wrapper `Memory` → classe JS `Memory` | Constructor async : `await Memory.open(path, agentId, encryptionKey, modelPath)` |
| [ ] | Méthodes : `remember`, `recallByLayer`, `recall`, `invalidate`, `forget` | Retourner des `Promise<T>` via `napi::Task`. |
| [ ] | Wrapper `Graph` → classe JS `Graph` | `addEntity`, `addEdge`, `traverse` |
| [ ] | Types TypeScript générés automatiquement | NAPI-RS génère les `.d.ts` via `#[napi]` — vérifier la qualité. |
| [ ] | Package npm `basemyai` : `package.json`, `index.js`, `index.d.ts` | |
| [ ] | Tests Jest | Au moins : remember + recall, invalidate, graph traversal. |
| [ ] | CI prebuild matrix : `linux-x64`, `win32-x64`, `darwin-x64`, `darwin-arm64` | GitHub Actions `@napi-rs/cli`, upload artefacts. |
| [ ] | Publish npm | `npm publish --access public` sur tag `v*` |
| [ ] | `examples/node/memory_basic.ts` | 15 lignes, copier-coller dans un README. |
| [ ] | `examples/node/llm_consolidation.ts` | |

---

## M3 — SDK Python (PyO3)

> **⚠️ Désynchronisé avec le code (voir `docs/status.md`).** Le binding existe
> déjà sous `bindings/basemyai-py` (pas `crates/`), avec classe `Memory` async,
> stubs `.pyi`, `py.typed`, tests et workflow `python-wheels.yml`. Reste
> réellement ouvert : **publication PyPI** + wrappers LangChain/LlamaIndex.

| # | Tâche | Notes |
|---|-------|-------|
| [x] | Créer le binding Python avec PyO3 | Fait sous `bindings/basemyai-py`. |
| [ ] | Wrapper `Memory` → classe Python `Memory` | Méthodes async via `asyncio` (pyo3-asyncio ou PyO3 0.22+ native async). |
| [ ] | Méthodes : `remember`, `recall`, `recall_by_layer`, `invalidate`, `forget` | |
| [ ] | Wrapper `Graph` | |
| [ ] | Stubs `.pyi` générés | `maturin develop --strip` + `maturin generate-ci` |
| [ ] | Tests pytest | `tests/python/test_memory.py`, `test_graph.py` |
| [ ] | CI manylinux wheel matrix | `manylinux2014_x86_64`, `musllinux_1_1`, `win_amd64`, `macosx_11_arm64` |
| [ ] | Publish PyPI | `maturin publish` sur tag `v*` |
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
| [ ] | Auth basique (Bearer token, configurable) | Sans auth le sidecar est un vecteur d'attaque locale. |
| [ ] | OpenAPI 3.1 spec source de vérité | `openapi-sidecar.yaml` reste la source canonique V1. Ne pas ajouter `utoipa` tant que la spec YAML n'est pas explicitement remplacée. |
| [ ] | Config complète | bind/port, db path, encryption key, model path, auth Bearer, agent policy. |
| [ ] | Image Docker (`FROM scratch` ou `alpine`) | Binaire statique — possible avec `musl`. |
| [ ] | CI build + push Docker Hub / GHCR | |
| [ ] | `examples/go/memory_client.go` | Démo cross-langage. |

---

## M5 — CLI `basemyai`

> **Statut au 2026-06-20 : premier jet livré et testé end-to-end.** Crate
> `crates/basemyai-cli` (binaire `basemyai`), build + `clippy -D warnings` verts.
> Smoke test validé : `init → remember → recall (+ --hybrid) → stats → inspect →
> verify`, **isolation agent vérifiée** (autre agent voit 0) et **chiffrement
> appliqué** (mauvaise clé → refus d'ouverture). Clé via `BASEMYAI_DB_KEY`.

| # | Tâche | Notes |
|---|-------|-------|
| [x] | Crate binaire `basemyai-cli` (clap) | Binaire `basemyai`. Features `embed`+`crypto` (set par défaut), miroir de `basemyai-mcp`. |
| [x] | Commandes V1 indispensables (`init`, `inspect`, `stats`, `recall`, `verify`, `migrate`) | + `remember` (pour alimenter `recall`). `recall --hybrid` (RRF). |
| [x] | `basemyai setup [--fetch]` | Détecte le matériel, provisionne le modèle (consentement explicite via `--fetch`, ADR-010), barre de progression. Persistance via `provision.json` (pas `config.json`). |
| [x] | `basemyai status` | Affiche matériel détecté + modèle provisionné + présence des fichiers. |
| [x] | `basemyai llm detect` | Serveurs LLM locaux + meilleur modèle pour la machine. |
| [x] | `basemyai llm suggest` | Modèles installables (`ollama pull <tag>`). |
| [ ] | `basemyai gc [--agent-id <id>]` | GC manuel des mémoires expirées. **Reste à faire.** |
| [ ] | Distribution : binaire unique dans le release GitHub | Via `cargo-dist` ou release action. **Reste à faire.** |
| [ ] | Tests d'intégration CLI (`assert_cmd` / `trycmd`) | Le smoke test est manuel ; à automatiser en CI. |

---

## M6 — Hardening & performance

| # | Tâche | Notes |
|---|-------|-------|
| [ ] | Stress test inférence 1h (Candle, ADR-003) | Vérifier l'absence de fuite mémoire. `valgrind` / DHAT sur Linux. |
| [ ] | Benchmark KNN : 10k, 100k, 1M vecteurs | `criterion`. Valider que l'ANN natif libSQL tient. |
| [ ] | Multi-connexions libSQL (pool) | Actuellement connexion partagée clonée. Pour une haute concurrence (sidecar), un pool de connexions est nécessaire. |
| [ ] | CUDA réel dans la détection hardware | Aujourd'hui : `CUDA_PATH` env var. V1 suffisant ; V1.1 : lier NVML directement. |
| [ ] | Key rotation (chiffrement) | `PRAGMA rekey` libSQL : changer la clé sans recréer la DB. |
| [ ] | Rotation des modèles d'embedding | Si le modèle change, tous les vecteurs doivent être re-générés. Détecter le changement via `model_id` stocké, proposer re-indexation. |

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

**Total estimé jusqu'au premier `pip install basemyai` qui marche : ~6-8 semaines.**
