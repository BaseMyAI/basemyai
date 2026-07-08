# Architecture — BaseMyAI

## Vue d'ensemble

BaseMyAI est organisée en **deux crates Rust** communiquant via imports Rust natifs :

1. **`basemyai-core`** : socle **business-agnostic**
   - Moteur de stockage natif (`basemyai-engine`) + KNN natif
   - Embeddings in-process (optionnel Candle)
   - Worker de tâches de maintenance *injectées*
   - **Aucune connaissance** du temps (`valid_until`), des agents (`agent_id`), ou de métier

2. **`basemyai`** : couche **sémantique mémoire**
   - 4 couches mémoire (short-term, episodic, procedural, semantic)
   - Isolation multi-agent + chiffrement obligatoire
   - RAG temporel, graphe entités/relations, oubli adaptatif
   - Consolidation épisodes → faits + graphe
   - Provisioning embeddings/LLM hardware-aware

---

## Principes d'architecture

### 1. Agnosticité (ADR-001)

Le core **ne connaît jamais** :
- `agent_id`, `valid_from`/`valid_until`
- Couches mémoire (episodic, semantic, etc.)
- Graphe (entity, edge, Symbol, Edge)
- LLM, inférence, consolidation

**Moyen** : injection de dépendances via traits (`Embedder`, `MaintenanceTask`, `LlmInference`).

**Test d'agnosticité** :
```bash
grep -rE 'agent_id|valid_until|episodic|semantic|graph|entity|edge|Symbol' crates/basemyai-core/src
# DOIT retourner zéro
```

### 2. Mécanisme au core, sens au consommateur

- Le core expose **primitives** : `StorageEngine`, index natifs (vecteur/graphe/FTS), capacités, `MaintenanceWorker`
- Le consommateur applique le **sens** : filtre temporel, tâches métier injectées

**Exemple** : recherche KNN `(query, k, filtre?)`
- Core : "applique le filtre fourni après le top-k natif"
- basemyai : "le filtre est (`valid_from <= now AND (valid_until IS NULL OR valid_until > now)`)"

### 3. Pas de téléchargement silencieux (ADR-010)

- L'`Embedder` **ne télécharge jamais**, ne détecte jamais le matériel
- `setup::provision(consent)` fait le fetch explicite si `consent = true`
- Modèles détectés au démarrage, listé à l'utilisateur (même pour LLM via `choose_llm()`)

### 4. Chiffrement obligatoire dans basemyai (ADR-007 / ADR-030)

- `basemyai-core` : chiffrement au repos natif optionnel (AEAD, aucune feature Cargo)
- `basemyai` : chiffrement **requis** sur disque — l'ouverture échoue sans clé

### 5. Backend = moteur natif BaseMyAI (ADR-024→032)

- **Pas de DB externe**, pas de services externes obligatoires
- Moteur `basemyai-engine` **pur Rust** : WAL + memtable + SST, index vectoriel natif LM-DiskANN/Vamana (`F32`), graphe et FTS/BM25 natifs
- Contrat `MemoryStore` async ; `Embedder` reste **sync** (CPU-bound)
- Backend unique, pas de fallback libSQL (ADR-032)

---

## Structure des modules

### basemyai-core

#### `storage/` — Moteur natif + capacités

| Module | Rôle |
|--------|------|
| `engine.rs` | `EngineKind`, `NativeEngine`, `EngineCapabilities` — moteur natif capability-first |
| `mod.rs` | re-exports du socle de stockage |

**Points clés** :
- Moteur `basemyai-engine` : WAL + memtable + SST, batches atomiques `apply_batch`, recovery crash-consistent
- KNN natif LM-DiskANN/Vamana : sur-échantillonne ×8 si filtre présent (ADR-012)
- Distance cosinus réelle (recalculée, pas placeholder)
- Chiffrement au repos natif AEAD (ADR-030, aucune feature Cargo ni CMake)

#### `embed/` — Embeddings in-process

| Module | Rôle |
|--------|------|
| `mod.rs` | `Device` enum (Cpu/Cuda/Metal), trait `Embedder` (object-safe) |
| `candle.rs` | `CandleEmbedder` BERT (feature "embed"), charge depuis dossier local |

**Points clés** :
- Aucun téléchargement, aucune détection matériel
- Reçoit `Device` + chemin modèle résolus par le setup du consommateur
- BERT (all-MiniLM-L6-v2, 384d) : mean-pooling masqué + L2 norm

#### `maintenance.rs` — Worker de tâches injectées

| Type | Rôle |
|------|------|
| `MaintenanceTask` | Trait : `async fn run(&self)` — tâche injectée par le consommateur |
| `MaintenanceWorker` | Planifie + exécute des tâches en tâche de fond (tokio::spawn par tâche) |

**Points clés** :
- Le core fait tourner la boucle (sleep → run)
- Le consommateur injecte le sens (GC temporel, consolidation)
- Idéal pour la maintenance off-path (RRF, graphe traverse, oubli adaptatif)

#### `error.rs`, `lib.rs`

- `CoreError` : `Storage`, `Vector`, `Embed`, `Encryption`, `ModelNotProvisioned`
- Pas de concepts métier dans les erreurs

---

### basemyai

#### `memory/` — Domaine mémoire

| Module | Rôle |
|--------|------|
| `layer.rs` | `MemoryLayer` (4 couches), `Record`, `AgentStats` |
| `isolation.rs` | `AgentId` newtype — scoping structurel dans le layout de clé du moteur |
| `porting.rs` | Export/import JSONL (backup, migration inter-modèles) |
| `mod.rs` | Façade `Memory` : remember, recall, invalidate, forget, stats, search_graph |

**Points clés** :
- L'ouverture scelle par `AgentId` + `Embedder`, contrat embedding porté en KV (`meta/bmai/`)
- `recall(query, k)` filtre par agent + temps via le contrat `MemoryStore`
- `search_graph` : KNN + traversée graphe native
- Métadonnées de validité : `valid_from`, `valid_until` (optionnel), `importance`, `last_access`

#### `cognition/` — Pipeline Phase 2

| Module | Rôle |
|--------|------|
| `inference.rs` | Trait `LlmInference` (object-safe) — abstraction fournisseur |
| `consolidation.rs` | `consolidate(memory, llm)` : épisodes → faits + graphe (idempotent) |
| `graph.rs` | `Graph` : add_entity, add_edge, traverse (BFS borné natif) |

**Points clés** :
- Consolidation : texte brut du LLM → JSON structuré → peuplement graphe + promotion semantic
- Graphe : index natif `entity`/`edge` (un nœud/une arête = un enregistrement KV), traversée BFS cycle-safe, scopée `agent_id` + `valid_until`
- Traverse : portage 1:1 de la CTE récursive historique en BFS sur scans préfixés par nœud source

#### `provision/` — Provisioning hardware-aware

| Module | Rôle |
|--------|------|
| `embedder.rs` | `detect_hardware()`, `provision(consent)` avec fetch + SHA256 |
| `llm.rs` | `KNOWN_MODELS` (20 modèles juin 2026), `detect_llm_options()`, `choose_llm()` |

**Points clés** :
- Embedder : detection NVIDIA (nvidia-smi) / macOS (system_profiler), choix device CUDA > Metal > CPU
- Embedder : fetch 3 fichiers HF avec vérification SHA-256, persist config JSON
- LLM : sonde 8 backends (Ollama, LM Studio, Jan, vLLM, KoboldCPP, LocalAI, AnythingLLM), budget RAM 60% CPU ou 90% VRAM

#### `maintenance/` — Tâches de fond

| Module | Rôle |
|--------|------|
| `mod.rs` | `ConsolidationTask` — Arc<Memory> + Arc<dyn LlmInference> impl MaintenanceTask |

**Points clés** :
- Consolidation : auto-suffisante (utilise sa propre `Arc<Memory>`), injectée dans le worker agnostique du core
- GC temporel et oubli adaptatif reposaient sur du fenêtrage SQL (`ROW_NUMBER() OVER`) libSQL-spécifique : **retirés** avec libSQL (ADR-032) plutôt que portés en passant — un portage natif mérite son propre design/tests

#### `retrieval.rs`, `temporal.rs` — Racine (utilitaires)

| Type | Rôle |
|------|------|
| `rrf_fuse` | Reciprocal Rank Fusion — fusionne N rankings par score `Σ 1/(k+rang)` |
| `Validity` | Fenêtre `valid_from`/`valid_until` — sert à construire le filtre de recall |

**Points clés** :
- RRF : déterministe, k=60 (Cormack et al.), préserve ordre stable
- Validité : simple struct, méthode `is_valid_at(now)` → sert à construire le filtre de recall

#### `error.rs` — Racine (transverse)

- `MemoryError` : Core, EncryptionRequired, MissingAgent, UnknownLayer, Inference, Extraction

#### `lib.rs`

- Réexporte l'API publique
- `now_unix()` interne — SystemTime → i64

---

## Flux d'exécution clés

### 1. Ouverture d'une mémoire

```
User: Memory::open_native(path, key, embedder, agent)
  ↓
  → NativeMemoryStore::open_encrypted(path, key)  (recovery WAL si besoin)
  → vérifie/écrit le contrat embedding (meta/bmai/)
  → Memory { store, embedder, agent }
```

### 2. Mémorisation

```
User: memory.remember(text, layer)
  ↓
  → embedder.embed(text) → vec (384d)
  → store.put_memory(id, agent, layer, text, valid_from, valid_until, emb)
     → UN batch WAL atomique : record + vecteur (index natif) + posting FTS
```

### 3. Recall

```
User: memory.recall(query, k)
  ↓
  → embedder.embed(query)
  → filtre = (agent = ? AND valid_from <= now AND (valid_until IS NULL OR valid_until > now))
  → store.recall_vector(agent, query_vec, k, filtre)   (KNN natif, oversampling ×8)
  → [Record { id, text, layer, score }, ...]  (hydratation depuis l'index mémoire)
  → touch last_access sur chaque record (écriture brève séparée)
```

### 4. Consolidation en tâche de fond

```
MaintenanceWorker::start()
  → spawn(async {
      sleep(every).await
      task.run().await
    })

ConsolidationTask::run()  (auto-suffisante via Arc<Memory>)
  → consolidate(&memory, llm)
    → recent_episodes(memory, 50)  (couche episodic, souvenirs valides)
    → llm.complete(prompt) → JSON text
    → parse RawExtraction { facts, entities, relations }
    → graph.add_entity, add_edge (upsert idempotent)
    → memory.remember(fact, Semantic) pour chaque fact
    → ConsolidationReport
```

> **Note** : GC temporel et oubli adaptatif (fenêtrage SQL `ROW_NUMBER()`
> libSQL-spécifique) ont été retirés avec libSQL (ADR-032). Un portage natif
> est un chantier dédié (design + tests), pas un portage en passant.

---

## Interfaces clés

### Extensibilité : traits injectables

| Trait | Implémentation requise | Injectée où |
|-------|---|---|
| `Embedder` | Chargement du modèle + `embed(text)` + `embed_batch(texts)` | ouverture de `Memory` |
| `MaintenanceTask` | `async fn run(&self)` | `MaintenanceWorker::register` |
| `LlmInference` | `async fn complete(prompt)` + `model_id()` | `ConsolidationTask::new` |

Aucune implémentation concrète de ces traits n'est forcée dans le crate — tout est injecté.

---

## Invariants critiques

1. **Agnosticité core** : zéro mention de `agent_id`, `valid_until`, métier
2. **Pas de surface SQL-leaky** : ni `Filter`, ni `Value`, ni `Store` ; le sens passe par le contrat `MemoryStore`
3. **Pas de DB externe** : moteur natif embarqué, store `.bmai` file-based
4. **Isolation agent** : scoping par `agent_id` structurel dans le layout de clé du moteur
5. **Chiffrement obligatoire basemyai** : l'ouverture sur disque rejette une clé absente
6. **Modèles non téléchargés** : `Embedder` reçoit chemin résolu, `LlmInference` ne télécharge pas
7. **Idempotence** : consolidation, graphe upserts, maintenance GC relançables

---

## Roadmap V2 (ne pas toucher)

- Multi-modèles embedding (sélection runtime)
- Portage natif du GC temporel + oubli adaptatif (design/tests dédiés)
- Sync multi-device (le WAL natif comme primitive de change-capture)
- Mémoire partagée inter-agents
- Explicabilité / provenance
