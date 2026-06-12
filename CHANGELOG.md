# Changelog

Suit [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) et
[Semantic Versioning 2.0](https://semver.org/).

## [Unreleased] — prévu 0.1.0

### Ajouté
- **basemyai-core** : store libSQL async (`:memory:` + fichier), recherche
  vectorielle *native* (`vector_top_k`, `F32_BLOB`, métrique cosine), embeddings
  in-process (Candle/`all-MiniLM-L6-v2`, feature `embed`), chiffrement au repos
  (feature `crypto`), boucle de maintenance par injection de tâches.
- **basemyai** : 4 couches mémoire (`ShortTerm`, `Episodic`, `Procedural`,
  `Semantic`), RAG temporel (`valid_from`/`valid_until`), isolation SQL par
  `AgentId`, chiffrement obligatoire sur fichier (ADR-007), oubli adaptatif
  (décroissance hyperbolique), GC des expirations.
- Graphe entités/relations (`Graph::add_entity`, `add_edge`, `traverse`)
  avec CTE récursive cycle-safe, scellé par `AgentId`.
- Pipeline de consolidation `consolidate()` : épisodes → faits sémantiques +
  graphe, via `LlmInference` injecté.
- Provisioning hardware-aware : `detect_hardware()`, `provision()`,
  `provision_with_progress()`, vérification SHA-256 des fichiers modèle.
- Détection LLM locale : 8 backends (Ollama, LM Studio, Jan, KoboldCPP, vLLM,
  LocalAI, GPT4All, AnythingLLM), 20 modèles connus (juin 2026), `choose_llm()`.
- RRF (`rrf_fuse`) : fusion multi-signal Reciprocal Rank Fusion, k = 60,
  déterministe.

### Stabilité API (0.1.0)

Les types suivants sont `#[non_exhaustive]` — de nouveaux variants pourront être
ajoutés en minor sans breaking change :

| Type | Crate | Raison |
|------|-------|--------|
| `CoreError` | `basemyai-core` | Erreurs du socle extensibles |
| `MemoryError` | `basemyai` | Erreurs mémoire extensibles |
| `Device` | `basemyai-core` | Nouveaux devices de calcul possibles |
| `MemoryLayer` | `basemyai` | Couche supplémentaire possible en V1.1 |
| `Value` | `basemyai-core` | Types SQL libSQL extensibles |
| `BackendKind` | `basemyai` | Nouveaux serveurs LLM locaux possibles |

Types stables (champs publics, pas de `#[non_exhaustive]`) :
`Record`, `AgentStats`, `Reached`, `ConsolidationReport`, `Fused`, `Ranking`,
`Neighbor`, `Filter`, `Migration`, `Validity`, `KnownModel`, `LlmOption`.
