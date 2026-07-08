# BaseMyAI — Product Requirements Document

**Version** : 1.0
**Date** : Juin 2026
**Statut** : Draft — En cours de validation

> **Note backend (ADR-032)** : le socle est désormais **100 % moteur natif BaseMyAI** (`basemyai-engine` : WAL + memtable + SST pur Rust, index vectoriel LM-DiskANN/Vamana `F32`, graphe et FTS/BM25 natifs, chiffrement au repos AEAD XChaCha20-Poly1305). libSQL/V1 et la feature `crypto` (CMake) ont été **entièrement retirés** du code actif. Les mentions historiques de libSQL/`vector_top_k`/`crypto` sont conservées uniquement dans les ADR et le CHANGELOG (records immuables). Source de vérité du backend : `ADR.md`, ADR-024→032.

---

## 1. Résumé exécutif

Les agents IA en 2026 sont amnésiques. Chaque session repart de zéro. Le modèle qui vous a aidé hier ne se souvient de rien aujourd'hui. Les rares solutions de mémoire qui existent envoient les conversations et les embeddings vers une base vectorielle cloud — inacceptable pour tout ce qui touche à des données sensibles.

BaseMyAI est un **moteur de mémoire local** pour agents IA. Il fournit une mémoire persistante, temporelle et multi-couches — embeddings, recherche vectorielle, et retrieval conscient du temps — entièrement dans un fichier local. Zéro cloud, zéro fuite.

**La thèse produit** : un développeur doit pouvoir donner à son agent une mémoire infinie, isolée par agent et chiffrée, en deux lignes de code, sans qu'aucune donnée ne quitte la machine.

Architecturalement, BaseMyAI est **deux crates dans un seul workspace Cargo** : `basemyai-core` (socle agnostique métier : moteur de stockage natif `basemyai-engine`, index vectoriel/graphe/FTS natifs, embeddings Candle, chiffrement au repos natif, worker de maintenance) et `basemyai` (la sémantique mémoire posée dessus). Le même core alimente les SDK Python/Node **et** peut être consommé directement par des crates Rust tiers.

---

## 2. Le problème

### 2.1 Problème primaire

Un développeur construit un agent IA (assistant personnel, copilote métier, bot de support). Il veut que l'agent **se souvienne** : des préférences de l'utilisateur, des échanges passés, des procédures apprises, des faits établis.

Aujourd'hui il a le choix entre :
- **Rien** : l'agent est stateless, chaque session repart à zéro.
- **Un store en RAM** : la mémoire disparaît au redémarrage.
- **Une base vectorielle cloud** (Pinecone, Qdrant Cloud) : les données quittent la machine, latence réseau, coût récurrent, dépendance.

Aucune de ces options ne donne, en local et simplement : de la persistance, de la recherche sémantique, **et** une notion du temps.

### 2.2 Problème secondaire — le temps

Une mémoire qui ignore le temps est une mémoire qui ment. « L'utilisateur est sur le plan Free » était vrai au T1 ; il est sur le plan Pro au T2. Un retrieval classique retourne les deux avec la même confiance. L'agent affirme alors des faits périmés.

Les solutions existantes traitent la mémoire comme un sac de vecteurs intemporel. Il manque le **RAG temporel** : ne retrouver que ce qui est à la fois *pertinent* et *encore valide*.

### 2.3 Problème tertiaire — confidentialité et isolation

Deux contraintes que le cloud résout mal :
- **Confidentialité** : les données de mémoire (conversations, profils utilisateurs) sont parmi les plus sensibles d'un produit. Les envoyer à un tiers est souvent un blocage compliance (RGPD, HDS, secret professionnel).
- **Isolation multi-agent** : un service qui héberge plusieurs agents (ou plusieurs utilisateurs) doit garantir qu'un agent ne lit **jamais** la mémoire d'un autre. Une fuite cross-agent est un incident de sécurité.

### 2.4 Ce que les développeurs font aujourd'hui

```
Workarounds actuels                   Coût
──────────────────────────────────    ──────────────────────────────
Réinjecter tout l'historique dans     Coûteux en tokens, fenêtre limitée,
le prompt à chaque tour               pas de recherche

Store vectoriel cloud (Pinecone…)     Données hors machine, latence, $/mois

Bricoler SQLite + un service          Fragile, pas de RAG temporel,
d'embedding séparé                    pas d'isolation, fuites de mémoire ML

Tout garder en RAM                    Perdu au redémarrage
```

---

## 3. Utilisateurs cibles

### Persona 1 — Le développeur solo qui construit un agent IA

**Profil** : Développeur indépendant ou en petite startup. Construit un assistant IA en Python. Veut une mémoire qui marche, localement, sans monter une infra vectorielle.

**Douleur principale** : « Mon agent oublie tout entre les sessions, et je ne vais pas déployer un Qdrant juste pour un side-project. »

**Succès** : « `pip install basemyai`, deux lignes, mon agent a une mémoire persistante et temporelle. »

### Persona 2 — L'utilisateur LangChain / LlamaIndex

**Profil** : Développeur déjà investi dans un framework d'orchestration (LangChain, LlamaIndex). Cherche un VectorStore / un backend mémoire qui s'intègre proprement et tourne en local.

**Douleur principale** : « Les VectorStores intégrés sont soit en RAM (volatiles), soit cloud. Je veux du local, persistant, et conscient du temps, branché sur mon graphe LangChain. »

**Succès** : « `BaseMyAIVectorStore` se branche en une ligne dans ma chaîne, et j'ai du RAG temporel gratuitement. »

### Persona 3 — L'équipe avec contrainte de confidentialité

**Profil** : Équipe produit (santé, finance, juridique, défense) sous contrainte compliance forte. Construit un agent multi-utilisateurs ou multi-tenant. Ne peut envoyer aucune donnée de mémoire dans le cloud.

**Douleur principale** : « Nos données de mémoire sont les plus sensibles du produit. On a besoin de chiffrement au repos et d'une garantie qu'un tenant ne lit jamais la mémoire d'un autre. »

**Succès** : « BaseMyAI nous donne le chiffrement au repos obligatoire (AEAD natif, sans CMake) et l'isolation par `agent_id` structurelle dans le layout de clé. La fuite cross-agent est structurellement impossible. »

---

## 4. Objectifs produit

### V1 — Ce qu'on veut prouver

```
1. Un agent IA peut acquérir une mémoire persistante, temporelle et
   isolée par agent en moins de 10 lignes de code

2. Tout tourne en local : embeddings in-process, vecteurs natifs
   dans le même store `.bmai`, zéro appel réseau par défaut

3. Le RAG temporel retourne uniquement ce qui est pertinent ET valide

4. Aucune mémoire d'un agent ne fuit jamais vers un autre agent

5. L'installation n'exige aucun compilateur chez le client (wheel / prebuild)
```

### Métriques de succès V1

| Métrique | Cible | Comment mesurer |
|---|---|---|
| Latence d'insertion (write + embed) | < 15 ms p50 | Bench automatisé |
| Latence requête RAG temporelle (k=5) | < 25 ms p50 | Bench automatisé |
| Throughput écriture soutenu | ≥ 100 writes/s | Stress-test |
| Throughput RAG soutenu | ≥ 50 RAG/s | Stress-test |
| Fuite cross-agent sur dataset adversarial | 0 | Test adversarial |
| Taille du wheel Python (par plateforme) | < 50 MB | CI packaging |
| Time-to-first-memory (install → 1ʳᵉ insertion) | < 10 min | Test utilisateur |
| Fuite mémoire sous charge 1h (inférence ML) | 0 croissance non bornée | Stress-test + profiling |

---

## 5. Scope V1

### 5.1 Inclus

**`basemyai-core` (socle agnostique)**
- Moteur de stockage **natif** (`basemyai-engine`) : WAL + memtable + SST pur Rust, batches atomiques (`apply_batch`), recovery crash-consistent, mono-écrivain sync (versionnage de wire gouverné par `format.lock`)
- Index vectoriel **natif** LM-DiskANN/Vamana (`F32`, in-store, pas d'extension à linker) : oversampling ×8 en présence d'un filtre (ADR-012), tombstones + réparation, rebuild depuis la donnée
- `Embedder` Candle in-process, **sync** (CPU-bound) : `embed`, `embed_batch`, `model_id()`, `dim()` — modèle `all-MiniLM-L6-v2` (384 dims). **N'auto-télécharge jamais** : reçoit un chemin local.
- Chiffrement au repos **natif** AEAD XChaCha20-Poly1305 (enveloppe DEK/KEK, WAL/SST scellés), clé fournie à l'ouverture, jamais stockée — **aucune feature Cargo, aucun CMake** (ADR-030)
- `MaintenanceWorker` : boucle de fond async, tâches **injectées par le consommateur**

**`basemyai` (sémantique mémoire)**
- Les 4 couches mémoire : `short_term`, `episodic`, `procedural`, `semantic`
- RAG temporel : champs `valid_from` / `valid_until`, requête hybride cosine + filtre temporel
- Isolation multi-agent : scoping obligatoire par `agent_id`, structurel dans le layout de clé du moteur
- Chiffrement au repos **obligatoire** (AEAD natif ADR-030 ; le store exige une clé)
- Active Worker : GC des souvenirs expirés (`valid_until` dépassé), compaction du moteur
- **Setup hardware-aware** (`basemyai setup`) : détection matériel, sélection modèle/device, fetch explicite du modèle (ADR-010)

**Bindings & surfaces**
- SDK Python (PyO3) packagé en wheel — `pip install` sans compilateur
- SDK Node/TS (NAPI-RS) packagé en prebuild — `npm install` sans compilateur
- Sidecar REST (axum) : un seul binaire autonome pour Go, Ruby, autres
- Crate Rust natif : `basemyai` (produit complet) et `basemyai-core` (socle seul)

**Écosystème**
- Connecteurs `BaseMyAIVectorStore` pour LangChain (Python & JS) et LlamaIndex

### 5.2 Explicitement exclus de V1

```
✗ Base vectorielle externe (Qdrant, LanceDB) — vecteurs natifs DANS le store `.bmai`, toujours
✗ Inférence via API cloud (OpenAI embeddings…) — in-process uniquement
✗ Auto-download du modèle par l'Embedder — fetch orchestré par le produit
✗ Sync de mémoire multi-machines / réplication distribuée
✗ Modèles d'embedding multiples / multilingues (V2)
✗ Mémoire partagée volontaire entre agents (le défaut est l'isolation stricte)
✗ Dashboard / GUI
✗ Fine-tuning du modèle d'embedding
```

---

## 6. Exigences fonctionnelles

### 6.1 Socle — `basemyai-core`

**REQ-001** : l'ouverture d'un store natif (`Engine`/`NativeMemoryStore`) doit être crash-consistent (WAL scellé par enregistrement, recovery au démarrage, batches atomiques `apply_batch`). Le moteur est mono-écrivain sync ; les lectures concurrentes sont servies sous verrou de lecture partagé (RwLock, ADR-N5.5).

**REQ-002** : `basemyai-core` ne doit contenir **aucun** concept métier. Un `grep` de `agent_id`, `valid_from`, `valid_until`, `episode`, `Symbol`, `Edge` dans le crate doit retourner zéro (test d'agnosticité).

**REQ-003** : la recherche vectorielle native (LM-DiskANN/Vamana, distance cosine) doit retourner les `k` plus proches voisins en appliquant un filtre **fourni par l'appelant**. Le core ne sait pas ce que le filtre signifie. Le filtre s'appliquant après le top-k, l'index sur-échantillonne (×8) pour garantir `k` résultats filtrés.

**REQ-004** : `Embedder` (trait **sync**, CPU-bound) doit produire des vecteurs de 384 dimensions via Candle (`all-MiniLM-L6-v2`), in-process, sans ONNX. Il reçoit un chemin de modèle local et **ne déclenche jamais** de téléchargement réseau. Le consommateur l'enveloppe dans `spawn_blocking` si besoin depuis un contexte async.

**REQ-005** : le chiffrement au repos est **natif** (AEAD XChaCha20-Poly1305, enveloppe DEK/KEK, ADR-030) : l'ouverture chiffrée accepte une clé, jamais persistée. Rotation de clé O(1) par re-scellement, sans réouverture. **Aucune feature Cargo ni CMake requis.**

**REQ-006** : Le `MaintenanceWorker` exécute des tâches **enregistrées par le consommateur**. Il n'embarque aucune tâche métier en dur.

### 6.2 Sémantique — `basemyai`

**REQ-010** : Les 4 couches mémoire (`short_term`, `episodic`, `procedural`, `semantic`) doivent être persistées avec leur couche et leurs champs `valid_from` / `valid_until` par souvenir, chacune interrogeable indépendamment.

**REQ-011** : Toute écriture et toute lecture doivent être scopées par `agent_id`, structurellement dans le layout de clé du moteur. Une requête sans `agent_id` valide doit échouer, jamais retourner des données d'un autre agent.

**REQ-012** : La requête de recall doit être **hybride** : similarité cosine (index vectoriel natif) **ET** `valid_until > now()` (ou `valid_until IS NULL`). Aucune mémoire expirée ne doit apparaître dans un recall.

**REQ-013** : `basemyai` doit imposer le chiffrement (AEAD natif ADR-030) : instancier une mémoire sur disque sans clé doit échouer.

**REQ-014** : L'Active Worker doit, en tâche de fond : (a) GC ou archiver les souvenirs dont le `valid_until` est expiré, (b) déclencher la compaction du moteur périodiquement. Il ne doit jamais bloquer le chemin critique d'écriture/lecture.

**REQ-015** : Un **setup hardware-aware** (`basemyai setup`, ou déclenché au 1ᵉʳ appel SDK si non configuré) doit, façon AnythingLLM : (a) détecter RAM / GPU / VRAM / device / cœurs / OS ; (b) résoudre le device Candle (CUDA > Metal > CPU) ; (c) sélectionner le modèle (baseline `all-MiniLM-L6-v2` en V1) ; (d) fetch explicite avec vérification de checksum, mis en cache dans `~/.basemyai/models/` ; (e) persister `{ model_id, dim, device }`. **Aucun download silencieux** : si le setup n'a pas été fait, le 1ᵉʳ usage échoue proprement avec un message d'invite, jamais un fetch surprise. La détection et la sélection sont faites par le produit ; `basemyai-core.Embedder` reçoit un chemin + device déjà résolus (ADR-010).

### 6.3 Bindings

**REQ-020** : `pip install basemyai` et `npm install basemyai` doivent fonctionner sur Linux/Windows/macOS **sans** exiger de compilateur C ou Rust chez le client (wheels / prebuilds précompilés).

**REQ-021** : Le SDK Python doit exposer une API idiomatique typée (annotations strictes, compatible MyPy). Le SDK Node doit générer les types `.d.ts`.

**REQ-022** : Le sidecar REST doit se compiler en un seul exécutable autonome et exposer les opérations mémoire en HTTP/JSON.

### 6.4 Sécurité

**REQ-030** : Aucune donnée ne doit quitter la machine par défaut. Le seul accès réseau possible (fetch du modèle) est orchestré explicitement par le produit, jamais par l'`Embedder`.

**REQ-031** : L'isolation cross-agent doit être testée avec un dataset adversarial tentant de contourner le filtre `agent_id`. Zéro fuite tolérée.

**REQ-032** : Le moteur natif est sans surface SQL : les inputs d'agent sont encodés dans des clés/valeurs binaires typées, jamais interpolés dans une requête (aucune surface d'injection SQL).

---

## 7. Exigences non-fonctionnelles

### Performance

| Opération | Cible p50 | Cible p99 |
|---|---|---|
| Insertion (write + embed) | < 15 ms | < 40 ms |
| Recall RAG temporel (k=5) | < 25 ms | < 60 ms |
| Embedding d'un texte court (Candle) | < 8 ms | < 20 ms |
| Throughput écriture soutenu | ≥ 100 writes/s | — |
| Throughput RAG soutenu | ≥ 50 RAG/s | — |

Configuration de référence : 4 cœurs, 8 GB RAM, sans GPU (inférence CPU).

### Fiabilité

- La DB ne doit jamais locker en lecture pendant une écriture (WAL).
- Une inférence ML embarquée sous charge soutenue (1h) ne doit pas fuir de mémoire de façon non bornée.
- Une requête invalide retourne une erreur structurée, jamais un crash du process hôte.
- Corruption détectée à l'ouverture → erreur explicite, pas de lecture silencieuse de données corrompues.

### Compatibilité

- Linux (glibc, x86_64), Windows 10+ (MSVC), macOS 12+ (Intel & ARM)
- Python 3.9+ (wheels), Node 18+ (prebuilds)
- Rust 1.78+
- `basemyai-core` testé Linux **et** Windows (moteur natif pur Rust : aucune dépendance C, aucun CMake, y compris pour le chiffrement)

### Maintenabilité

- Couverture de tests : ≥ 85% sur `basemyai-core`, ≥ 80% sur `basemyai`
- Clippy sans warning sur `--all-targets`
- Chaque décision architecturale documentée dans un ADR
- Semver strict sur `basemyai-core` (les consommateurs tiers pin une version)

---

## 8. Contraintes

**Contrainte d'architecture** : un seul workspace Cargo, **deux crates publiables indépendamment** (`basemyai-core`, `basemyai`). `basemyai-core` ne `use` jamais `basemyai`.

**Contrainte d'agnosticité** : `basemyai-core` est business-agnostic. Les concepts métier (`agent_id`, couches, RAG temporel) vivent exclusivement dans `basemyai`.

**Contrainte vectorielle** : les vecteurs sont stockés **dans** le store `.bmai` via l'index **natif** LM-DiskANN/Vamana (`F32`). Pas d'extension à linker, aucune base vectorielle externe.

**Contrainte backend** : backend unique = **moteur natif BaseMyAI** (`basemyai-engine`, pur Rust, embarqué local, mono-écrivain sync), exposé via le contrat `MemoryStore` **async** ; `Embedder` **sync** (CPU-bound). Pas de fallback libSQL (ADR-032).

**Contrainte ML** : inférence pure Rust via Candle (`all-MiniLM-L6-v2`, 384 dims). Pas d'ONNX, pas de fastembed, pas d'API cloud.

**Contrainte chiffrement** : chiffrement au repos via l'enveloppe AEAD **native** (ADR-030, pur Rust, sans CMake), optionnel au core, **obligatoire** dans `basemyai`.

**Contrainte réseau** : zéro réseau par défaut. L'`Embedder` ne télécharge jamais le modèle lui-même.

**Contrainte licence** : BUSL-1.1 (précédemment MIT). Toutes les dépendances compatibles MIT, Apache 2.0 ou autres licences permissives.

---

## 9. Risques produit

| Risque | Probabilité | Impact | Mitigation |
|---|---|---|---|
| Fuite cross-agent (contournement du scoping `agent_id`) | Faible | Critique | Scoping structurel dans le layout de clé, dataset adversarial en CI, isolation = invariant |
| Bug de durabilité/corruption du moteur natif | Faible | Critique | Harnais crash-consistency (kill réel, 5 modes × 20 cycles, 0 violation), fuzzing, `format.lock` anti-drift, recovery vérifiée |
| Immaturité du moteur natif face à un backend éprouvé | Faible | Moyen | Parité prouvée (19 scénarios `backend_suite!`, recall@10 = 1.0, benchs 3,8×→13,8× vs l'ancien libSQL) avant bascule ADR-032 |
| Fuite mémoire de l'inférence ML embarquée | Moyen | Haut | Stress-test 1h + profiling dès la Phase 5, traque des leaks Candle |
| Wheel/prebuild ne compile pas sur une plateforme | Moyen | Haut | Moteur natif **pur Rust** (aucune dépendance C ni CMake), testé Linux + Windows |
| Perf insuffisante (< 100 writes/s) | Moyen | Moyen | Batches WAL atomiques + compaction ; bench mesuré (~2,9× plus rapide en recall bout-en-bout que l'ancien backend) |
| Intégrité du modèle téléchargé (supply chain) | Faible | Haut | Fetch orchestré par le produit, vérification de checksum, modèle mis en cache local |
| Fuite de la sémantique métier dans `basemyai-core` | Moyen | Moyen | Test d'agnosticité automatisé (grep) en CI |
