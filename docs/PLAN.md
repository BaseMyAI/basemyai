# Plan : dominer le marché des moteurs de mémoire pour agents IA

**Date** : 2026-06  
**Statut** : stratégie active — à traduire en ADR et sprints  
**Périmètre** : BaseMyAI, de l'état actuel (Phase 2 implémentée, SDKs absents) jusqu'à la sortie V1 des bindings + MCP

---

## 1. Diagnostic honnête

### Ce que le marché propose aujourd'hui

Les bases de données multi-modèles généralistes (document + graphe + vecteur + relationnel) ont atteint une maturité technique réelle en 2026. Elles offrent :

- Des index vectoriels ANN avancés (HNSW, DiskANN) avec 5–6 métriques de distance
- Un MCP server intégré, prêt à l'emploi, sécurisé
- Des protocoles multiples : REST, WebSocket texte, WebSocket binaire (CBOR)
- Des SDKs dans les langages majeurs avec prebuilds prêts à `pip install` / `npm install`
- Une couche d'authentification et de permissions granulaires intégrée au schéma
- Des live queries pour les agents réactifs

**Ce qu'elles ne font pas :**

Elles posent une couche de stockage généraliste. La mémoire d'un agent IA — avec sa temporalité, son oubli adaptatif, sa consolidation épisodique, son isolation multi-agent comme invariant de sécurité, ses embeddings in-process — est une **responsabilité laissée au développeur**. Il doit tout câbler à la main : `valid_from/until`, les couches sémantiques, le GC, la consolidation LLM, le provisioning hardware-aware.

En pratique, les builders d'agents passent des semaines à réinventer ce que BaseMyAI fournit nativement.

### Où nous en sommes

**Points forts réels (Phase 2 ✅) :**
- Moteur complet en Rust : libSQL + vecteur natif ANN + Candle async
- 4 couches mémoire, RAG temporel, isolation `agent_id` comme invariant SQL
- Graphe entités/relations (CTE récursive cycle-safe), RRF multi-signal
- Oubli adaptatif (hyperbolique, libSQL-safe), consolidation idempotente
- Provisioning hardware-aware + 8 backends LLM OpenAI-compat détectés
- Chiffrement au repos (feature `crypto`), mono-fichier `.db`

**Points faibles critiques (ce qui bloque la mise sur le marché) :**
- Aucune surface MCP — la table stakes du marché agent en 2026
- Aucun SDK : PyO3 et NAPI-RS non démarrés, sidecar REST sans spec
- Bridge async Rust ↔ Python asyncio / Node event loop non résolu
- Métriques de distance : cosine seulement — euclidean et hamming manquent
- Matrice CI de build non définie (wheels, prebuilds)
- Pas de live queries / notifications de changement de mémoire

---

## 2. Thèse de victoire

**Nous ne gagnons pas en faisant pareil en mieux. Nous gagnons en faisant ce qu'ils ne peuvent structurellement pas faire.**

### Axe 1 — Privacy-first n'est pas un feature flag, c'est une architecture

Les solutions cloud-native ont une offre hébergée en vitrine. Leur architecture est conçue pour le réseau. Le "mode embarqué" est une option, pas le cœur.

Chez nous, **zéro réseau par défaut est un invariant de compilation**, pas une option de configuration. L'embedding s'exécute in-process. La mémoire ne quitte jamais la machine sans action explicite de l'opérateur. Le fichier `.db` est chiffré au repos avec une clé que nous ne stockons jamais.

C'est invendable pour eux sans casser leur offre cloud. C'est notre position naturelle.

**Action produit :** Rendre ce point de différenciation visible dans la doc, la CLI (`fmyai setup` imprime le device détecté et confirme "zero network"), et le README. Pas un bullet point — une garantie contractuelle avec test CI (`cargo test --features no-network`).

### Axe 2 — La mémoire d'agent est un problème résolu, pas un problème à résoudre

Avec une base généraliste, un builder d'agents doit implémenter :
- Le système de validité temporelle (et son GC)
- L'isolation multi-agent (et s'assurer qu'elle ne peut pas être contournée)
- L'oubli adaptatif (et choisir sa formule de décroissance)
- La consolidation épisodes → faits (et gérer l'idempotence)
- Le provisioning du modèle d'embedding (et la détection hardware)
- La fusion multi-signal RRF
- Le graphe entités/relations

Avec BaseMyAI : `memory.remember(text, agent_id, layer)`. C'est tout.

**Action produit :** Le README principal ne doit pas expliquer l'architecture interne. Il doit montrer les 5 lignes de code qui remplacent les semaines de câblage. Ensuite, et seulement ensuite, l'architecture.

### Axe 3 — Les SDKs sont meilleurs parce que le cœur est dans le binaire

Les solutions génériques exposent un serveur et font appeler les SDKs via réseau. Leurs SDKs Python/JS font des appels HTTP/WebSocket. C'est correct, mais ça impose un daemon, une latence réseau (même sur loopback), et une gestion de connexion.

Nos SDKs PyO3 et NAPI-RS **lient le cœur Rust directement dans le process Python ou Node**. Zéro réseau, zéro daemon, zéro latence de transport. `pip install basemyai` et la mémoire tourne dans le même process que l'agent.

C'est une différence d'expérience développeur fondamentale.

**Risque à gérer :** le bridge async Rust ↔ Python/Node est le problème technique le plus difficile de cette architecture. Il doit être résolu en spike avant de commencer les SDKs (voir §4).

---

## 3. Ce qu'on doit combler — par ordre de priorité

### P0 — Bloquants absolus (rien ne peut sortir sans ça)

#### P0.1 — MCP server

**Pourquoi c'est P0 :** Les builders d'agents IA configurent leur toolbox via MCP. Un moteur de mémoire sans surface MCP en 2026 n'existe pas pour eux. C'est la porte d'entrée.

**Ce que ça implique :**
```
basemyai-mcp (nouveau crate dans le workspace)
  ├─ transport stdio  → basemyai setup | basemyai mcp
  ├─ transport HTTP   → port 7743 (configurable)
  └─ outils MCP exposés :
       remember(text, layer, agent_id, valid_until?)
       recall(query, agent_id, k?, layer?)
       recall_graph(entity_id, agent_id, depth?)
       invalidate(memory_id, agent_id)
       forget(agent_id)
       stats(agent_id)
```

**Sécurité :**
- Stdio : l'appelant est l'opérateur — pas d'auth par appel (comme le concurrent, mais avec un warning explicite dans la doc)
- HTTP : clé API dans header `Authorization: Bearer` ; clé générée au `basemyai setup`, jamais hardcodée
- Audit log : chaque tool call → `tracing::info!` avec `tool`, `agent_id`, `outcome`, `time_ms` — jamais le contenu des données

**Implémentation :** `rmcp` ou `mcp-rs`. Le crate `basemyai-mcp` dépend de `basemyai`, pas de `basemyai-core`.

#### P0.2 — Spike async bridge (débloquer PyO3 + NAPI)

**Pourquoi c'est P0 :** Sans valider que le bridge async fonctionne, on peut écrire du PyO3 qui bloque le GIL Python ou qui provoque des deadlocks sur l'event loop Node. Mieux vaut 2 jours de spike que 3 semaines de débogage en fin de projet.

**Décisions à prendre dans le spike :**

*Python :*
```rust
// Option A — API sync (simple, pas de GIL issue)
#[pyfunction]
fn remember(text: &str, agent_id: &str, layer: &str) -> PyResult<String> {
    RUNTIME.block_on(async { memory.remember(...).await })
}

// Option B — API async (meilleure DX pour les async agents)
#[pyfunction]
fn remember<'py>(py: Python<'py>, text: &str, ...) -> PyResult<Bound<'py, PyAny>> {
    pyo3_asyncio_0_21::tokio::future_into_py(py, async move {
        memory.remember(...).await.map_err(Into::into)
    })
}
```

**Recommandation :** Option B (async natif). En 2026, les frameworks agents Python (LangChain, LlamaIndex, smolagents) sont tous async. Un SDK sync force des `asyncio.to_thread()` partout.

*Node.js :*
```rust
// NAPI-RS avec threadsafe_function
#[napi]
pub async fn remember(text: String, agent_id: String, layer: String) -> napi::Result<String> {
    memory.remember(&text, &agent_id, ...).await
        .map_err(|e| napi::Error::from_reason(e.to_string()))
}
```

NAPI-RS 3.x gère l'async nativement. Le runtime tokio est partagé via `once_cell`.

**Livrable du spike :** Un crate `basemyai-py` qui expose `remember` + `recall` et passe un test Python async (`pytest-asyncio`). Un crate `basemyai-node` qui fait la même chose côté Node. Si ça marche : on code les SDKs. Si ça bloque : on ajuste l'architecture avant d'aller plus loin.

#### P0.3 — Spec du protocole wire (sidecar REST)

**Pourquoi c'est P0 :** Le sidecar REST est la surface pour Go, Ruby et les langages sans binding natif. Sans spec, chaque endpoint sera incohérent avec les SDKs natifs.

**Spec minimale :**

```
POST /v1/remember
  Body: { text, agent_id, layer, valid_until? }
  Response: { id, created_at }

POST /v1/recall
  Body: { query, agent_id, k?, layer?, include_graph? }
  Response: { results: [{ id, text, score, layer, contributions? }] }

POST /v1/recall_graph
  Body: { entity_id, agent_id, depth }
  Response: { entities: [...], edges: [...] }

DELETE /v1/memories/{id}?agent_id=
  → invalidate

DELETE /v1/agent/{agent_id}
  → forget (suppression complète)

GET /v1/agent/{agent_id}/stats
  Response: { counts_by_layer, oldest_memory, newest_memory }
```

Format : JSON. Auth : `Authorization: Bearer <key>`. Pas de CBOR en V1 (inutile pour le sidecar REST, qui n'est pas le chemin critique de perf).

---

### P1 — Importants, font la différence en V1

#### P1.1 — Table de mapping de types cross-language

À définir avant de coder les SDKs pour garantir la cohérence. Principe : le SDK doit être **idiomatique dans son langage**, pas une translitération du Rust.

| Type Rust | Python | TypeScript |
|---|---|---|
| `MemoryLayer` | `MemoryLayer` (str enum : `"short_term"`, `"episodic"`, `"procedural"`, `"semantic"`) | `"short_term" \| "episodic" \| "procedural" \| "semantic"` |
| `AgentId` (`&str`) | `str` | `string` |
| `Vec<f32>` (embedding) | `list[float]` (ou `numpy.ndarray` optionnel) | `Float32Array` |
| `valid_until: Option<i64>` | `datetime \| None` (auto-converti UTC) | `Date \| null` |
| `Fused` (résultat RRF) | `@dataclass MemoryResult` | `interface MemoryResult` |
| `CoreError` / `MemoryError` | `BasemyaiError` (hiérarchie `ValueError`, `RuntimeError`) | `BasemyaiError extends Error` (avec `code: string`) |
| `Record` (mémoire) | `@dataclass Memory` | `interface Memory` |

Règle : les enums Rust deviennent des string literals (pas des int). Les options deviennent des `None`/`null`, jamais des `0` ou `""`.

#### P1.2 — Métriques de distance additionnelles

Cosine seule est insuffisante pour tous les cas d'usage :
- **Euclidean** : clustering spatial, embeddings image-text
- **Hamming** : fingerprints binaires, perceptual hashing
- **Manhattan** : robustesse aux outliers

**Implémentation :** libSQL `vector_distance_cos` est la seule disponible nativement. Pour les autres métriques, deux options :
1. Récupérer les vecteurs bruts et calculer en Rust post-top-k (acceptable si k est petit)
2. Attendre Turso DB pur Rust (V2/V3)

En V1 : implémenter euclidean en post-top-k Rust avec oversampling ×16. Documenter honnêtement que cosine utilise l'index ANN et que les autres métriques font un scan partiel.

#### P1.3 — Matrice CI de build

**Cibles :**

```yaml
# wheels Python (maturin)
matrix:
  os: [ubuntu-22.04, macos-14, windows-2022]
  python: ["3.10", "3.11", "3.12", "3.13"]
  arch: [x86_64, aarch64]  # aarch64 seulement sur macOS et Linux
  features: [default, embed, crypto]

# prebuilds Node (napi-build)
matrix:
  os: [ubuntu-22.04, macos-14, windows-2022]
  node: [18, 20, 22]
  arch: [x86_64, aarch64]
```

**Attention Windows + feature `crypto` :** CMake via pip + `cp` de Git sur PATH requis (voir mémoire `libsql-windows-crypto-build.md`). Cette étape doit être scriptée dans le workflow CI avant `cargo build`.

**Tooling :**
- `maturin build --release` pour les wheels Python
- `napi build --release` pour les prebuilds Node
- GitHub Actions + cross-rs pour la cross-compilation Linux aarch64

---

### P2 — V2 et différenciation longue durée

#### P2.1 — Live queries / subscriptions

Permet aux agents réactifs de s'abonner à des changements de mémoire :
```python
async for event in memory.watch(agent_id="agent-1", layer="semantic"):
    # un autre agent a mis à jour un fait → re-plan
    await replanner.handle(event)
```

**Implémentation :** canal tokio broadcast dans `basemyai`, exposé via WebSocket dans le sidecar, via callback en PyO3 et NAPI.

#### P2.2 — Mémoire partagée volontaire entre agents (ADR-006 relax contrôlé)

ADR-006 interdit la mémoire partagée en V1. En V2 : `memory.share(memory_id, from_agent, to_agent)` avec audit log obligatoire. Utile pour les systèmes multi-agents avec mémoire collaborative.

#### P2.3 — Export / import `.bmem` (portabilité de la mémoire)

Format d'archive portable pour la mémoire : contient le store chiffré + le manifeste (model_id, dim, version). Permet de déplacer la mémoire d'un agent entre machines sans recréer les embeddings.

#### P2.4 — Interface Surrealism / WASM (si besoin navigateur)

Pas en scope V1 (privacy-first = local). Si un cas d'usage navigateur émerge (ex. : agent WebAssembly dans un VSCode extension web), `basemyai-core` pourrait être compilé en WASM via `wasm-bindgen`. Bloquant actuel : libSQL n'est pas WASM-compatible. Turso DB (pur Rust, V3) ouvrirait ce chemin.

---

## 4. Séquençage précis

### Phase A — Débloquer la mise sur le marché (4–6 semaines)

```
Semaine 1
  ├─ Spike async bridge Python (pyo3 0.21 + tokio)          [2j]
  └─ Spike async bridge Node (napi-rs 3 + tokio)            [2j]
      → Si les deux passent : go SDK. Sinon : ajuster archi.

Semaine 2
  ├─ Spec wire sidecar REST (doc + OpenAPI 3.1)              [1j]
  ├─ Table de mapping de types cross-language (doc)          [1j]
  └─ MCP server : définition des outils + spec sécurité      [2j]

Semaine 3–4
  ├─ crate basemyai-mcp : transport stdio + HTTP             [5j]
  │    remember / recall / recall_graph / invalidate / stats
  └─ Tests MCP : in-process + stdio (miroir de la prod)      [2j]

Semaine 5–6
  ├─ crate basemyai-py : PyO3 async, tous les outils         [5j]
  ├─ crate basemyai-node : NAPI-RS async, tous les outils    [5j]
  └─ CI matrix : maturin + napi-build, Linux/macOS/Windows   [3j]
```

### Phase B — Solidifier et différencier (4 semaines)

```
Semaine 7–8
  ├─ Sidecar REST : implémentation axum                      [4j]
  ├─ Métriques euclidean + hamming (post-top-k)              [2j]
  └─ Tests cross-SDK : même comportement Python / Node / REST [3j]

Semaine 9–10
  ├─ Wiring consolidation dans MaintenanceWorker             [3j]
  │    (nécessite Arc<Memory> + LLM provider dans la tâche)
  ├─ Doc getting-started : 5 lignes en Python, 5 en TS       [2j]
  └─ Benchmarks : latence recall in-process vs réseau        [2j]
```

### Phase C — Publication (2 semaines)

```
Semaine 11–12
  ├─ Publication crates.io : basemyai-core, basemyai          [1j]
  ├─ Publication PyPI : basemyai (wheels précompilés)         [1j]
  ├─ Publication npm : basemyai (prebuilds)                   [1j]
  ├─ Publication binaire sidecar : GitHub Releases            [1j]
  └─ README principal : pitch 5 lignes + garanties privacy    [2j]
```

---

## 5. Comment on gagne le benchmark de la communauté

Les builders d'agents comparent sur ces axes. Voici notre position cible :

### Intégration (DX)

```python
# Ce qu'on veut que les gens voient
from basemyai import Memory

memory = Memory(path="~/.myagent/memory.db")  # chiffré, hardware-aware, local
await memory.remember("L'utilisateur préfère les réponses courtes", agent_id="agent-1")
results = await memory.recall("préférences utilisateur", agent_id="agent-1")
```

vs une base généraliste :
```python
# Ce qu'il faut câbler à la main
client = SomeDb()
now = datetime.utcnow()
await client.query("""
    INSERT INTO memory SET
        text = $text, agent_id = $agent_id, layer = 'semantic',
        valid_from = $now, valid_until = NONE,
        embedding = $embedding
""", {"text": text, "agent_id": agent_id, "now": now, "embedding": embed(text)})
```

Notre SDK doit rendre ce contraste évident dans la doc.

### Privacy

| Critère | Nous | Bases généralistes |
|---|---|---|
| Données sur disque | Chiffrées AES-256 (SQLCipher) | Dépend de la config |
| Réseau par défaut | **Zéro** | Dépend du mode |
| Embedding externe | **Jamais** (in-process Candle) | Souvent (API cloud) |
| Offre cloud | **Non** (par conception) | Oui (business model) |
| Clé de chiffrement | Jamais stockée | N/A |

### Performance (recall local)

L'avantage in-process est quantifiable. Mesures cibles à publier :

| Scénario | Nous (in-process PyO3) | Serveur local (loopback) |
|---|---|---|
| `recall(q, k=5)` p50 | < 2 ms | ~8–15 ms (+ sérialisation + réseau) |
| `remember(text)` p50 | < 5 ms (embed + write) | ~20 ms |

Ces chiffres doivent figurer dans le README, mesurés sur hardware réel et reproductibles en CI.

### Richesse sémantique

Fonctionnalités que les bases généralistes ne donnent pas clé en main :

| Feature | Nous | Bases généralistes |
|---|---|---|
| Temporal RAG | ✅ natif | ❌ à implémenter |
| Oubli adaptatif | ✅ natif | ❌ à implémenter |
| 4 couches mémoire | ✅ natif | ❌ à implémenter |
| Isolation agent (invariant SQL) | ✅ natif | ⚠️ à câbler |
| Graphe entités/relations | ✅ natif | ⚠️ à modéliser |
| Consolidation LLM | ✅ natif | ❌ à implémenter |
| Provisioning hardware-aware | ✅ natif | ❌ à implémenter |

---

## 6. Invariants à ne jamais sacrifier pour "couvrir les gaps"

Ces invariants définissent ce qu'on est. Les abandonner pour ressembler aux solutions génériques serait se tirer une balle dans le pied.

1. **Zéro réseau par défaut.** Même si ça rend WASM impossible en V1.
2. **L'isolation `agent_id` est un invariant SQL, jamais une option.** Même si des utilisateurs demandent une mémoire "globale non isolée" — refuser ou implémenter en V2 avec audit log obligatoire.
3. **L'`Embedder` ne télécharge jamais.** Le fetch est dans le setup, explicite et consenti.
4. **Mono-fichier chiffré.** Pas de second système de stockage externe, jamais.
5. **`basemyai-core` reste agnostique.** Le test d'agnosticité en CI est non-négociable.
6. **Aucun `unwrap()` sans message en lib.** Qualité Rust 2026, pas négociable.

---

## 7. Décisions ouvertes à trancher avant Phase A

| # | Question | Recommandation |
|---|---|---|
| D1 | Python sync ou async en V1 ? | **Async** (pyo3 0.21 + tokio). Les frameworks agents 2026 sont tous async. |
| D2 | Port par défaut du sidecar REST ? | **7743** (mémorable, non conflictuel). Configurable via `--port`. |
| D3 | Port par défaut du MCP HTTP ? | **7744** (adjacent au sidecar). |
| D4 | Nom du crate PyPI ? | `basemyai` (correspond au crate Rust). |
| D5 | Nom du package npm ? | `basemyai` ou `@basemyai/sdk` (scope npm pour le futur). |
| D6 | Format d'archive mémoire portable ? | À spécifier en V2. En V1 : export JSON via le sidecar REST suffit. |
| D7 | Métriques de distance V1 ? | Cosine (ANN natif) + Euclidean (post-top-k ×16). Hamming en V2. |

---

## Appendice — Référence rapide des crates à créer

```
basemyai-mcp      → MCP server (stdio + HTTP), outils agent, audit log
basemyai-py       → Binding PyO3, wheel précompilé, API async
basemyai-node     → Binding NAPI-RS, prebuild, API async Promise
basemyai-rest     → Sidecar REST axum, spec OpenAPI 3.1
```

Tous dépendent de `basemyai` (pas de `basemyai-core` directement). Ils ne connaissent ni les types code-domain (`Symbol`/`Edge`), ni les internals du core. La séparation tient.
