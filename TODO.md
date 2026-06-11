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

### M0.1 — Méthodes `Memory` incomplètes

| # | Méthode | Priorité | Notes |
|---|---------|----------|-------|
| [ ] | `Memory::invalidate(id)` | P0 | Met `valid_until = now()` sur une ligne. Sans ça, les faits ne peuvent jamais être corrigés. |
| [ ] | `Memory::recall_by_layer(query, layer, k)` | P0 | Recall filtré sur une seule couche (episodic only, semantic only…). |
| [ ] | `Memory::recall` → met à jour `last_access` | P0 | Actuellement `last_access` n'est jamais mis à jour. L'oubli adaptatif est aveugle sans ça. |
| [ ] | `Memory::forget(id)` | P1 | Suppression physique d'une ligne (GDPR, right to erasure). |
| [ ] | `Memory::stats() -> AgentStats` | P1 | Nombre de souvenirs par couche, utilisation vectorielle. Utile pour le debug. |
| [ ] | `Memory::search_graph(query, k)` | P2 | Recall vectoriel limité aux entités du graphe. |

### M0.2 — Wiring `MaintenanceWorker`

| # | Tâche | Notes |
|---|-------|-------|
| [ ] | Câbler `AdaptiveForgetting` dans `MaintenanceWorker` | `MaintenanceTask::run` reçoit `&Store`, mais `AdaptiveForgetting` a besoin du `agent_id`. Solution : soit `run` reçoit un contexte enrichi, soit `AdaptiveForgetting` est initialisée avec l'`agent_id`. |
| [ ] | Câbler `consolidate()` en tâche de fond | Idem : la tâche a besoin de `Arc<Memory>` + `Arc<dyn LlmInference>`. Créer un `ConsolidationTask` wrapper. |
| [ ] | `MaintenanceWorker::start()` nécessite `Arc<Store>` — refactorer en `Arc<Memory>` | Ou garder `Store` + passer les providers en paramètre. Décision d'archi à trancher. |

### M0.3 — Setup réel du modèle

`setup.rs::provision(consent_to_fetch=true)` retourne une `Err` avec
_"non encore implémenté"_. À implémenter :

| # | Tâche | Notes |
|---|-------|-------|
| [ ] | Fetch HTTP du modèle `all-MiniLM-L6-v2` depuis HuggingFace | `reqwest` déjà en dépendance. URL : `https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/main/{fichier}`. Fichiers : `config.json`, `tokenizer.json`, `model.safetensors`. |
| [ ] | Vérification checksum SHA-256 après download | Valeurs SHA officielles à ancrer dans le code. Jamais de fichier non vérifié. |
| [ ] | Persistance de la config `{ model_id, dim, device }` | Dans `~/.basemyai/config.json`. Rechargée au démarrage sans re-détecter. |
| [ ] | Détection VRAM GPU réelle | `sysinfo` ne l'expose pas. Essayer NVML (Linux/Windows, feature `cuda`), `system_profiler` (macOS). `gpu_vram_mb` est `None` dans le code actuel — le budget LLM est sous-estimé sur GPU. |
| [ ] | Barre de progression lors du fetch | UX : les fichiers font ~90 Mo total, le silence est suspect. |

### M0.4 — Chiffrement obligatoire dans `Memory`

| # | Tâche | Notes |
|---|-------|-------|
| [ ] | `Memory::open` doit échouer si `Store` est ouvert sans clé | Actuellement aucune vérification. ADR-007 : le chiffrement est **obligatoire** dans `basemyai`. Ajouter un `Store::is_encrypted() -> bool` dans le core ou passer une `EncryptionKey` obligatoire à `Memory::open`. |
| [ ] | Test `open_without_key_fails` | Compléter le test crypto existant. |

### M0.5 — CI GitHub Actions

| # | Tâche | Notes |
|---|-------|-------|
| [ ] | Workflow `ci.yml` : `cargo test --workspace` + `cargo clippy -- -D warnings` | Sur push + PR. |
| [ ] | Matrice OS : Ubuntu, Windows, macOS | Obligatoire avant publish (ADR-009). |
| [ ] | Feature flags dans la matrice : `default`, `embed`, `crypto` | `crypto` exige CMake en CI — à installer dans l'action. |
| [ ] | Cache des artefacts Rust (`.cargo` + `target`) | Build libSQL/Candle = ~8 min sans cache. |
| [ ] | Badge CI dans le README | |

---

## M1 — Rust SDK (crates.io)

La surface la plus simple : le crate `basemyai` est déjà en Rust. Il s'agit
de le rendre **publiable et utilisable** comme dépendance.

| # | Tâche | Notes |
|---|-------|-------|
| [ ] | Compléter `Cargo.toml` des deux crates : `keywords`, `categories`, `documentation` | Requis par crates.io. |
| [ ] | `cargo doc --no-deps --all-features` sans warning | Ajouter les `//!` et `///` manquants. En particulier `Memory`, `Graph`, `LlmInference`. |
| [ ] | `examples/rust/memory_basic.rs` | Open, remember, recall, invalidate. Doit compiler et tourner en 10 lignes. |
| [ ] | `examples/rust/llm_consolidation.rs` | Avec un `FakeLlm` ou Ollama. |
| [ ] | Fixer la version semver à `0.1.0` et définir la stabilité API | Documenter les types `#[non_exhaustive]` à risque de breaking change. |
| [ ] | Publier `basemyai-core` sur crates.io | `cargo publish -p basemyai-core` |
| [ ] | Publier `basemyai` sur crates.io | `cargo publish -p basemyai` |
| [ ] | Workflow CI `publish.yml` déclenché sur tag `v*` | |

---

## M2 — SDK TypeScript / Node.js (NAPI-RS)

| # | Tâche | Notes |
|---|-------|-------|
| [ ] | Créer `crates/basemyai-node` avec NAPI-RS | `napi-rs/cli`, `napi = { version = "2", features = ["async"] }` |
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

| # | Tâche | Notes |
|---|-------|-------|
| [ ] | Créer `crates/basemyai-python` avec PyO3 | `pyo3 = { version = "0.23", features = ["extension-module", "abi3-py39"] }` + `maturin` |
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

| # | Tâche | Notes |
|---|-------|-------|
| [ ] | Nouveau crate binaire `basemyai-server` (axum) | `POST /v1/memory`, `GET /v1/recall`, `DELETE /v1/memory/:id`, `GET /v1/stats` |
| [ ] | Auth basique (Bearer token, configurable) | Sans auth le sidecar est un vecteur d'attaque locale. |
| [ ] | OpenAPI 3 spec générée (`utoipa`) | |
| [ ] | Config YAML : port, agent_id, model_path, encryption_key_env | |
| [ ] | Image Docker (`FROM scratch` ou `alpine`) | Binaire statique — possible avec `musl`. |
| [ ] | CI build + push Docker Hub / GHCR | |
| [ ] | `examples/go/memory_client.go` | Démo cross-langage. |

---

## M5 — CLI `basemyai`

| # | Tâche | Notes |
|---|-------|-------|
| [ ] | Nouveau crate binaire `basemyai-cli` (clap) | |
| [ ] | `basemyai setup` | Détecte le matériel, fetch + vérifie le modèle, écrit `~/.basemyai/config.json`. Affiche la machine détectée et le modèle choisi. |
| [ ] | `basemyai status` | Lit `~/.basemyai/config.json`, vérifie que les fichiers modèle sont présents, affiche le résumé. |
| [ ] | `basemyai gc [--agent-id <id>]` | Déclenche manuellement le GC des mémoires expirées. |
| [ ] | `basemyai llm detect` | Affiche les serveurs LLM locaux détectés + le meilleur modèle pour la machine. |
| [ ] | `basemyai llm suggest` | Liste les modèles installables avec `ollama pull`. |
| [ ] | Distribution : binaire unique dans le release GitHub | Via `cargo-dist` ou release action. |

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
