# Architecture — BaseMyAI

## Vue d'ensemble

BaseMyAI est organisée en **deux crates Rust** communiquant via imports Rust natifs :

1. **`basemyai-core`** : socle **business-agnostic**
   - Store libSQL async + KNN natif
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

- Le core expose **primitives** : `Store`, `KNN`, `Filter` paramétré, `MaintenanceWorker`
- Le consommateur applique le **sens** : filtre temporel via `Filter`, tâches métier injectées

**Exemple** : `vector_knn(query, k, filter?)`
- Core : "applique le WHERE fourni après le top-k natif"
- basemyai : "le WHERE est (`valid_from <= now AND valid_until IS NULL OR > now`)"

### 3. Pas de téléchargement silencieux (ADR-010)

- L'`Embedder` **ne télécharge jamais**, ne détecte jamais le matériel
- `setup::provision(consent)` fait le fetch explicite si `consent = true`
- Modèles détectés au démarrage, listé à l'utilisateur (même pour LLM via `choose_llm()`)

### 4. Chiffrement obligatoire dans basemyai (ADR-007)

- `basemyai-core` : chiffrement optionnel (feature `crypto`)
- `basemyai` : chiffrement **requis** — `Memory::open` échoue sans clé

### 5. Backend = libSQL (ADR-011)

- **Pas de DB externe**, pas de services externes obligatoires
- Vecteur **natif** libSQL (`F32_BLOB`, `libsql_vector_idx`, `vector_top_k`)
- Async toujours ; `Embedder` reste **sync** (CPU-bound)
- Chemin futur : Turso DB (pur Rust)

---

## Structure des modules

### basemyai-core

#### `storage/` — Recherche vectorielle + Store

| Module | Rôle |
|--------|------|
| `store.rs` | `Store` async — ouverture, migrations, `vector_upsert`, `vector_knn` |
| `vector.rs` | `Filter`, `Value`, `Neighbor` — paramétrage sans injection |

**Points clés** :
- `Filter { where_sql: String, params: Vec<Value> }` — fragment SQL + valeurs liées
- `vector_knn` sur-échantillonne ×8 si filtre présent (ADR-012)
- Distance cosinus réelle (recalculée, pas placeholder)
- Chiffrement au repos (libSQL feature `crypto`, optionnel en core)

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
| `MaintenanceTask` | Trait : `async fn run(&self, store: &Store)` |
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
| `schema.rs` | Migrations SQL (memory v1/v2/v3, entity/edge), `EMBEDDING_DIM` |
| `isolation.rs` | `AgentId` newtype — isolation SQL au level requêtes |
| `mod.rs` | Façade `Memory` : remember, recall, invalidate, forget, stats, search_graph |

**Points clés** :
- `Memory::open` applique le schéma, scelle par `AgentId` + `Embedder`
- `recall(query, k)` filtre par agent + temps via `Filter` paramétré
- `search_graph` : KNN + EXISTS sur graphe
- Metabase de validité : `valid_from`, `valid_until` (optionnel), `importance`, `last_access`

#### `cognition/` — Pipeline Phase 2

| Module | Rôle |
|--------|------|
| `inference.rs` | Trait `LlmInference` (object-safe) — abstraction fournisseur |
| `consolidation.rs` | `consolidate(memory, llm)` : épisodes → faits + graphe (idempotent) |
| `graph.rs` | `Graph` : add_entity, add_edge, traverse (CTE récursive) |

**Points clés** :
- Consolidation : texte brut du LLM → JSON structuré → peuplement graphe + promotion semantic
- Graphe : tables `entity`/`edge`, CTE récursive `UNION` (cycle-safe), scopé `agent_id` + `valid_until`
- Traverse : `SELECT ... FROM reach r JOIN entity e ... WHERE r.node <> start ...`

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
| `gc.rs` | `ExpiredMemoryGc` — DELETE where `valid_until <= now` |
| `forgetting.rs` | `AdaptiveForgetting` — score rétention (importance + récence hyperbolique) |
| `mod.rs` | `ConsolidationTask` — Arc<Memory> + Arc<dyn LlmInference> impl MaintenanceTask |

**Points clés** :
- GC : simple, aucun paramètre
- Forgetting : `ROW_NUMBER() OVER (PARTITION BY agent_id ORDER BY retention DESC)` → évince > capacity
- Récence hyperbolique (pas exponentielle) : `H / (H + age)`, reste dans (0, 1], distingue les grands âges
- Consolidation : ignore `_store` (utilise son propre via Arc<Memory>)

#### `retrieval.rs`, `temporal.rs` — Racine (utilitaires)

| Type | Rôle |
|------|------|
| `rrf_fuse` | Reciprocal Rank Fusion — fusionne N rankings par score `Σ 1/(k+rang)` |
| `Validity` | Fenêtre `valid_from`/`valid_until` — issu pour construire `Filter` |

**Points clés** :
- RRF : déterministe, k=60 (Cormack et al.), préserve ordre stable
- Validité : simple struct, méthode `is_valid_at(now)` → utiliser pour constructeur Filter

#### `error.rs` — Racine (transverse)

- `MemoryError` : Core, EncryptionRequired, MissingAgent, UnknownLayer, Inference, Extraction

#### `lib.rs`

- Réexporte l'API publique
- `now_unix()` interne — SystemTime → i64

---

## Flux d'exécution clés

### 1. Ouverture d'une mémoire

```
User: Memory::open(store, embedder, agent_id)
  ↓
  → store.migrate(&schema())
  → Memory { store, embedder, agent }
```

### 2. Mémorisation

```
User: memory.remember(text, layer)
  ↓
  → embedder.embed(text) → vec (384d)
  → conn.execute("INSERT INTO memory ...")
     (id, agent_id, layer, content, valid_from, valid_until, emb)
```

### 3. Recall

```
User: memory.recall(query, k)
  ↓
  → embedder.embed(query)
  → Filter::new("agent_id = ? AND valid_from <= ? AND (valid_until IS NULL OR valid_until > ?)", [...])
  → store.vector_knn("memory", query_vec, k, Some(&filter))
  → [Neighbor { id, distance }, ...]
  → conn.query("SELECT content, layer FROM memory WHERE id = ?")
  → [Record { id, text, layer, score }, ...]
  → UPDATE last_access on each record (for forgetting)
```

### 4. Consolidation en tâche de fond

```
MaintenanceWorker::start(store)
  → spawn(async {
      sleep(every).await
      task.run(&store).await
    })

ConsolidationTask::run(_store) [ignore _store]
  → consolidate(&memory, llm)
    → recent_episodes(memory, 50) [SELECT episodic where valid]
    → llm.complete(prompt) → JSON text
    → parse RawExtraction { facts, entities, relations }
    → graph.add_entity, add_edge (ON CONFLICT upsert)
    → memory.remember(fact, Semantic) pour chaque fact
    → ConsolidationReport
```

### 5. Oubli adaptatif

```
AdaptiveForgetting::run(store)
  → conn.execute("""
      DELETE FROM memory WHERE id IN (
        SELECT id FROM (
          SELECT id, ROW_NUMBER() OVER (
            PARTITION BY agent_id
            ORDER BY importance + H/(H+age) DESC
          ) rn FROM memory
        ) WHERE rn > capacity
      )
    """)
```

---

## Interfaces clés

### Extensibilité : traits injectables

| Trait | Implémentation requise | Injectée où |
|-------|---|---|
| `Embedder` | Chargement du modèle + `embed(text)` + `embed_batch(texts)` | `Memory::open` |
| `MaintenanceTask` | `async fn run(&self, store)` | `MaintenanceWorker::register` |
| `LlmInference` | `async fn complete(prompt)` + `model_id()` | `ConsolidationTask::new` |

Aucune implémentation concrète de ces traits n'est forcée dans le crate — tout est injecté.

---

## Invariants critiques

1. **Agnosticité core** : zéro mention de `agent_id`, `valid_until`, métier
2. **Paramétrage SQL** : tous les WHERE doivent passer par `Filter { where_sql, params }`
3. **Pas de DB externe** : libSQL embeddée, file-based ou `:memory:`
4. **Isolation agent** : toute requête SELECT/INSERT/UPDATE filtrée par `agent_id = ?`
5. **Chiffrement obligatoire basemyai** : `Memory::open` rejette clé absente
6. **Modèles non téléchargés** : `Embedder` reçoit chemin résolu, `LlmInference` ne télécharge pas
7. **Idempotence** : consolidation, graphe upserts, maintenance GC relançables

---

## Roadmap V2 (ne pas toucher)

- Multi-modèles embedding (sélection runtime)
- Migration Turso DB (pur Rust, zéro C)
- Sync multi-device
- Mémoire partagée inter-agents
- Explicabilité / provenance
