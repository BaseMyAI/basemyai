# BaseMyAI Strategy Research — Agent Memory Database

**Date**: 2026-06-18  
**Scope**: recherche produit, benchmark concurrentiel, direction architecture avant refactor.  
**Directive**: BaseMyAI is not a SQLite wrapper. It is an AI-native memory database. SQLite/libSQL is only the first storage backend unless research proves otherwise.

## Executive Summary

BaseMyAI doit rester basé sur SQLite/libSQL en V1, mais ne doit plus se présenter ni se structurer mentalement comme "SQLite + vectors + memory helpers". Le bon positionnement est une **Agent Memory Database locale** : un moteur de mémoire privé, temporel, chiffré, isolé par agent, avec SDK et outils de diagnostic pour développeurs d'agents.

La recherche confirme trois points.

1. Le marché "agent memory" est déjà réel et concurrentiel. Mem0, Zep/Graphiti, Letta, LangMem, LlamaIndex, Cognee, Supermemory et Hindsight vendent tous une forme de mémoire persistante, de graphe temporel, de consolidation ou d'expérience développeur. BaseMyAI ne peut pas gagner en étant "un wrapper SQLite local".
2. Les bases vectorielles restent des briques, pas des produits mémoire. Qdrant, LanceDB, Chroma et Pinecone sont forts sur la recherche vectorielle, le scale et l'écosystème, mais ne résolvent pas nativement la mémoire temporelle d'agent, l'isolation agent, la consolidation et l'oubli.
3. Le vrai espace différenciant est: **local-first + embedded + encrypted + temporal + agent-isolated + debuggable**. Zep est fort sur le graphe temporel, Mem0 sur les intégrations et la simplicité, Letta sur l'agent runtime, Cognee sur le graphe open-source, Supermemory sur MCP et UX utilisateur. BaseMyAI doit être le "DuckDB/SQLite de la mémoire d'agent privée", pas un framework d'agents complet.

Recommandation nette:

- Garder **libSQL/SQLite en backend V1**.
- Exposer le format utilisateur comme **`.bmai` dès V1**, même si le contenu est un fichier libSQL chiffré avec tables de métadonnées BaseMyAI.
- Créer maintenant une abstraction **`StorageEngine` minimale**, orientée opérations mémoire, pas une abstraction SQL générique.
- Préparer un futur backend natif `.bmai` append-only par interfaces, capacités et metadata, mais **ne pas l'implémenter en V1**.
- Reporter Tauri. Livrer d'abord CLI + éventuellement une Web UI locale lancée par `basemyai studio` en V1.5.

## Research Method

Sources internes lues:

- `docs/PRD.md`
- `docs/ADR.md`
- `README.md`
- `CLAUDE.md`
- `../ECOSYSTEM_ARCHITECTURE.md`
- `docs/ARCHITECTURE.md` et `docs/VISION.md` comme contexte complémentaire.

Sources externes consultées: docs et dépôts officiels Mem0, Zep/Graphiti, Letta, LangMem, LlamaIndex, Cognee, Qdrant, LanceDB, Chroma, Pinecone, Turso/libSQL, DuckDB VSS, sqlite-vec, sqlite-vss, Supermemory, Hindsight. Voir "Sources" en fin de document.

Limite: il s'agit d'un benchmark conceptuel et stratégique, pas d'un benchmark de latence exécuté sur une machine cible.

## Market And Competitor Research

### Lecture du marché

Le marché se sépare en quatre familles.

**1. Memory layers pour agents**

Mem0, Zep, LangMem, LlamaIndex Memory, Hindsight, Supermemory et Cognee vendent directement la mémoire d'agent: extraction de faits, personnalisation, contexte long terme, graphe, consolidation, intégrations frameworks, MCP.

**2. Agent runtimes avec mémoire**

Letta/MemGPT est moins une base mémoire qu'un runtime d'agents stateful. Sa proposition n'est pas "stocke mes souvenirs", mais "construis des agents qui gèrent leur contexte et leur mémoire".

**3. Vector databases**

Qdrant, LanceDB, Chroma et Pinecone sont des moteurs de recherche vectorielle. Ils sont utiles comme backend, mais ils ne savent pas, seuls, ce qu'est une mémoire temporelle, un épisode, une préférence, une contradiction ou une isolation d'agent.

**4. Embedded/local vector storage**

libSQL native vector search, sqlite-vec, sqlite-vss et DuckDB VSS prouvent que la recherche vectorielle locale est devenue une capacité d'infrastructure. Cela renforce le choix libSQL en V1, mais rend aussi le positionnement "SQLite vector wrapper" insuffisant.

### Benchmark conceptuel

| Produit | Positionnement | Local-first ou cloud | Mémoire temporelle | Chiffrement | Agent isolation | SDK | Open-source ou paid | Ce que BaseMyAI peut apprendre | Différenciation BaseMyAI |
|---|---|---|---|---|---|---|---|---|---|
| Mem0 | Memory layer universel pour agents et apps | Cloud + OSS/self-host selon usage | Partielle, centrée personnalisation/faits | Dépend du déploiement/cloud | User/session/agent dans le modèle produit | Python, JS, intégrations frameworks | OSS + plateforme paid | Simplicité API, intégrations, marketing "drop-in memory" | Plus privé: fichier local chiffré, pas plateforme par défaut |
| Zep / Graphiti | Temporal knowledge graph pour agent memory | Zep managé; Graphiti self-host | Très forte: graphes avec validité historique | Cloud/infra selon déploiement | Users/threads côté Zep | Python, TS, Go côté Zep | Graphiti OSS + Zep paid | Le graphe temporel est un axe gagnant; provenance importante | Même thèse temporelle, mais embedded mono-fichier et local-first |
| Letta / MemGPT | Runtime d'agents stateful avec mémoire avancée | Local CLI + API/cloud | Oui, via gestion de contexte/mémoire du runtime | Dépend du backend | Agent-centric | Python/TS/API/CLI | OSS + commercial | UX d'observabilité et agent self-editing | Ne pas devenir un runtime; rester une database embeddable |
| LangMem / LangGraph memory | SDK mémoire long terme intégré LangGraph | Mix: storage pluggable + managed | Types mémoire, pas DB temporelle native | Dépend du storage | Dépend du storage | Python, LangGraph-native | OSS + LangSmith/LangGraph paid | Primitives de consolidation et prompt optimization | Être le storage privé spécialisé derrière LangGraph |
| LlamaIndex Memory | Blocks: static, fact extraction, vector memory | Framework local/cloud selon backends | Faible à moyenne; blocks et chat history | Dépend du vector store | Session-centric | Python | OSS + services LlamaCloud | API "memory blocks" compréhensible | Backend mémoire durable et temporel pour ces blocks |
| Cognee | Open-source AI memory platform, graph + vector | Local/self-host + cloud | Évolutive via graphes, pas forcément mono-fichier | Dépend déploiement | Multi-agent selon config | Python, MCP, intégrations | OSS + cloud | Graphe + ontology + MCP sont demandés | Un seul fichier chiffré; moins de poly-store |
| Hindsight | Agent memory that learns: retain/recall/reflect | Self-host + cloud Vectorize | Forte: temporal strategy + observations | Dépend déploiement | Memory banks | API/SDK | OSS + paid/cloud | "learn, not just remember"; retrieval multi-stratégie | Plus bas niveau, embeddable, privacy-by-default |
| Supermemory | Memory/context engine + app + MCP | Cloud API + local one-binary annoncé | Plutôt profil/mémoire persistante que temporal DB | Compte/API; local selon mode | User/project scoping | API, MCP, plugins | OSS components + paid | MCP et UX cross-tool sont essentiels | Cibler développeurs qui veulent posséder la DB locale |
| Qdrant | Vector search engine Rust, cloud/self-host/edge | Cloud, self-host, Qdrant Edge | Non native mémoire; payload filters | Cloud/security self-host | Multitenancy/payloads | Nombreux SDK | OSS + cloud/enterprise | Performance, filtres, edge mode | BaseMyAI n'est pas en concurrence frontale: il peut utiliser les leçons de DX/perf |
| LanceDB | Embedded multimodal vector DB/lakehouse | OSS embedded + enterprise | Non native agent memory | Enterprise/security selon offre | Tables/namespaces | Python, TS, Rust | OSS + enterprise | Excellente histoire embedded -> cloud | BaseMyAI est mémoire d'agent, pas lakehouse multimodal |
| Chroma | Search infra AI: vector, full-text, metadata | Local + Chroma Cloud | Non native agent memory | Cloud SOC2; local dépend env | Multi-tenant indexes côté cloud | Python, TS, Rust | Apache 2 + cloud | Developer happiness, CLI, local quickstart | Temporal/encrypted/agent-isolated by design |
| Pinecone | Fully managed vector DB production | Cloud | Non native agent memory | Cloud enterprise controls | Namespaces/metadata | SDKs | Paid/cloud | Scale, managed reliability, enterprise security | BaseMyAI évite le cloud et le coût infra pour mémoire sensible |
| sqlite-vec | Extension SQLite vector search | Local embedded | Non | Non intégré | Non | Multi-langages | OSS | Local vector search portable | Remplacé en V1 par libSQL native vectors; utile comme benchmark fallback |
| sqlite-vss | SQLite vector search via Faiss | Local embedded | Non | Non intégré | Non | SQLite extension | OSS, plus en active dev | Preuve historique du besoin local | Ne pas dépendre d'un projet moins actif |
| DuckDB VSS | Extension vectorielle HNSW pour DuckDB | Local embedded analytique | Non | Non core memory | Non | SQL/Python/etc. | OSS | Excellent pour analytics/telemetry offline | Pas OLTP/agent memory; possible outil d'analyse, pas backend V1 |
| libSQL/Turso | SQLite-compatible avec vecteurs natifs | Embedded/local + Turso Cloud | Non native, mais permet de la bâtir | Crypto feature/Turso controls | À modéliser | Rust/TS/etc. | OSS + cloud | Choix V1 cohérent: vecteur natif sans extension | BaseMyAI ajoute sémantique mémoire, sécurité agent et DX |

## Product Positioning Recommendation

### Positionnement final

**BaseMyAI is the private, embedded memory database for AI agents.**

En français produit: **la base mémoire privée et locale pour agents IA**.

BaseMyAI ne doit pas être vendu comme:

- un vector store;
- un framework d'agents;
- un wrapper SQLite;
- une alternative directe à Pinecone/Qdrant;
- un système de notes personnelles.

BaseMyAI doit être vendu comme:

- une base de données spécialisée mémoire d'agent;
- locale et chiffrée par défaut;
- temporelle par construction;
- agent-isolated par invariant;
- embeddable dans Python, Node, Rust, REST/MCP;
- inspectable par CLI puis Studio.

### Tagline

**The private memory database for AI agents.**

Alternative plus technique:

**Temporal, encrypted, local-first memory for AI agents.**

### Promesse courte

Donner à un agent une mémoire persistante, temporelle et privée dans un seul fichier local, sans cloud et sans base vectorielle externe.

### Personas prioritaires

1. **Builder Python/TypeScript d'agents locaux**  
   Veut `pip install basemyai` / `npm install basemyai`, `remember`, `recall`, zéro infra.

2. **Développeur de coding agents / MCP tools**  
   Veut une mémoire cross-session locale pour Claude Code, Cursor, Codex, ForgeMyAI, outils internes.

3. **Équipe privacy/compliance**  
   Ne peut pas envoyer mémoire utilisateur, conversations, préférences ou embeddings vers un service tiers.

4. **Développeur Rust/local-first**  
   Veut un composant embeddable, déterministe, testable, sans service à déployer.

### Cas d'usage V1

- Assistant personnel local avec préférences et faits utilisateur.
- Coding agent qui se souvient des règles projet et décisions précédentes.
- Agent support interne qui garde une mémoire par utilisateur/tenant.
- Prototype LangGraph/LlamaIndex qui veut une mémoire persistante locale.
- MCP memory server local pour outils de dev.

### Cas d'usage à éviter au début

- Billion-scale vector search.
- Mémoire partagée temps réel multi-device.
- Knowledge graph enterprise multi-source à la Zep.
- Full agent runtime à la Letta.
- Data lake multimodal à la LanceDB.
- UI consommateur grand public.
- Cloud-hosted managed service avant que le noyau local soit crédible.

## Key Strategic Answers

### BaseMyAI doit-il rester basé sur SQLite/libSQL en V1 ?

Oui. C'est le bon choix V1.

Raisons:

- Le besoin V1 est embedded, local, transactionnel, mono-fichier.
- libSQL fournit déjà le vecteur natif via `vector_top_k` et index vectoriel, sans extension externe.
- SQLite/libSQL donne WAL, ACID, portabilité et outillage.
- Un backend natif `.bmai` maintenant détournerait l'énergie du vrai risque: prouver la mémoire d'agent et la DX.

Critique: le PRD doit cesser de parler comme si le backend était le produit. Le backend V1 est un détail d'implémentation.

### Faut-il exposer un format `.bmai` même si SQLite/libSQL est utilisé en interne ?

Oui.

Le fichier utilisateur doit être `memory.bmai`, pas `memory.db`. En V1, `.bmai` peut être un fichier libSQL chiffré avec:

- table `bmai_meta`;
- `format_version`;
- `storage_engine = "libsql"`;
- `schema_version`;
- `embedding_model_id`;
- `embedding_dim`;
- `created_by`;
- options de sécurité et capacités.

Il ne faut pas mentir: la documentation peut dire "V1 `.bmai` uses an encrypted libSQL-compatible container internally". Mais l'API publique et les garanties doivent être BaseMyAI, pas SQLite.

Point technique: on ne peut pas changer le magic header SQLite sans casser l'ouverture SQLite. Le branding format doit donc être extension + metadata + toolchain, pas header custom en V1.

### Faut-il créer une abstraction `StorageEngine` maintenant ?

Oui, mais minimale.

Ne pas créer une abstraction SQL générique. Créer un trait d'opérations mémoire:

- `open`;
- `migrate`;
- `put_memory`;
- `get_memory`;
- `recall_vector`;
- `recall_text`;
- `recall_hybrid`;
- `invalidate`;
- `delete`;
- `agent_stats`;
- `graph_upsert_entity`;
- `graph_upsert_edge`;
- `graph_traverse`;
- `maintenance`.

Le point clé: les filtres publics ne doivent plus être des fragments SQL. `Filter { where_sql, params }` est acceptable à l'intérieur du backend, mais pas comme contrat de niveau memory database.

### Faut-il préparer un futur backend natif sans l'implémenter ?

Oui.

Préparer par:

- `StorageEngine` orienté mémoire;
- `EngineCapabilities`;
- `BmaiFormatVersion`;
- tests de contrat communs;
- documentation "native backend candidate";
- séparation claire entre modèle mémoire et backend libSQL.

Ne pas faire:

- append-only custom engine;
- moteur d'index vectoriel maison;
- chiffrement de pages maison;
- WAL custom;
- compacteur custom.

Ces chantiers tueraient la V1.

### Quels sont les vrais différenciateurs ?

1. **Private by default**: local, chiffré, zéro réseau implicite.
2. **Temporal by construction**: validité de faits, pas simple timestamp décoratif.
3. **Agent isolation as a security invariant**: pas une option.
4. **Embedded database, not hosted service**: un fichier, pas un cluster.
5. **Memory semantics above vector search**: couches, consolidation, graphe, oubli.
6. **Developer tools**: CLI, tests d'isolation, recall debugger, export/import.
7. **Rust core + Python/Node DX**: fiable côté noyau, simple côté adoption.

### Quelles fonctionnalités sont indispensables en V1 ?

- `.bmai` file path.
- LibSQL backend chiffré.
- `StorageEngine` minimal et backend libSQL.
- `remember`, `recall`, `invalidate`, `forget`, `stats`.
- Couches `short_term`, `episodic`, `semantic`; `procedural` peut être présent mais peu sophistiqué.
- `valid_from` / `valid_until`.
- Isolation `agent_id` obligatoire.
- Embedding local explicite, pas auto-download silencieux.
- CLI: `init`, `inspect`, `stats`, `recall`, `verify`, `migrate`.
- Tests adversariaux d'isolation et tests injection SQL.
- SDK Python et Node simples, pas feature-complete.
- MCP ou REST: choisir un seul canal prioritaire si la vélocité baisse. Pour le marché 2026, MCP est probablement plus stratégique que REST.

### Quelles fonctionnalités doivent attendre V1.5/V2 ?

V1.5:

- Studio local web.
- Recall Lab.
- timeline mémoire.
- connecteurs LangGraph/LlamaIndex.
- observabilité et diagnostics.
- export JSONL.
- hybrid full-text BM25 + vector si pas déjà stable.

V2:

- backend natif `.bmai`.
- sync multi-device.
- mémoire partagée volontaire inter-agents.
- graphe temporel avancé comparable Graphiti.
- conflict resolver sophistiqué.
- multiple embedding models.
- cloud managed service.
- enterprise admin.

### Est-ce qu'une UI Studio/Tauri est utile maintenant ?

Utile, mais pas maintenant en Tauri.

Une UI de debug mémoire est un différenciateur réel, car la mémoire d'agent échoue souvent de manière invisible: mauvais souvenir, souvenir périmé, fuite inter-agent, contradiction, consolidation erronée. Mais un packaging desktop complet ajoute trop de surface.

Recommandation:

- V1: CLI.
- V1.5: `basemyai studio` lance une Web UI locale sur `localhost`, read-only au début.
- V2: Tauri seulement si usage fréquent hors dev server ou si distribution grand public.

### Quel business model est cohérent ?

Open-core.

- Core local memory DB: open-source permissif ou source-available très clair. MIT actuel est cohérent pour adoption développeur.
- Paid plus tard: Studio Pro, cloud sync optionnel, team policy, audit logs, enterprise compliance, managed relay, support, connectors entreprise.
- Ne pas monétiser le chiffrement local ou l'isolation agent: ce sont les piliers du produit, pas des paywalls.

## Architecture Recommendation

### Évaluation de l'architecture cible proposée

Architecture cible:

- `basemyai-core`: types/traits mémoire, no SQLite dependency.
- `basemyai-storage-sqlite`: backend V1 SQLite/sqlcipher/sqlite-vec.
- `basemyai`: API haut niveau produit.
- `basemyai-cli`: CLI développeur.
- futur `basemyai-storage-native`: backend `.bmai` append-only, pas implémenté maintenant.

Avis: la direction est bonne, mais il faut corriger deux points.

1. Le backend V1 ne devrait pas être nommé `sqlite` si la décision ADR-011 reste libSQL native vector. Préférer `basemyai-storage-libsql` ou `basemyai-storage-sqlite` avec feature `libsql`; mais le nom `sqlite-vec` réintroduit une décision supplantée.
2. `basemyai-core` comme "types/traits mémoire" contredit l'ancien invariant "core agnostique métier". Cette contradiction est normale si le produit devient une Agent Memory Database. Il faut choisir.

### Recommandation de découpage

Découpage recommandé à terme:

```text
basemyai-core
  Types mémoire publics, traits, erreurs, contrat StorageEngine,
  pas de libSQL, pas de SQL, pas de Candle concret.

basemyai-storage-libsql
  Implémentation V1 libSQL: schema, migrations, vector_top_k,
  crypto, FTS/BM25, CTE graphe, maintenance SQL.

basemyai-embed-candle
  Optionnel si Candle rend les builds trop lourds.
  Sinon garder sous feature dans basemyai.

basemyai
  API haut niveau: Memory, remember, recall, consolidate,
  choix engine par défaut, re-exports ergonomiques.

basemyai-cli
  init, setup, inspect, recall, verify, migrate, studio.

basemyai-mcp
  Serveur MCP, si priorisé pour agents dev.

future: basemyai-storage-native
  Append-only / log-structured .bmai, non implémenté.
```

### Est-ce trop complexe ?

En une seule fois, oui. Comme cible, non.

Le refactor doit être en deux passes:

**Pass 1: abstraction sans explosion**

- Garder workspace simple.
- Introduire `StorageEngine` dans `basemyai-core`.
- Déplacer seulement les types SQL-leaky derrière le backend.
- Le backend libSQL peut rester dans un module/crate temporaire selon coût.

**Pass 2: crates séparées**

- Extraire `basemyai-storage-libsql`.
- Extraire CLI.
- Ajouter tests de contrat par backend.

Ne pas créer `basemyai-storage-native` maintenant. Créer uniquement un document et un trait suffisamment propre.

### Qu'est-ce qu'il faut simplifier ?

- Réduire la V1 à une boucle magique: `open -> remember -> recall -> inspect`.
- Ne pas livrer Python + Node + REST + MCP + Studio en même temps si l'équipe est petite.
- Ne pas faire de backend natif.
- Ne pas faire de sync.
- Ne pas faire de graph UI avancée.
- Ne pas multiplier les modèles embedding.

### Qu'est-ce qu'il faut absolument garder ?

- Chiffrement obligatoire dans le produit mémoire.
- Isolation agent obligatoire.
- Temporalité native.
- Zéro réseau implicite.
- Fichier unique.
- Tests adversariaux d'isolation.
- Semver et contrat backend.
- Une expérience install simple.

### Modules/crates à créer maintenant

Priorité haute:

- `basemyai-core::engine` ou crate `basemyai-core` refondé autour de `StorageEngine`.
- `basemyai-storage-libsql` ou module équivalent si extraction en crate trop coûteuse.
- `basemyai-cli`.
- `docs/format/bmai-v1.md`.
- `tests/storage_contract.rs`.

Priorité conditionnelle:

- `basemyai-mcp` si la stratégie est d'abord coding agents.
- `basemyai-embed-candle` si Candle complique trop bindings/wheels.

À repousser:

- `basemyai-storage-native`.
- Tauri.
- Cloud sync.
- Enterprise admin.
- Multi-backend officiel hors libSQL.

## SQLite vs Native `.bmai` Decision

Décision recommandée:

```text
V1: .bmai = fichier libSQL chiffré + metadata BaseMyAI + schéma mémoire.
V1.5: contrat StorageEngine stabilisé + tests de conformité.
V2: spike backend natif append-only si et seulement si libSQL bloque une exigence réelle.
```

Pourquoi ne pas faire natif maintenant:

- Les parties difficiles d'une DB ne sont pas le format de fichier, mais transactions, crash recovery, compaction, index, chiffrement, migrations, corruption handling, concurrent reads/writes.
- SQLite/libSQL résout déjà ces problèmes.
- Le marché ne récompensera pas un format natif invisible si l'API mémoire n'est pas excellente.
- Le vrai risque est product-market fit, pas "est-ce que les pages B-tree sont à nous".

Quand envisager un backend natif:

- libSQL native vectors ne suffit pas pour rappel multi-signal.
- Le chiffrement libSQL complique trop packaging/perf.
- Besoin d'append-only audit log/provenance impossible proprement.
- Besoin de sync CRDT/log shipping incompatible avec libSQL.
- Besoin de format partiellement lisible/streamable par agents.

Même dans ce cas, commencer par un prototype séparé et un benchmark.

## Product Roadmap

### V1 — Private Temporal Memory DB

Objectif: un développeur donne une mémoire locale, chiffrée et temporelle à son agent en moins de 10 lignes.

Contenu:

- `.bmai` libSQL-backed.
- API `Memory`.
- `remember`, `recall`, `invalidate`, `forget`, `stats`.
- `agent_id` obligatoire.
- `valid_from`, `valid_until`.
- embeddings locaux provisionnés explicitement.
- CLI minimal.
- SDK Python ou Node prioritaire, puis le second.
- tests sécurité/injection/isolation.
- documentation "not a vector DB".

### V1.5 — Debuggable Memory

Objectif: rendre la mémoire inspectable et fiable pour les développeurs.

Contenu:

- `basemyai studio` web local.
- Recall Lab.
- memory timeline.
- stats/diagnostics.
- export/import.
- connecteurs LangGraph/LlamaIndex.
- MCP server si non livré en V1.
- hybrid BM25/vector stable.
- graphe simple inspectable si déjà présent.

### V2 — Cognitive Memory Database

Objectif: se rapprocher de Zep/Graphiti/Hindsight côté qualité mémoire, mais en local-first embedded.

Contenu:

- graphe temporel robuste.
- consolidation épisode -> faits avec provenance.
- conflict resolver.
- oubli adaptatif avancé.
- multi-signal explainable recall.
- sync optionnelle.
- backend native `.bmai` seulement si justifié.
- Studio Tauri ou cloud optional.

## UI/Studio Recommendation

### CLI seulement en V1 ?

Oui. V1 doit avoir un CLI solide.

Commandes utiles:

```bash
basemyai init ./agent.bmai
basemyai setup
basemyai inspect ./agent.bmai
basemyai stats ./agent.bmai --agent assistant-42
basemyai recall ./agent.bmai --agent assistant-42 "billing plan"
basemyai verify ./agent.bmai
basemyai migrate ./agent.bmai
```

### Web UI locale via `basemyai studio` ?

Oui en V1.5. C'est le bon compromis.

Commencer read-only:

- liste agents;
- stats par agent;
- recall lab;
- timeline;
- records expirés;
- détails de score;
- vérification isolation.

Puis write-capable:

- invalidate;
- edit importance;
- merge duplicates;
- resolve conflicts.

### Tauri maintenant ou plus tard ?

Plus tard. Tauri est trop tôt.

Tauri devient pertinent si:

- Studio est utilisé souvent par des non-développeurs;
- distribution desktop est un canal de croissance;
- il faut une app tray/background memory server;
- le produit vise personal AI memory grand public.

### Vues vraiment utiles

**Recall Lab**  
Entrer une requête, voir les souvenirs retournés, filtres temporels, agent scope, score vectoriel, score lexical, graphe, RRF. C'est la vue la plus importante.

**Memory Timeline**  
Voir faits valides, expirés, remplacés, contradictions. Crucial pour la thèse temporelle.

**Agent Isolation Viewer**  
Lister agents/tenants et vérifier qu'une requête ne traverse jamais le scope. Utile pour confiance et conformité.

**Stats/Diagnostics**  
Nombre de records par couche, taille fichier, index, derniers GC, latences, modèle embedding, schema version.

**Conflict Resolver**  
Très utile, mais V2. Il suppose consolidation/provenance mature.

## Business Model Recommendation

Modèle recommandé: **open-core developer infrastructure**.

Open-source:

- core memory API;
- backend local libSQL;
- CLI de base;
- MCP de base;
- SDKs;
- format `.bmai`;
- tests de sécurité.

Paid plus tard:

- Studio Pro;
- team sync;
- managed encrypted backup;
- enterprise policy packs;
- audit/compliance reports;
- hosted control plane optionnel;
- support et SLA;
- connectors enterprise.

À ne pas paywaller:

- chiffrement local;
- isolation agent;
- format `.bmai`;
- recall de base;
- CLI inspect/verify.

Raison: ces éléments sont la promesse centrale de confiance. Les rendre payants affaiblirait le positionnement.

## PRD/ADR Review

### Bons choix actuels

- Deux niveaux actuels core/produit: la discipline "mécanisme vs sens" était saine.
- libSQL native vectors: meilleur choix que sqlite-vec en V1 si le build est stable.
- Temporalité `valid_from` / `valid_until`: vrai différenciateur.
- Isolation `agent_id` au niveau requête: indispensable.
- Zéro téléchargement silencieux: excellent pour privacy et confiance.
- Candle local embeddings: cohérent avec local-first.
- RRF/hybrid retrieval: direction correcte.
- ADR explicites: bon niveau de rigueur.

### Choix faibles ou à revoir

- Le nom `basemyai-core` est ambigu: aujourd'hui il signifie "generic storage/vector core"; demain il devrait peut-être signifier "memory database core".
- `Filter` SQL paramétré est bon en interne, mais trop leaky si exposé comme abstraction long terme.
- Le PRD promet trop de surfaces V1: Python, Node, REST, Rust, connecteurs, setup, sidecar, cognition. Réduire.
- Les docs internes se contredisent sur le statut: certaines sections disent Phase 2 implémentée, d'autres roadmap. Avant refactor, créer une source de vérité "Implemented / Planned".
- `basemyai` qui impose crypto est bon pour produit, mais risque packaging. Il faut un mode test/dev clair sans affaiblir le message production.
- Trop d'éléments LLM/consolidation dans V1 peuvent détourner du noyau memory DB.

### Décision stratégique à formaliser

Créer un nouvel ADR:

**ADR-019 — BaseMyAI as an Agent Memory Database, with libSQL-backed `.bmai` V1 and backend abstraction.**

Cet ADR doit acter:

- `.bmai` comme format utilisateur;
- libSQL comme backend V1;
- `StorageEngine` minimal;
- séparation backend/product;
- backend natif repoussé;
- Studio repoussé V1.5.

## Risks And Mitigations

| Risque | Impact | Probabilité | Mitigation |
|---|---:|---:|---|
| Être perçu comme wrapper SQLite/vector store | Haut | Moyen | `.bmai`, docs product, API memory-first, cacher SQL |
| Sur-ingénierie avant adoption | Haut | Haut | V1 resserrée, pas de backend natif, pas Tauri |
| Packaging libSQL crypto/Candle difficile | Haut | Moyen | CI multi-OS tôt, features claires, SDK prioritaire |
| Fuite cross-agent | Critique | Faible à moyenne | Tests adversariaux, engine contract, pas de requête sans agent |
| SQL injection via filtres | Critique | Moyen | Filtres typés publics, SQL seulement backend |
| Qualité d'embedding faible | Moyen | Moyen | Documenter baseline, permettre reindex futur, pas multi-modèle V1 |
| Temporal recall incorrect | Haut | Moyen | Tests de remplacement de faits, horloge injectable |
| Consolidation hallucine des faits | Haut | Moyen | V1 peut fonctionner sans consolidation auto; provenance en V2 |
| Docs promettent plus que le produit | Haut | Haut | Matrice Implemented/Planned, README réaliste |
| Dépendance libSQL devient bloquante | Moyen | Faible à moyenne | `StorageEngine`, tests de contrat, spike natif V2 |
| Studio consomme trop de temps | Moyen | Moyen | CLI V1, Studio local read-only V1.5 |

## Final Recommendation

La bonne direction est:

**BaseMyAI = Agent Memory Database privée, locale et temporelle.**

Pas:

- "SQLite wrapper";
- "yet another vector DB";
- "agent framework";
- "hosted memory SaaS first".

Décision finale:

1. Garder libSQL V1.
2. Exposer `.bmai` V1.
3. Créer `StorageEngine` maintenant.
4. Déplacer progressivement SQL/libSQL derrière `basemyai-storage-libsql`.
5. Réduire la V1 à memory DB + CLI + un SDK prioritaire.
6. Reporter backend natif, Tauri, sync, enterprise.
7. Créer Studio local en V1.5 seulement après CLI et recall fiable.

## Concrete Next Steps Before Refactor

1. Écrire ADR-019 avec les décisions ci-dessus.
2. Écrire `docs/format/bmai-v1.md`.
3. Faire une matrice `docs/status.md`: implemented, partial, planned, deferred.
4. Définir le trait `StorageEngine` sur papier avant code.
5. Définir `EngineCapabilities`.
6. Définir les tests de contrat backend:
   - isolation agent;
   - temporal validity;
   - invalidation;
   - recall k;
   - SQL injection impossible;
   - migration idempotente;
   - encryption required product-level.
7. Choisir le canal V1 prioritaire:
   - Python SDK si cible agent builders;
   - MCP si cible coding agents;
   - Node ensuite.
8. Réduire README/PRD pour ne pas sur-promettre la V1.
9. Bench rapide libSQL:
   - 10k, 100k, 1M records;
   - encrypted vs unencrypted;
   - vector recall filtered by agent/time;
   - write throughput with embeddings mocked.

## Recommended Refactor Prompt

```text
Agis comme un staff engineer Rust.

Objectif: refactorer BaseMyAI vers une Agent Memory Database avec backend libSQL V1 caché derrière une abstraction, sans changer le comportement public inutilement.

Contraintes:
- Lire AGENTS.md, docs/ADR.md, docs/PRD.md, docs/strategy/2026-06-18-agent-memory-database-research.md.
- Ne pas implémenter de backend natif.
- Ne pas ajouter Tauri/Studio.
- Ne pas casser les invariants: chiffrement obligatoire dans basemyai, agent_id obligatoire, temporalité native, zéro réseau implicite, inputs SQL paramétrés.
- Garder libSQL comme backend V1.
- Introduire un trait StorageEngine orienté opérations mémoire, pas SQL générique.
- SQL/libSQL doit être confiné au backend.
- Ajouter tests de contrat backend pour isolation, temporalité, invalidation et injection.
- Créer/mettre à jour ADR-019 et docs/format/bmai-v1.md si absents.

Livrable:
1. Plan de refactor incrémental.
2. Première passe de code minimale.
3. Tests ciblés.
4. Résumé des changements et risques restants.
```

## Sources

- Mem0 GitHub: <https://github.com/mem0ai/mem0>
- Zep/Graphiti GitHub: <https://github.com/getzep/graphiti>
- Zep paper: <https://arxiv.org/abs/2501.13956>
- Letta GitHub: <https://github.com/letta-ai/letta>
- LangMem docs: <https://langchain-ai.github.io/langmem/>
- LangChain memory concepts: <https://docs.langchain.com/oss/python/concepts/memory>
- LlamaIndex Memory docs: <https://developers.llamaindex.ai/python/framework/module_guides/deploying/agents/memory/>
- Cognee GitHub: <https://github.com/topoteretes/cognee>
- Qdrant docs: <https://qdrant.tech/documentation/>
- LanceDB docs: <https://docs.lancedb.com/>
- Chroma: <https://www.trychroma.com/>
- Pinecone docs: <https://docs.pinecone.io/guides/get-started/overview>
- Turso/libSQL AI & embeddings: <https://docs.turso.tech/features/ai-and-embeddings>
- DuckDB VSS: <https://duckdb.org/docs/current/core_extensions/vss.html>
- sqlite-vec GitHub: <https://github.com/asg017/sqlite-vec>
- sqlite-vss Datasette plugin docs: <https://datasette.io/plugins/sqlite-vss>
- Supermemory GitHub: <https://github.com/supermemoryai/supermemory>
- Supermemory MCP docs: <https://supermemory.ai/docs/supermemory-mcp/introduction>
- Hindsight docs: <https://hindsight.vectorize.io/>
