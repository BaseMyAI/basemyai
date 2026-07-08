# Skill: architecture — BaseMyAI

## Vue d'ensemble du workspace

```
                ┌──────────────────────────────┐
                │        basemyai-core          │
                │  (crate Rust, agnostique)     │
                │  moteur natif · vecteur · FTS │
                │  Candle · MaintenanceWorker   │
                └──────────────┬───────────────┘
                               │
                ┌──────────────▼───────────────┐
                │          basemyai             │
                │  (sémantique mémoire)         │
                │  4 couches · RAG temporel     │
                │  agent_id · valid_* · graphe  │
                └──┬────────┬──────┬───────────┘
                   │        │      │
              ┌────▼──┐  ┌──▼──┐  ┌▼────┐
              │SDK Py │  │SDK  │  │REST │
              │(PyO3) │  │Node │  │side │
              └───────┘  │NAPI │  │ car │
                         └─────┘  └─────┘
```

Des crates Rust tiers peuvent aussi consommer `basemyai-core` directement (sémantique propre, hors de ce repo). Voir [`../../ECOSYSTEM_ARCHITECTURE.md`](../../ECOSYSTEM_ARCHITECTURE.md) pour le contexte écosystème.

## Règle de dépendance (JAMAIS violer)

- **`basemyai-engine`** : moteur interne, non publié — consommé par `basemyai-core` uniquement.
- **`basemyai-core`** ne dépend de rien au-dessus : ni `basemyai`, ni produit tiers.
- **`basemyai`** importe `basemyai-core` (jamais l'inverse).
- Les SDKs PyO3/NAPI/REST/CLI wrappent `basemyai` (la sémantique), jamais le core directement.

## Invariant d'agnosticité de `basemyai-core` (ADR-001)

`basemyai-core` ne connaît **jamais** :
- `agent_id`, `valid_from`, `valid_until` — sémantique mémoire de `basemyai`
- `Symbol`, `Edge`, call graph — sémantique code d'un consommateur tiers
- Couches mémoire (episodic, semantic, procedural, short-term)
- LLM, inférence, consolidation

**Test d'agnosticité** (doit retourner zéro) :
```bash
grep -rE 'agent_id|valid_until|episodic|Symbol|Edge' crates/basemyai-core/src
```

## Principe fondateur : mécanisme au core, sens au consommateur

| Brique core | Sens ajouté par `basemyai` |
|-------------|----------------------------|
| recherche vectorielle + filtres | isolation `agent_id`, validité temporelle |
| `MaintenanceWorker.register(task)` | GC expiration, oubli adaptatif, consolidation |
| graphe / FTS | entités mémoire, relations entre faits |

## Workflow ADR

**Un ADR ne se modifie JAMAIS.** Une décision qui change = **un nouvel ADR**.

Fichiers de décision :
- `docs/ADR.md` — index des décisions BaseMyAI
- `docs/PRD.md` — product requirements
- `../ECOSYSTEM_ARCHITECTURE.md` — relation avec les autres produits de l'écosystème

## Stack technique actée (ADR-032, natif-only)

| Composant | Choix |
|-----------|-------|
| Stockage | **moteur natif** `basemyai-engine` (WAL + LSM + SST) |
| Vecteur | **LM-DiskANN / Vamana** in-process |
| FTS | index inversé natif (BM25) |
| Graphe | layout KV préfixé par agent |
| Embeddings | **Candle** (`all-MiniLM-L6-v2`, 384d) |
| Chiffrement | enveloppe native ADR-030 (XChaCha20-Poly1305) |

## Provisioning hardware-aware (ADR-010)

1. **Détection matériel** au démarrage (RAM, GPU, VRAM)
2. **Liste des modèles disponibles** présentée à l'utilisateur
3. **Fetch explicite et consenti** — jamais silencieux
4. L'`Embedder` reçoit chemin + `Device` déjà résolus — il ne télécharge jamais

## Modèle en V1

- **Baseline unique** : `all-MiniLM-L6-v2` (384d, Candle, pur Rust)
- Multi-modèles = V2

## Gouvernance des releases

- **Semver strict** sur `basemyai-core` : tout changement d'API = bump de version
- Les consommateurs tiers **pin** la version de `basemyai-core` dans leur `Cargo.toml`

## Surfaces de basemyai

| Surface | Consommateur | Couche |
|---------|-------------|--------|
| SDK Python (PyO3) | builders d'agents Python | `basemyai` |
| SDK Node (NAPI-RS) | builders d'agents JS/TS | `basemyai` |
| Sidecar REST (axum) | Go, Ruby, etc. | `basemyai` |
| CLI (`basemyai`) | ops, scripting | `basemyai` |
| Crate Rust natif | programmes custom | `basemyai-core` |

## Statut juillet 2026

Workspace **100 % moteur natif** (ADR-032). Phases 1+2 implémentées ; surfaces MCP/REST/bindings/CLI en place. Voir `docs/status.md`.
