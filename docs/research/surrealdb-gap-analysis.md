# Gap analysis SurrealDB ↔ BaseMyAI — ce qu'ils ont que nous n'avons pas (encore)

- Date : 2026-06-13
- Statut : **analyse / recommandations** (pas un ADR — toute décision retenue ici se matérialisera par un ADR dédié)
- Sources : crates `analayse/surrealdb/surrealdb/{core,mcp}` (jamais couverts par les 4 docs précédents) + inventaire exhaustif de nos crates (`basemyai-core`, `basemyai`, `basemyai-mcp`, `basemyai-rest`, bindings). Analyse menée par 3 agents d'exploration parallèles, claims clés re-vérifiés sur source.
- Complète : `surrealdb-patterns.md` (patterns code de `strand`/`server`/`src`), `surrealdb-strand-analyse.md`, `surrealdb-server-analyse.md`, `surrealdb-sdk-analyse.md`.

> Garde-fou transverse inchangé : `basemyai-core` reste **agnostique métier**.
> Mécanisme au core, sens au consommateur. Aucun emprunt ci-dessous ne justifie
> de violer ça.

---

## 0. Cadrage — la comparaison juste

SurrealDB est une **base généraliste multi-modèle** (documents, graphe, vecteur,
FTS, temps réel) que l'agent pilote **en SurrealQL**. BaseMyAI est un **moteur
de mémoire opinioné** : l'agent appelle `remember`/`recall`, pas du SQL. Le but
n'est donc **pas** de devenir SurrealDB — c'est de repérer ce qui, chez eux, rend
une « database pour agents IA » *proposable et utilisable*, et qui nous manque.

### Ce que NOUS avons qu'ils n'ont PAS (à protéger, c'est le pitch)

| Capacité | BaseMyAI | SurrealDB |
|---|---|---|
| **Embeddings in-process** (texte → vecteur intégré) | Candle BERT, 100 % local, zéro réseau | ❌ aucun embedding côté serveur — ne fait que stocker des vecteurs fournis par le client (vérifié : pas de `ml::embed`, SurrealML = stockage/versioning de modèles, pas d'exécution) |
| **Sémantique mémoire métier** | 4 couches, validité temporelle, oubli adaptatif, consolidation épisodes→faits, purge RGPD | ❌ mécanismes bruts ; tout est à construire par le client |
| **Surface agent au niveau tâche** | `remember`/`recall` : zéro charge cognitive, pas de langage de requête à maîtriser | l'agent doit écrire du SurrealQL (puissant mais risqué/verbeux) |
| **Chiffrement au repos obligatoire** | par défaut dans `basemyai` (libSQL `crypto`) | optionnel/édition |
| **Provisioning hardware-aware** | détection matériel + fetch explicite consenti | ❌ |
| **Embarquable comme crate fine** | `Arc<Memory>` in-process | SDK lourd ou serveur ; l'embedded tire le kitchen-sink |

C'est exactement le gap **dans l'autre sens** : un dev qui veut une mémoire
d'agent sur SurrealDB doit construire embeddings, couches, oubli, consolidation,
isolation. Nous le livrons. Reste à être *au niveau* sur l'opérationnel et la
surface agent — c'est l'objet des sections suivantes.

---

## 1. Surface MCP/agent — le gap produit n°1 (et le moins cher à combler)

### Eux (`mcp/`, SDK `rmcp`, stdio + Streamable HTTP)

- **13 outils** (CRUD complet, `query`, `relate`, `run`, `info`, `list` sur 19
  sortes d'entités, `use`) — normal, c'est une base généraliste.
- **Resources MCP** : `surrealdb://instructions`, `surrealdb://info`,
  `surrealdb://version`, + **templates URI de schéma**
  (`surrealdb://schema/ns/{ns}/db/{db}[/table/{t}]`) — le LLM *introspecte* la
  base comme une ressource de première classe au lieu de deviner.
- **6 prompts MCP** (`query_builder`, `schema_explorer`, `data_modeler`,
  `transaction_guide`, `graph_traversal`, `search_guide`) — des guides d'usage
  *embarqués dans le serveur*, découvrables par le client (`mcp/src/prompts/`).
- **Completions** : auto-complétion d'arguments d'outils par énumération live
  (tables, namespaces…) — `mcp/src/completions.rs`.
- **Caps configurables pour le contexte LLM** : `max_result_bytes` (256 KiB),
  `params_max_keys`, `run_max_args`, timeout requête (60 s), via env
  `SURREAL_MCP_*` — `mcp/src/cnf.rs`.
- **Sentinel `$ql`** dans le JSON pour exprimer les types natifs (datetime,
  duration, record id…) que JSON ne représente pas.
- **Auth liée à la session** : `BoundSubject` vérifié *par requête* sur les
  transports réseau (anti-hijack) — `mcp/src/auth.rs`.
- **Métriques par outil** (`surrealdb.mcp.tool.*`, outcome Success/Timeout/
  Truncated/…) + audit log dédié — `mcp/src/metrics.rs`, `audit.rs`.

### Nous (`basemyai-mcp`)

6 outils (`remember`, `recall`, `recall_hybrid`, `recall_graph`, `invalidate`,
`stats`), stdio + HTTP, Bearer temps constant, audit minimal (sans contenu).
**Pas** de resources, **pas** de prompts, **pas** de completions, caps fixes,
pas de métriques.

### Verdict : **Adopter — c'est le cœur de « database pour agents »**

Concrètement, par coût croissant :

1. **Resources** : `basemyai://instructions` (comment bien utiliser la mémoire :
   quelle couche pour quoi, quand invalider), `basemyai://capabilities`
   (features actives : crypto, hybrid, graphe, dimension/modèle d'embedding),
   `basemyai://agent/{id}/stats`. Quasi gratuit, gros gain d'autonomie du LLM.
2. **Prompts** : `memory_guide` (politique de mémorisation), `recall_strategy`
   (vector vs hybrid vs graph selon le besoin), `consolidation_review`.
3. **Caps configurables** (`BASEMYAI_MCP_MAX_RESULT_BYTES`, `…_TIMEOUT_SECS`) —
   on a déjà `truncated: bool` dans les réponses, il manque les knobs.
4. **Métriques d'outcome par outil** (tracing + compteurs) — aligné avec le
   constat « observabilité quasi absente » de l'inventaire.

Le tout reste au niveau **surface** (basemyai-mcp), zéro impact core. Manque
notable côté outils (pas un emprunt SurrealDB, mais révélé par la comparaison) :
`forget` et `purge_agent` existent dans `Memory` et REST mais **pas en MCP**.

---

## 2. Atomicité, transactions, batch — le gap correctness

### Eux
ACID partout, MVCC/OCC avec retry au commit, savepoints, API batch
(`core/src/kvs/tr.rs`, `tx.rs`).

### Nous
- `Memory::remember` fait **deux écritures** (table `memory` vectorisée + miroir
  FTS5 `memory_fts`) **sans transaction** : un crash entre les deux laisse le
  recall hybride incohérent (souvenir trouvable par vecteur, invisible en BM25 —
  ou l'inverse pour `forget`).
- **Aucun batch** : pas de `remember_batch` alors que `Embedder::embed_batch`
  existe et que l'ingestion initiale (import d'un historique de conversation)
  est LE premier geste d'un nouvel utilisateur.

### Verdict : **Adopter — P0**
Transaction libSQL autour de chaque écriture multi-tables (`remember`,
`forget`, `purge_agent`, upserts de consolidation) ; `remember_batch(texts,
layer)` qui embedde par lot puis insère dans une seule transaction. Pas besoin
du modèle MVCC complet de SurrealDB — libSQL fournit la transaction, il suffit
de l'utiliser.

---

## 3. Export / import / backup — le gap « produit installable »

### Eux
Export **sélectif** streamé (users, fonctions, tables incluses/exclues, records,
versions…), import streamé, script de démarrage (`core/src/kvs/export.rs`).

### Nous
**Rien.** Ni backup, ni export, ni import. Pour un produit local-first c'est
bloquant : pas de portabilité de la mémoire d'un agent entre machines, pas de
sauvegarde avant migration, et surtout **aucun chemin de migration de modèle
d'embedding** (V1 = baseline unique, mais le jour où on change de modèle, il
faut ré-embedder — donc exporter le texte et réimporter).

### Verdict : **Adopter — P0**
`Memory::export(agent) -> JSONL` (records + entités + arêtes + validité +
importance ; embeddings exclus par défaut puisque re-calculables) et
`Memory::import(jsonl)` qui ré-embedde à l'ingestion. Ça donne en un geste :
backup, portabilité, RGPD (droit à la portabilité), et le chemin multi-modèles
V2. Exposer ensuite en REST/MCP/bindings.

---

## 4. Recherche plein-texte — exposer ce qu'on a déjà

### Eux
BM25 paramétrable (k1/b), analyzers custom (tokenizers + filtres, snowball),
`search::highlight`/`offsets`/`score`, opérateur `@@`, **`search::rrf` natif**
(`core/src/idx/ft/`, `core/src/fnc/search.rs`).

### Nous
FTS5 + BM25 existent (migration V4) mais **enfouis** dans `recall_hybrid` :
pas de recherche mot-clé seule, pas de snippet/highlight. Notre RRF
(`rrf_fuse`, k=60) est équivalent au leur — ce point-là est couvert.

### Verdict : **Adopter (petit) — P1**
`recall_keyword(query, k)` exposant BM25 seul, avec `snippet()`/`highlight()`
FTS5 (fonctions natives SQLite, coût quasi nul). Utile quand l'agent cherche un
identifiant exact, un nom propre, un code — cas où le vecteur est mauvais.
Analyzers custom : **Écarter** (anglais/multilingue baseline suffit en V1).

---

## 5. Temps réel — LIVE queries / change feeds : verdict révisé

### Eux
LIVE queries avec broker de notifications injectable
(`core/src/kvs/ds.rs::live_query_broker`, `doc/lives.rs`) + change feeds
persistés avec diff optionnel et GC (`core/src/cf/`).

### Nous
Rien. `surrealdb-patterns.md` §2 avait acté **Écarter en V1** (« pas de cas
d'usage temps-réel mémoire »).

### Verdict : **Adapter — P1, révision motivée du verdict précédent**
Le cas d'usage existe dès qu'il y a **deux** consommateurs de la même mémoire :
agent A apprend → agent B (ou l'UI, ou le worker de consolidation) doit le
savoir sans polling. La version BaseMyAI n'est **pas** un broker généraliste :

- un `tokio::sync::broadcast<MemoryEvent>` émis par la façade `Memory`
  (`Remembered { id, layer }`, `Invalidated`, `Forgotten`, `Consolidated`),
- consommé par : MCP (notifications), REST (un endpoint SSE), et le
  `MaintenanceWorker` (déclencher la consolidation *à l'écriture d'épisodes*
  plutôt qu'à intervalle fixe — meilleure réactivité, moins de réveils à vide).

**Écarter** les change feeds persistés : notre modèle `Validity`
(valid_from/until) + l'audit couvrent déjà l'historique *métier* ; un journal
de mutations bas niveau est de la plomberie sans demande.

---

## 6. Index vectoriel — nuance importante, gap modéré

### Eux
HNSW maison production-ready (compaction async des pending, cache vecteurs,
9 distances, types compressés F64→I8/U8) + **DiskANN** (sharding du pending
state) — `core/src/idx/trees/{hnsw,diskann}/`.

### Nous
L'agent d'analyse a d'abord conclu « brute force KNN » — **c'est faux** :
l'index natif libSQL derrière `vector_top_k` est un ANN de la famille
**DiskANN (LM-DiskANN)**, maintenu in-DB. Nous sommes donc structurellement au
niveau. Les vrais écarts :

- **Types compressés** : libSQL supporte `FLOAT16`/`FLOAT8`/`1BIT`, nous
  n'exploitons que `F32_BLOB`. Gain mémoire ×2–×4 possible, mais change le
  format `.idx` → **V2, ADR requis** (invariant compat baseline V1).
- **Métriques** : cosine natif + Euclid/Hamming en re-rank Rust chez nous vs 9
  natives chez eux. Suffisant pour de la mémoire sémantique. **Écarter.**
- **Oversampling fixe ×8 avec filtre** : leur planner ajuste ; le nôtre est
  constant. À revisiter seulement si le recall@k mesuré se dégrade avec des
  filtres très sélectifs. **Différer (post-bench).**

---

## 7. Permissions / partage inter-agents

### Eux
Row-level **et** field-level security par expression (`PERMISSIONS … WHERE
owner == $auth.id`), rôles, JWT/JWKS, hiérarchie root/ns/db/record
(`core/src/iam/`, `catalog/schema/`).

### Nous
Isolation **structurelle** par `AgentId` (plus simple, plus sûre — pas de
policy à se tromper) + Bearer unique par serveur. Mais : **aucun partage
contrôlé**. Pas de mémoire d'équipe, pas de « l'agent reviewer lit la mémoire
de l'agent codeur en lecture seule ».

### Verdict : **Adapter — V2, ADR requis**
Pas d'IAM générique. Un concept de **scope** : la mémoire privée d'agent reste
le défaut ; on ajoute des espaces partagés (`scope_id`) auxquels des agents
sont abonnés en lecture (ou écriture). Mécaniquement, ça passe par le `Filter`
paramétré existant — zéro impact core. C'est une décision de produit
multi-agents, pas un correctif : à instruire quand le besoin multi-agents est
confirmé.

---

## 8. Écartés (avec raison courte)

| Capacité SurrealDB | Raison de l'écarter |
|---|---|
| Events/triggers en base (`DEFINE EVENT`, sync/async + retry) | Notre équivalent est le `MaintenanceWorker` + l'événementiel §5 ; un moteur de triggers générique en base est hors scope mémoire |
| Computed fields | L'auto-embedding *est* notre computed field, déjà câblé dans `remember` |
| Vues matérialisées / index `Count` | `stats()` est borné par agent ; optimiser si profilage le montre |
| Schemafull/schemaless, types riches (geometry, closures, ranges) | Nous ne sommes pas une base généraliste ; notre schéma est le produit |
| WASM plugins (`surrealism/`), scripting JS | Extensibilité kitchen-sink ; notre extensibilité = traits Rust injectés (`LlmInference`, `MaintenanceTask`) |
| Buckets/blob storage, GeoJSON, GraphQL, `DEFINE API` | Hors scope mémoire d'agent local |
| MVCC/OCC multi-backend, versionstamps HLC | libSQL transactionnel suffit pour un moteur embarqué mono-nœud |

---

## 9. Synthèse et roadmap

| # | Gap | Verdict | Priorité | Où |
|---|---|---|---|---|
| 1 | Atomicité écritures multi-tables + `remember_batch` | Adopter | **P0 — ✅ fait (2026-06-13)** : `Store::begin_write()`/`WriteTxn` au core ; `remember`/`forget`/`purge_agent` transactionnels + `remember_batch` | `basemyai` + core |
| 2 | Export/import par agent (JSONL, ré-embed à l'import) | Adopter | **P0 — ✅ fait (2026-06-13)** : `Memory::export_jsonl`/`import_jsonl`, atomique + idempotent | `basemyai` puis surfaces |
| 3 | Boucle cognitive complète : provider `LlmInference` concret + wiring consolidation | (interne, révélé par comparaison) | **P0 — ✅ couvert (2026-06-13)** : `OpenAiCompatBackend` (ex-`OllamaBackend`) implémente `LlmInference` + timeouts ; reste un test E2E contre un vrai serveur local | `basemyai` |
| 4 | MCP : resources + prompts + caps + métriques ; outils `forget`/`purge_agent` manquants | Adopter | **P1** | `basemyai-mcp` |
| 5 | `recall_keyword` (BM25 seul) + snippets FTS5 | Adopter | **P1** | `basemyai` + surfaces |
| 6 | `MemoryEvent` broadcast (notif MCP, SSE REST, réveil consolidation) | Adapter (révise patterns §2) | **P1** | `basemyai` + surfaces |
| 7 | Scopes partagés inter-agents | Adapter | **V2 — ADR** | `basemyai` |
| 8 | Quantization vecteurs (F16/F8 libSQL) | Adapter | **V2 — ADR** (casse compat `.idx`) | core |
| 9 | Analyzers FTS custom, métriques distance exotiques, change feeds persistés, IAM générique, WASM, buckets, GraphQL | Écarter | — | — |

### Lecture produit

Le « vrai gap pour proposer le produit » n'est pas une feature spectaculaire de
SurrealDB : c'est l'**opérationnel** (1–3 : atomicité, portabilité, boucle
cognitive qui tourne de bout en bout) puis la **surface agent** (4–6 : un LLM
doit pouvoir découvrir, comprendre et suivre la mémoire sans documentation
externe). Les points 7–8 sont la croissance V2. Tout le reste est du
kitchen-sink généraliste qui ne sert pas un moteur mémoire — et notre avantage
(embeddings intégrés, sémantique mémoire livrée, 100 % local) reste entier.

### Prochain pas suggéré

Attaquer **#1** immédiatement (correctness pur, aucun choix produit), puis
**#2**. **#3** attend le choix du provider LLM (décision utilisateur en
suspens). **#4–6** méritent un mini-ADR « surface agent » qui actera resources,
prompts, events et caps d'un bloc.
