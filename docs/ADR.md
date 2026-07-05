# Architecture Decision Records — BaseMyAI

Un ADR documente une décision architecturale importante : pourquoi elle a été prise, quelles alternatives ont été rejetées, et quelles en sont les conséquences. Un ADR ne se modifie jamais. Si une décision change, un nouvel ADR est créé.

Chaque ADR vit dans son propre fichier sous [`docs/adr/`](adr/). Cette page n'est qu'un **index** — elle ne contient aucune décision elle-même.

---

| # | Décision | Statut |
|---|---|---|
| [ADR-001](adr/ADR-001-two-crates-split.md) | Découpage en deux crates `basemyai-core` / `basemyai` | ✅ Accepted |
| [ADR-002](adr/ADR-002-sqlite-vec.md) | sqlite-vec — vecteurs dans SQLite | 🔵 Superseded by ADR-011 |
| [ADR-003](adr/ADR-003-candle-embeddings.md) | Candle pour l'inférence in-process | ✅ Accepted |
| [ADR-004](adr/ADR-004-four-memory-layers.md) | Les 4 couches mémoire | ✅ Accepted |
| [ADR-005](adr/ADR-005-temporal-rag.md) | RAG temporel — `valid_from` / `valid_until` | ✅ Accepted |
| [ADR-006](adr/ADR-006-agent-isolation.md) | Isolation multi-agent par `agent_id` | ✅ Accepted |
| [ADR-007](adr/ADR-007-encryption-at-rest.md) | Chiffrement au repos — sqlcipher | ✅ Accepted |
| [ADR-008](adr/ADR-008-active-worker.md) | Active Worker — thread de fond | ✅ Accepted |
| [ADR-009](adr/ADR-009-three-binding-surfaces.md) | Trois surfaces de binding + wheels précompilés | ✅ Accepted |
| [ADR-010](adr/ADR-010-hardware-aware-model-provisioning.md) | Provisioning du modèle hardware-aware (setup intelligent) | ✅ Accepted |
| [ADR-011](adr/ADR-011-libsql-pivot.md) | Pivot vers libSQL (vecteur natif + chiffrement), traits async | ✅ Accepted |
| [ADR-012](adr/ADR-012-phase2-cognition.md) | Phase 2 Cognition — Graphe, RRF, Oubli adaptatif, Consolidation | ✅ Accepted |
| [ADR-013](adr/ADR-013-llm-inference-model-agnostic.md) | Inférence LLM model-agnostic + provisioning hardware-aware | ✅ Accepted |
| [ADR-014](adr/ADR-014-hybrid-search-bm25-rrf.md) | Recherche hybride : full-text BM25 (FTS5) fusionné au vecteur par RRF | ✅ Accepted |
| [ADR-015](adr/ADR-015-additional-distance-metrics.md) | Métriques de distance additionnelles : euclidienne & hamming par re-classement | ✅ Accepted |
| [ADR-016](adr/ADR-016-anythingllm-backend.md) | AnythingLLM comme backend LLM de premier rang via API workspace-chat | ✅ Accepted |
| [ADR-017](adr/ADR-017-mcp-sampling-consolidation.md) | Consolidation par sampling MCP (emprunter le LLM du client) + politique des modes LLM | ⛔ Superseded by ADR-018 |
| [ADR-018](adr/ADR-018-agent-driven-consolidation.md) | Consolidation pilotée par l'agent — politique d'inférence à niveaux | ✅ Accepted |
| [ADR-019](adr/ADR-019-agent-memory-database-format-and-engine.md) | Agent Memory Database, format `.bmai` V1 et frontière StorageEngine | ✅ Accepted |
| [ADR-020](adr/ADR-020-memory-store-trait.md) | `MemoryStore` : contrat d'opérations mémoire dans `basemyai` | ✅ Accepted |
| [ADR-021](adr/ADR-021-libsql-reader-pool.md) | Pool de connexions lecteur libSQL + writer unique sérialisé, sous WAL | ✅ Accepted |
| [ADR-022](adr/ADR-022-memory-event-broadcast.md) | `MemoryEvent` : abonnements mémoire en direct via canal tokio broadcast | ✅ Accepted |
| ADR-023 | *(numéro non attribué)* | — |
| [ADR-024](adr/ADR-024-native-engine.md) | Moteur natif BaseMyAI (stockage/vecteur/graphe/langage maison) — remplace le chemin Turso DB | ✅ Accepted |
| [ADR-025](adr/ADR-025-native-engine-storage-foundation.md) | Fondation Couche 1 du moteur natif : LSM-tree maison (clôture spike N1) | ✅ Accepted |
| [ADR-026](adr/ADR-026-native-vector-index-lm-diskann.md) | Index vectoriel natif : famille DiskANN (LM-DiskANN sur KV), pas HNSW | ✅ Accepted |
| [ADR-027](adr/ADR-027-native-memory-store.md) | `MemoryStore` sur le moteur natif : mapping, atomicité et découpage N5 | ✅ Accepted |
