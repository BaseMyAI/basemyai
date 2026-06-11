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
```

Le **vecteur est natif libSQL** (compile sans CMake). Seule la feature `crypto` (chiffrement au repos) exige CMake. `embed` tire Candle (lourd).

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
- Tout doit passer le gate clippy ci-dessus avant commit.

## Layout

`crates/basemyai-core/src/` : `store` · `embed` · `maintenance` · `error` · `lib`.

`crates/basemyai/src/` :

- **Mémoire** : `memory` · `temporal` · `isolation` · `schema` · `error`
- **Phase 2 Cognition** : `graph` · `retrieval` · `forgetting` · `consolidation` · `inference`
- **Provisioning** : `setup` (embeddings) · `llm_provision` (LLM hardware-aware)

`crates/basemyai/tests/` : `graph` · `retrieval` · `forgetting` · `consolidation` · `llm_provision` · `provisioning`.

## Statut (juin 2026)

Phase 1 (socle) ✅ et Phase 2 (Cognition) ✅ implémentées :

- **KNN réel** : distance cosine, oversampling ×8 quand filtre présent (ADR-012).
- **Graphe** : entités/relations, CTE récursive `UNION` (cycle-safe), scopée `agent_id` + profondeur (ADR-012).
- **RRF** : `rrf_fuse` multi-signal, k=60, déterministe (ADR-012).
- **Oubli adaptatif** : `AdaptiveForgetting`, décroissance hyperbolique `H/(H+age)` (pas exponentielle — libSQL n'a pas `pow`/`exp`) (ADR-012).
- **Consolidation** épisodes→faits : `consolidate(memory, llm)`, idempotente, `LlmInference` injecté (ADR-012).
- **LLM provision** : `KNOWN_MODELS` 20 modèles juin 2026, 8 backends détectés, `choose_llm()` hardware-aware (ADR-013).

Reste ouvert : wiring consolidation dans `MaintenanceWorker` (nécessite `Arc<Memory>` + provider LLM dans la tâche) ; bindings PyO3/NAPI ; sidecar REST.
