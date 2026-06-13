# Changelog

Suit [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) et
[Semantic Versioning 2.0](https://semver.org/).

## [Unreleased] — prévu 0.1.0

### Ajouté

- **basemyai-mcp + basemyai** : **consolidation pilotée par l'agent** (ADR-018,
  supersède ADR-017). Le test E2E réel dans Claude Code a montré que le **sampling
  MCP n'y est pas supporté** (`-32601`) et qu'il est **déprécié** dans le protocole
  (SEP-2577). Nouvelle politique à niveaux pour l'outil `consolidate` : sampling
  *si annoncé par le client* → **LLM local** (Ollama/LM Studio/AnythingLLM via
  `choose_llm`) → sinon `status:"extraction_required"` : **l'agent appelant extrait
  avec son propre LLM** puis persiste via le nouvel outil **`consolidate_apply`**.
  Nouveau prompt **`consolidate_memory`** qui pilote ce flux de bout en bout. Côté
  `basemyai`, `consolidate()` est scindé en `consolidation_prompt` / `parse_extraction`
  / `apply_extraction` (signature de `consolidate` inchangée) ; types `Extraction` /
  `ExtractedEntity` / `ExtractedRelation` publics. **Annotations d'outils** MCP
  (`read_only_hint`/`destructive_hint`/`idempotent_hint`/`open_world_hint`) sur les
  8 outils. 2 tests E2E (sampling annoncé ; `consolidate_apply` déterministe).
- **basemyai-mcp** : **binaire `basemyai-mcp`** (`src/main.rs`) — point d'entrée de
  production. Setup hardware-aware (embedder Candle) → provider libSQL chiffré →
  serveur MCP sur **stdio** (défaut, plug-and-play agent local) ou **HTTP** local
  (`BASEMYAI_MCP_TRANSPORT=http`). Logs sur **stderr** (stdout = canal MCP en stdio).
  Variables : `BASEMYAI_DB_KEY` (requis), `BASEMYAI_FETCH=1` (consent fetch modèle
  au 1ᵉʳ run). Doc d'install : `docs/mcp-install.md` (Claude Code / Desktop / Cursor /
  HTTP). `claude mcp add basemyai -- basemyai-mcp` et l'agent a une mémoire persistante.
- **basemyai-mcp** : outil MCP **`consolidate`** + **`SamplingBackend`** — la
  consolidation épisodes→faits peut désormais **emprunter le LLM du client MCP**
  via `sampling/createMessage` (ADR-017). Quand BaseMyAI tourne dans Claude Code /
  Claude Desktop / Cursor / ChatGPT, l'agent appelle `consolidate` et c'est *son*
  modèle qui fait l'extraction : **aucun LLM externe ni clé requis**, et ça reste
  privacy-first (la donnée passe par le client que l'utilisateur a déjà choisi).
  `SamplingBackend` vit dans `basemyai-mcp` (le crate mémoire reste agnostique de
  MCP), implémente `LlmInference`. Test E2E in-memory : serveur + client MCP reliés
  par duplex, chemin complet `remember → consolidate (sampling) → graphe →
  recall_graph` validé. Nouvelle variante `McpError::Sampling`.
- **basemyai** : `AnythingLlmBackend` — nouveau backend LLM via l'API workspace-chat
  d'AnythingLLM (`POST /api/v1/workspace/{slug}/chat`, authentifié Bearer). Implémente
  `LlmInference`, retourne `textResponse`. `choose_llm()` l'utilise en **fallback niveau 2**
  si `BASEMYAI_ANYTHINGLLM_KEY` + `BASEMYAI_ANYTHINGLLM_WORKSPACE` sont définis et
  qu'aucun backend direct (Ollama, LM Studio…) n'est disponible. `anythingllm_from_env()`
  lit la config depuis les variables d'env. (ADR-016). `LlmProvision.backend` est
  désormais `Box<dyn LlmInference>` (supporte les deux backends sans branching).
- **basemyai** : Test E2E `consolidation_e2e` (#[ignore], déclenché manuellement) :
  3 épisodes → `consolidate()` via AnythingLLM → **6 entités + 5 relations** extraites
  par `qwen3-vl:4b-instruct` (validé 13 juin 2026). Première exécution réelle du
  pipeline consolidation→graphe contre un LLM physique.
- **basemyai** : `Memory::export_jsonl` / `import_jsonl` — export/import JSONL
  versionné de toute la mémoire d'un agent (souvenirs + graphe + validité +
  importance). Les embeddings sont exclus et **re-calculés à l'import** (une
  passe `embed_batch` par lots), ce qui fait de l'export le chemin de
  migration de modèle d'embedding. Import atomique (une transaction) et
  idempotent (`INSERT OR IGNORE`, bilan `ImportReport`). Nouvelle variante
  d'erreur `MemoryError::Porting`.
- **basemyai** : `OpenAiCompatBackend` — nouveau nom du backend d'inférence
  (`OllamaBackend` reste un alias, nom historique d'ADR-013) : il parle à tout
  serveur OpenAI-compat (Ollama, LM Studio, Jan, vLLM…). Ajout d'un **timeout
  d'inférence** (300 s par défaut, `with_timeout`) et d'un timeout de
  connexion (5 s) — un serveur local figé ne bloque plus la consolidation.
- **basemyai-core** : `Store::begin_write()` → `WriteTxn` — transaction
  d'écriture sérialisée (`BEGIN IMMEDIATE` + verrou writer interne, rollback
  automatique au drop). Les écritures multi-tables des consommateurs deviennent
  atomiques sur la connexion partagée.
- **basemyai** : `Memory::remember_batch` / `remember_batch_with` — ingestion
  par lot (une passe `embed_batch`, une transaction : tout le lot ou rien).
- **basemyai** : `remember`, `forget` et `purge_agent` sont désormais
  **atomiques** (table `memory` + miroir FTS `memory_fts` mis à jour dans la
  même transaction — plus d'état incohérent possible entre vecteur et BM25).
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
| --- | --- | --- |
| `CoreError` | `basemyai-core` | Erreurs du socle extensibles |
| `MemoryError` | `basemyai` | Erreurs mémoire extensibles |
| `Device` | `basemyai-core` | Nouveaux devices de calcul possibles |
| `MemoryLayer` | `basemyai` | Couche supplémentaire possible en V1.1 |
| `Value` | `basemyai-core` | Types SQL libSQL extensibles |
| `BackendKind` | `basemyai` | Nouveaux serveurs LLM locaux possibles |

Types stables (champs publics, pas de `#[non_exhaustive]`) :
`Record`, `AgentStats`, `Reached`, `ConsolidationReport`, `Fused`, `Ranking`,
`Neighbor`, `Filter`, `Migration`, `Validity`, `KnownModel`, `LlmOption`.
