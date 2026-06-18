# Skill: architecture — Écosystème BaseMyAI / ForgeMyAI

## Vue d'ensemble de l'écosystème

```
                ┌──────────────────────────────┐
                │        basemyai-core          │
                │  (crate Rust, agnostique)     │
                │  Store libSQL · sqlite-vec    │
                │  Candle · MaintenanceWorker   │
                └──────┬────────────────┬───────┘
                       │                │
       ┌───────────────▼──────┐    ┌────▼──────────────┐
       │       basemyai        │    │    ForgeMyAI       │
       │  (sémantique mémoire) │    │  (contexte code)  │
       │  4 couches · RAG      │    │  graphe · RRF     │
       │  agent_id · valid_*   │    │  MCP/LSP          │
       └──┬────────┬──────┬────┘    └───────────────────┘
          │        │      │
     ┌────▼──┐  ┌──▼──┐  ┌▼────┐
     │SDK Py │  │SDK  │  │REST │
     │(PyO3) │  │Node │  │side │
     └───────┘  │NAPI │  │ car │
                └─────┘  └─────┘
```

## Règle de dépendance (JAMAIS violer)

- **`basemyai-core`** ne dépend de rien au-dessus : ni `basemyai`, ni `forge-*`.
- **`basemyai`** importe `basemyai-core` (jamais `forge-*`).
- **`forgemyai-app`** importe `basemyai-core` — **PAS `basemyai`** (sinon hérite du RAG temporel / `agent_id` inutiles pour du code).
- Les SDKs PyO3/NAPI/REST wrappent `basemyai` (la sémantique), jamais le core directement.

## Invariant d'agnosticité de `basemyai-core` (ADR-001)

`basemyai-core` ne connaît **jamais** :
- `agent_id`, `valid_from`, `valid_until` — sémantique mémoire de `basemyai`
- `Symbol`, `Edge`, call graph, FTS — sémantique code de ForgeMyAI
- Couches mémoire (episodic, semantic, procedural, short-term)
- LLM, inférence, consolidation

**Test d'agnosticité** (doit retourner zéro) :
```bash
grep -rE 'agent_id|valid_until|episodic|Symbol|Edge|semantic|graph|entity' \
  crates/basemyai-core/src
```

## Principe fondateur : mécanisme au core, sens au consommateur

| Brique core | Sens ajouté par basemyai | Sens ajouté par ForgeMyAI |
|-------------|--------------------------|---------------------------|
| `Store.knn(q, k, filter?)` | filter = `agent_id = ? AND valid_until > now()` | filter = `file_path = ? AND lang = ?` |
| `MaintenanceWorker.register(task)` | task = GC `valid_until` expiré | task = purge vieilles versions MVCC |
| `Filter { sql, params }` | params = `[agent_id, timestamp]` | params = `[symbol_id, version]` |

## Workflow ADR

**Un ADR ne se modifie JAMAIS.** Une décision qui change = **un nouvel ADR**.

Fichiers de décision :
- `basemyai/ADR.md` — décisions BaseMyAI (001 à 013+)
- `basemyai/PRD.md` — product requirements
- `forgemyai-app/ADR (1).md` — décisions ForgeMyAI
- `forgemyai-app/ADR-013-basemyai-core.md` — décision d'adopter basemyai-core
- `ECOSYSTEM_ARCHITECTURE.md` — relation entre les deux produits

## Stack technique actée (ADR-011, ADR-013)

| Composant | Choix | Interdit |
|-----------|-------|---------|
| Base de données | **libSQL async** | rusqlite, sqlx, DB externe |
| Vecteur | **natif libSQL** (`vector_top_k`, `F32_BLOB`) | extension externe, pgvector |
| Embeddings | **Candle** (`all-MiniLM-L6-v2`, 384d) | ONNX, fastembed |
| Chiffrement | **libSQL feature `crypto`** | sqlcipher séparé, AES manuel |
| Futur | Turso DB (pur Rust) | cloud-only DBs |

## Provisioning hardware-aware (ADR-010)

1. **Détection matériel** au démarrage (RAM, GPU, VRAM)
2. **Liste des modèles disponibles** présentée à l'utilisateur
3. **Fetch explicite et consenti** — jamais silencieux
4. L'`Embedder` reçoit chemin + `Device` déjà résolus — il ne télécharge jamais

## Modèle en V1

- **Baseline unique** : `all-MiniLM-L6-v2` (384d, Candle, pur Rust)
- Garantit la compatibilité `.idx` entre basemyai et ForgeMyAI
- Multi-modèles = V2

## Gouvernance des releases

- **Semver strict** sur `basemyai-core` : tout changement d'API = bump de version
- ForgeMyAI **pin** la version de `basemyai-core` dans son `Cargo.toml`
- `basemyai-core` testé Linux + Windows dès son premier commit (il porte du code C via bindgen)

## Surfaces SDK de basemyai (4 surfaces)

| Surface | Consommateur | Couche |
|---------|-------------|--------|
| SDK Python (PyO3) | builders d'agents Python | `basemyai` |
| SDK Node (NAPI-RS) | builders d'agents JS/TS | `basemyai` |
| Sidecar REST (axum) | Go, Ruby, etc. | `basemyai` |
| Crate Rust natif | ForgeMyAI | `basemyai-core` |

## Statut juin 2026

**BaseMyAI** : workspace scaffoldé, Phases 1+2 implémentées :
- KNN cosine + oversampling ×8 (ADR-012)
- Graphe entités/relations CTE récursive
- RRF (`rrf_fuse`, k=60)
- Oubli adaptatif hyperbolique
- Consolidation épisodes → faits (`LlmInference` injecté)
- LLM provision 20 modèles, 8 backends

**Reste ouvert** : wiring `ConsolidationTask` dans `MaintenanceWorker`, publication SDKs.

**ForgeMyAI** : docs/ADRs seulement — pas encore scaffoldé.
