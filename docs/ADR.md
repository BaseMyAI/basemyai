# Architecture Decision Records — BaseMyAI

Un ADR documente une décision architecturale importante : pourquoi elle a été prise, quelles alternatives ont été rejetées, et quelles en sont les conséquences. Un ADR ne se modifie jamais. Si une décision change, un nouvel ADR est créé.

---

| # | Décision | Statut |
|---|---|---|
| [ADR-001](#adr-001) | Découpage en deux crates `basemyai-core` / `basemyai` | ✅ Accepted |
| [ADR-002](#adr-002) | sqlite-vec — vecteurs dans SQLite | 🔵 Superseded by ADR-011 |
| [ADR-003](#adr-003) | Candle pour l'inférence in-process | ✅ Accepted |
| [ADR-004](#adr-004) | Les 4 couches mémoire | ✅ Accepted |
| [ADR-005](#adr-005) | RAG temporel — `valid_from` / `valid_until` | ✅ Accepted |
| [ADR-006](#adr-006) | Isolation multi-agent par `agent_id` | ✅ Accepted |
| [ADR-007](#adr-007) | Chiffrement au repos — sqlcipher | ✅ Accepted |
| [ADR-008](#adr-008) | Active Worker — thread de fond | ✅ Accepted |
| [ADR-009](#adr-009) | Trois surfaces de binding + wheels précompilés | ✅ Accepted |
| [ADR-010](#adr-010) | Provisioning du modèle hardware-aware (setup intelligent) | ✅ Accepted |
| [ADR-011](#adr-011) | Pivot vers libSQL (vecteur natif + chiffrement), traits async | ✅ Accepted |
| [ADR-012](#adr-012) | Phase 2 Cognition — Graphe, RRF, Oubli adaptatif, Consolidation | ✅ Accepted |
| [ADR-013](#adr-013) | Inférence LLM model-agnostic + provisioning hardware-aware | ✅ Accepted |
| [ADR-014](#adr-014) | Recherche hybride : full-text BM25 (FTS5) fusionné au vecteur par RRF | ✅ Accepted |
| [ADR-015](#adr-015) | Métriques de distance additionnelles : euclidienne & hamming par re-classement | ✅ Accepted |
| [ADR-016](#adr-016) | AnythingLLM comme backend LLM de premier rang via API workspace-chat | ✅ Accepted |
| [ADR-017](#adr-017) | Consolidation par sampling MCP (emprunter le LLM du client) + politique des modes LLM | ✅ Accepted |
| [ADR-019](ADR-019-agent-memory-database-format-and-engine.md) | Agent Memory Database, format `.bmai` V1 et frontière StorageEngine | ✅ Accepted |
| [ADR-024](ADR-024-native-engine.md) | Moteur natif BaseMyAI (stockage/vecteur/graphe/langage maison) — remplace le chemin Turso DB | ✅ Accepted |
| [ADR-025](ADR-025-native-engine-storage-foundation.md) | Fondation Couche 1 du moteur natif : LSM-tree maison (clôture spike N1) | ✅ Accepted |

---

## ADR-001

### Découpage en deux crates `basemyai-core` / `basemyai`

**Statut** : ✅ Accepted | **Date** : 2026-06

**Contexte**

BaseMyAI doit être consommé par des publics incompatibles. Les builders d'agents Python et JS veulent une API mémoire de haut niveau (« remember », « recall », RAG temporel). Mais le même cœur — pool SQLite durci, sqlite-vec, embeddings, chiffrement, worker — est exactement ce dont un consommateur **Rust** a besoin **sans** la sémantique mémoire. En particulier, ForgeMyAI (moteur de contexte de code) veut le socle vectoriel, mais ses concepts sont `Symbol`/`Edge`, pas `agent_id`/`valid_until`.

Si on empaquette tout dans un seul crate, le consommateur Rust hérite de concepts métier qui n'ont aucun sens pour lui (RAG temporel, isolation par agent, GC par date). Et PyO3 comme NAPI-RS doivent de toute façon wrapper le **même** cœur — la séparation core/sémantique est due quoi qu'il arrive.

**Décision**

Un seul workspace Cargo, **deux crates publiables indépendamment** :

```
basemyai-core   socle AGNOSTIQUE métier
                Store durci · sqlite-vec · Candle · sqlcipher · MaintenanceWorker
                ne connaît JAMAIS : agent_id, valid_from/valid_until, les 4 couches,
                ni Symbol/Edge

basemyai        sémantique mémoire, posée SUR basemyai-core
                4 couches · RAG temporel · isolation agent_id · chiffrement obligatoire
                + bindings Python / Node / REST
```

Règle de dépendance : les flèches pointent toujours **vers `basemyai-core`**. Il ne `use` jamais `basemyai` ni `forge-*`.

**Le pattern clé : mécanisme au core, sens au consommateur.** `basemyai-core.knn(q, k, filtre?)` applique un filtre SQL fourni par l'appelant ; le `MaintenanceWorker` exécute des tâches injectées par le produit. Le core fournit le mécanisme générique ; le consommateur fournit le sens.

Les 4 surfaces de consommation :

| Surface | Consommateur | Couche |
|---|---|---|
| SDK Python (PyO3) | builders Python | `basemyai` |
| SDK Node (NAPI-RS) | builders JS/TS | `basemyai` |
| Sidecar REST | Go, Ruby, autres | `basemyai` |
| Crate Rust natif | **ForgeMyAI** | **`basemyai-core`** |

**Conséquences**

✅ ForgeMyAI consomme `basemyai-core` comme crate Rust natif — pas de FFI, pas de HTTP, pas d'overhead.
✅ `basemyai-core` reste testable et réutilisable sans rien savoir du métier mémoire.
✅ PyO3 et NAPI wrappent le même cœur ; la dette de séparation est payée une fois.
✅ Histoire d'écosystème : *« ForgeMyAI, powered by BaseMyAI »*.
⚠️ Deux crates à versionner et publier ; discipline semver stricte exigée.
⚠️ Tentation de faire fuiter un concept métier dans le core — contrée par un test d'agnosticité (grep des termes interdits en CI).

**Alternatives rejetées**

Crate unique tout-en-un — fait hériter au consommateur Rust (ForgeMyAI) le RAG temporel et l'`agent_id`, qui n'ont aucun sens pour du code.

Troisième crate neutre `coremyai` possédé par personne — un repo de plus à nommer, versionner, documenter, sans bénéfice en contexte solo. Le socle a un propriétaire naturel : BaseMyAI.

Repos séparés — friction inutile ; un seul workspace Cargo suffit, avec deux crates publiables.

---

## ADR-002

### sqlite-vec — vecteurs dans SQLite

**Statut** : 🔵 Superseded by ADR-011 | **Date** : 2026-06

> Remplacé par ADR-011 : libSQL fournit le vecteur **natif** (pas d'extension à
> linker). L'intention — vecteurs dans le même fichier, pas de DB externe — tient.

**Contexte**

Le RAG exige une recherche vectorielle (KNN par similarité cosine). L'approche standard de l'industrie est une base vectorielle dédiée (Qdrant, LanceDB, Pinecone). Mais BaseMyAI est *privacy-first, 100% local, mono-fichier*. Ajouter une base vectorielle externe signifie : deux systèmes à déployer, deux stores à synchroniser, deux fichiers (ou un service réseau) — et une violation directe du principe mono-fichier local.

**Décision**

Stocker les vecteurs **dans** SQLite via l'extension `sqlite-vec`. Une table virtuelle porte les embeddings ; le KNN s'exécute en SQL, dans le même fichier que le reste de la mémoire.

```
VectorIndex (basemyai-core)
  upsert(id, &[f32])
  knn(query, k, filtre SQL optionnel) -> Vec<(id, distance)>
```

Une requête de recall combine, en une seule requête SQL, la similarité cosine sqlite-vec **et** un filtre fourni par l'appelant (cf. RAG temporel, ADR-005 ; isolation, ADR-006).

**Conséquences**

✅ Mono-fichier conservé : un seul `.db` contient données + vecteurs.
✅ Pas de second système à déployer/synchroniser.
✅ Transactions ACID couvrant données ET vecteurs ensemble.
✅ Le filtre SQL permet de fusionner KNN + temps + agent en une requête.
⚠️ `sqlite-vec` est une dépendance C (extension à compiler/lier) — à tester Linux + Windows dès le 1ᵉʳ commit.
⚠️ Compatibilité de build sqlite-vec + sqlcipher à valider (cf. ADR-007).
⚠️ Pas d'index ANN sophistiqué (HNSW distribué) — acceptable à l'échelle visée (mémoire d'agent local, pas milliards de vecteurs).

**Alternatives rejetées**

Qdrant / LanceDB / base vectorielle externe — deux systèmes à synchroniser, viole le mono-fichier et le 100% local.

Embeddings dans SQLite + scan linéaire cosine maison — jetable, ne passe pas à l'échelle, réinvente ce que sqlite-vec fait déjà mieux.

API d'embedding/recherche cloud — fait fuiter les données, viole le zéro-cloud.

---

## ADR-003

### Candle pour l'inférence in-process

**Statut** : ✅ Accepted | **Date** : 2026-06

**Contexte**

Le RAG exige de transformer du texte en vecteurs. Trois familles de solutions : (a) appel à une API d'embedding cloud — exclu (zéro-cloud) ; (b) un runtime ONNX embarqué (fastembed/ort) — dépendance C lourde, fragile à compiler sur Windows (MSVC), toolchain externe ; (c) une inférence pure Rust.

BaseMyAI veut un binaire autonome, sans service ML séparé, qui se lie proprement sur les trois OS et se package en wheel/prebuild sans imposer de compilateur au client.

**Décision**

Inférence **in-process via Candle** (pur Rust). Modèle : `all-MiniLM-L6-v2` (384 dimensions). Pas d'ONNX, pas de fastembed.

```rust
trait Embedder {
    fn embed(&self, text: &str) -> Result<Vec<f32>>;
    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    fn model_id(&self) -> &str;   // ex. "all-MiniLM-L6-v2"
    fn dim(&self) -> usize;       // 384
}
```

**L'`Embedder` n'auto-télécharge JAMAIS le modèle.** Il reçoit un **chemin local**. Le fetch (et sa vérification d'intégrité) est orchestré par le produit, jamais par le core — pour garantir « zéro réseau par défaut ». BaseMyAI cache le modèle dans `~/.basemyai/models/` après un fetch explicite ; ForgeMyAI le fetch uniquement pendant `fmyai setup`.

**Conséquences**

✅ Pur Rust : se lie proprement sur Linux/Windows/macOS, pas de toolchain ONNX/MSVC fragile.
✅ Inférence in-process : pas de service ML séparé, un seul binaire.
✅ 384 dims, compatible avec le `nomic-embed-text-v1.5` (384) côté ForgeMyAI.
✅ `model_id()` permet de détecter un changement de modèle et de régénérer les vecteurs.
⚠️ Candle est plus jeune qu'ONNX — couverture de modèles plus restreinte. Acceptable : un seul modèle visé en V1.
⚠️ Inférence ML embarquée = risque de fuite mémoire à surveiller (stress-test 1h, profiling).
⚠️ Modèles multiples / multilingues reportés en V2.

**Alternatives rejetées**

ONNX Runtime / fastembed — dépendance C lourde, compilation Windows fragile (c'est le risque produit n°1 dans l'écosystème), toolchain externe.

API d'embedding cloud (OpenAI, Cohere) — viole le zéro-cloud, latence réseau, coût.

Service ML Python séparé (sidecar sentence-transformers) — casse le « un seul binaire », deux runtimes à gérer.

---

## ADR-004

### Les 4 couches mémoire

**Statut** : ✅ Accepted | **Date** : 2026-06

**Contexte**

« La mémoire d'un agent » n'est pas un concept unique. Un contexte de travail éphémère, un souvenir d'un événement passé, une procédure apprise et un fait établi ont des durées de vie, des modes d'accès et des stratégies de péremption différents. Tout mettre dans une seule table indifférenciée empêche de raisonner sur ces différences (TTL, GC, priorité de retrieval).

**Décision**

Quatre couches mémoire, modélisées en tables distinctes dans `basemyai` (jamais dans `basemyai-core`).

| Couche | Contenu | Durée de vie typique |
|---|---|---|
| `short_term` | Contexte de travail de la session courante | TTL court |
| `episodic` | Ce qui s'est passé et quand (événements, interactions) | Bornée dans le temps |
| `procedural` | Comment faire X : étapes, workflows, compétences apprises | Longue durée |
| `semantic` | Faits et connaissances, recherchables vectoriellement | Jusqu'à invalidation |

Chaque table porte les colonnes `valid_from` / `valid_until` (ADR-005) et est filtrée par `agent_id` (ADR-006). Index B-Tree adaptés au mode d'accès de chaque couche.

**Conséquences**

✅ Stratégie de péremption et de GC adaptée par couche.
✅ Le retrieval peut prioriser/filtrer par couche selon le besoin de l'agent.
✅ Modèle mental clair pour le développeur (« je range ça où ? »).
⚠️ Quatre schémas à maintenir cohérents.
⚠️ La frontière entre couches peut être floue pour certains cas d'usage ; documentation et exemples nécessaires.

**Alternatives rejetées**

Table mémoire unique avec un champ `type` — empêche d'optimiser index/GC par type, mélange des durées de vie incompatibles.

Couches arbitrairement configurables par l'utilisateur — complexité et incohérence ; les 4 couches couvrent les besoins réels des agents.

---

## ADR-005

### RAG temporel — `valid_from` / `valid_until`

**Statut** : ✅ Accepted | **Date** : 2026-06

**Contexte**

Une mémoire qui ignore le temps ment. « L'utilisateur est sur le plan Free » était vrai au T1, faux au T2. Un RAG classique (cosine pur) retourne les deux faits avec la même confiance ; l'agent affirme alors des informations périmées. Il manque une notion de validité temporelle intégrée à la recherche, pas appliquée après coup.

**Décision**

Chaque mémoire porte deux colonnes temporelles : `valid_from` et `valid_until` (nullable = « valide jusqu'à invalidation explicite »). Le recall est une **requête hybride** qui combine, en une seule requête SQL, la similarité vectorielle et le filtre temporel :

```sql
-- conceptuel
SELECT id, text
FROM   memory
WHERE  agent_id = ?1                         -- isolation (ADR-006)
  AND  (valid_until IS NULL OR valid_until > now())   -- validité temporelle
ORDER  BY vec_distance_cosine(embedding, ?2)  -- pertinence (sqlite-vec, ADR-002)
LIMIT  ?3;
```

Le filtre temporel est passé à `VectorIndex.knn(q, k, filtre)` comme le filtre SQL fourni par l'appelant. `basemyai-core` exécute le KNN ; il ne sait pas que le filtre concerne le temps — c'est le sens, propriété de `basemyai`.

**Conséquences**

✅ Le recall ne retourne que ce qui est pertinent **ET** encore valide.
✅ Historiser un fait (le remplacer) = poser `valid_until` sur l'ancien et insérer le nouveau ; l'historique reste auditable.
✅ Le filtre temporel s'exprime via le mécanisme générique de filtre du core — pas de couplage métier dans le core.
⚠️ `now()` doit être cohérent (horloge système) ; les fuseaux/horaires sont gérés en UTC.
⚠️ Les lignes expirées s'accumulent jusqu'au passage du GC (Active Worker, ADR-008).

**Alternatives rejetées**

Filtrer le temps **après** le KNN, côté application — inefficace (on récupère puis on jette), et risque de retourner < k résultats valides.

Pas de notion de temps (cosine pur) — l'agent affirme des faits périmés ; c'est précisément le problème à résoudre.

Versionnement d'événements complet (event sourcing) — overkill pour V1 ; deux colonnes temporelles suffisent.

---

## ADR-006

### Isolation multi-agent par `agent_id`

**Statut** : ✅ Accepted | **Date** : 2026-06

**Contexte**

Un service hébergeant plusieurs agents (ou plusieurs utilisateurs/tenants) partage le même store mémoire. Un agent ne doit **jamais** lire la mémoire d'un autre. Une fuite cross-agent n'est pas un bug fonctionnel : c'est un incident de sécurité (exfiltration de données d'un tenant vers un autre). Filtrer côté application est fragile — un oubli, et la fuite est silencieuse.

**Décision**

Chaque ligne mémoire porte un `agent_id`. **Toute** lecture et **toute** écriture sont filtrées par `agent_id` **au niveau SQL**, dans `basemyai`. Une requête sans `agent_id` valide échoue ; elle ne retourne jamais les données d'un autre agent.

```sql
WHERE agent_id = ?1   -- jamais omis, jamais optionnel
```

Le filtre `agent_id` est passé à `basemyai-core.knn(q, k, filtre)` comme partie du filtre SQL fourni par l'appelant. Le core applique le filtre sans savoir ce qu'est un agent. **L'isolation est un invariant de sécurité, pas une option de configuration.**

**Conséquences**

✅ Fuite cross-agent structurellement empêchée : le filtre est au niveau SQL, pas dans la logique applicative.
✅ Argument compliance direct (multi-tenant, RGPD).
✅ S'exprime via le mécanisme de filtre générique du core — pas de concept d'agent dans `basemyai-core`.
⚠️ Le filtre `agent_id` ne doit jamais pouvoir être contourné par injection SQL → inputs paramétrés, jamais interpolés (REQ-032).
⚠️ Pas de mémoire partagée volontaire entre agents en V1 (le défaut, et le seul mode, est l'isolation stricte).

**Alternatives rejetées**

Filtrage côté application (en Rust/Python, après la requête) — un oubli = fuite silencieuse ; trop fragile pour un invariant de sécurité.

Une DB par agent — coûteux à grande échelle (milliers d'agents), perd les avantages du mono-fichier, complexifie le worker de maintenance.

`agent_id` optionnel (isolation opt-in) — transforme un invariant de sécurité en piège ; rejeté.

---

## ADR-007

### Chiffrement au repos — sqlcipher

**Statut** : ✅ Accepted | **Date** : 2026-06

**Contexte**

Les données de mémoire (conversations, profils, faits sur les utilisateurs) sont parmi les plus sensibles d'un produit IA. Au repos, le fichier `.db` ne doit pas être lisible par quiconque accède au disque. Pour les personas sous contrainte compliance (santé, finance), le chiffrement au repos n'est pas optionnel.

**Décision**

Chiffrement via **sqlcipher** (fork de SQLite chiffrant pages et journal). La DB s'ouvre avec une `encryption_key` ; le fichier sur disque est illisible sans elle. La clé est **fournie à l'ouverture, jamais stockée**.

Statut différencié par niveau :
- Dans **`basemyai-core`** : sqlcipher est **optionnel**. `Store::open(path, key: Option<EncryptionKey>)`.
- Dans **`basemyai`** : le chiffrement est **obligatoire**. Instancier une mémoire sans `encryption_key` échoue.
- (Côté ForgeMyAI, consommateur du core : chiffrement **off par défaut** — un index de code est moins sensible, et le coût perf n'est pas justifié. Décision propre à ForgeMyAI.)

**Conséquences**

✅ Fichier mémoire illisible hors du process sans la clé.
✅ Argument compliance direct.
✅ Le core garde le chiffrement optionnel → réutilisable par des consommateurs qui n'en veulent pas (ForgeMyAI).
⚠️ **Compatibilité de build sqlcipher + sqlite-vec à valider** : sqlcipher est un fork de SQLite ; le linkage de l'extension n'est pas garanti. **Risque accepté, pas de spike préalable.** Repli de provisioning si le linkage échoue : build SQLite custom, ou chargement dynamique de l'extension dans le build sqlcipher.
⚠️ Gestion de la clé déléguée au consommateur : si la clé est perdue, les données sont irrécupérables (par conception).
⚠️ Léger surcoût de perf (chiffrement/déchiffrement des pages).

**Alternatives rejetées**

Chiffrement applicatif champ-par-champ — casse la recherche vectorielle (on ne peut pas faire de KNN sur des vecteurs chiffrés), complexe et partiel.

Chiffrement du système de fichiers (LUKS, BitLocker) — hors du contrôle du produit, pas portable, ne protège pas une fois le volume monté.

Pas de chiffrement — inacceptable pour les personas compliance ; rejeté pour `basemyai`.

---

## ADR-008

### Active Worker — thread de fond

**Statut** : ✅ Accepted | **Date** : 2026-06

**Contexte**

Le RAG temporel (ADR-005) laisse s'accumuler des lignes expirées (`valid_until` dépassé). Sans nettoyage, la DB grossit indéfiniment et les index se dégradent. Faire ce travail sur le chemin critique (à chaque write/read) ajoute de la latence imprévisible. Il faut un mécanisme de fond, découplé.

**Décision**

Un **Active Worker** : thread de fond (tokio) qui exécute périodiquement des tâches de maintenance, **sans bloquer** le chemin critique.

Côté **`basemyai-core`**, c'est le `MaintenanceWorker` : il fait tourner la boucle ; **les tâches sont injectées par le consommateur** (mécanisme au core, sens au consommateur). Le core fournit `PRAGMA optimize` / VACUUM partiel comme briques, mais n'embarque aucune tâche métier en dur.

Côté **`basemyai`**, les tâches enregistrées sont :
- **Task 1 — Garbage Collection** : nettoyer ou archiver les lignes dont le `valid_until` est expiré.
- **Task 2 — Optimisation** : `PRAGMA optimize`, VACUUM partiel si nécessaire.

```rust
worker.register(GcExpiredRows);     // tâche métier basemyai
worker.register(OptimizeDb);        // brique core, planifiée par basemyai
```

**Conséquences**

✅ Le chemin critique (write/recall) n'absorbe jamais le coût du GC ou de l'optimisation.
✅ La DB reste bornée et performante dans le temps.
✅ Le core reste agnostique : il fait tourner la boucle, le GC par `valid_until` est une tâche **injectée** par `basemyai`.
⚠️ Le GC introduit un délai entre l'expiration d'une ligne et son nettoyage effectif (les lignes expirées sont déjà exclues du recall par le filtre temporel, donc invisibles entre-temps).
⚠️ Un worker de fond mal réglé peut entrer en contention avec les écritures → WAL + busy_timeout + planification espacée.

**Alternatives rejetées**

GC synchrone sur le chemin critique — ajoute une latence imprévisible à chaque opération.

Pas de GC (laisser grossir) — DB non bornée, dégradation des perfs dans le temps.

Tâches de maintenance codées en dur dans `basemyai-core` — viole l'agnosticité du core (le GC par `valid_until` est un concept métier).

---

## ADR-009

### Trois surfaces de binding + wheels précompilés

**Statut** : ✅ Accepted | **Date** : 2026-06

**Contexte**

Le marché des builders d'agents IA est majoritairement Python, secondairement JS/TS, et résiduellement d'autres langages (Go, Ruby). Le cœur de BaseMyAI est en Rust. Pour être adopté, il doit être consommable **idiomatiquement** dans ces langages — et son installation ne doit pas exiger un compilateur C/Rust chez le client, sous peine d'écraser le taux d'adoption.

**Décision**

Trois surfaces de binding au-dessus du **même** `basemyai` :

| Surface | Techno | Cible | Packaging |
|---|---|---|---|
| SDK Python | PyO3 | builders Python (LangChain, LlamaIndex) | **wheel précompilé** (`pip install basemyai`) |
| SDK Node | NAPI-RS | builders JS/TS | **prebuild précompilé** (`npm install basemyai`) |
| Sidecar REST | axum | Go, Ruby, autres langages | binaire autonome unique |

(La 4ᵉ surface, le crate Rust natif consommé par ForgeMyAI, vise `basemyai-core` et fait l'objet d'ADR-001 ; elle n'est pas un « binding ».)

**Wheels et prebuilds précompilés** : `pip install` / `npm install` ne doivent **jamais** exiger un compilateur chez le client. La compilation se fait en CI, par plateforme.

**Conséquences**

✅ Adoption frictionless : `pip install basemyai`, deux lignes de code, mémoire opérationnelle.
✅ Un seul cœur Rust ; les trois bindings n'en sont que des façades — cohérence garantie.
✅ Le sidecar REST couvre les langages sans binding natif, sans dupliquer la logique.
⚠️ Matrice de build à maintenir : (Linux/Windows/macOS) × (Python versions / Node versions). CI lourde.
⚠️ Les dépendances C (sqlite-vec, sqlcipher) doivent compiler sur toutes les cibles de la matrice → testées dès le 1ᵉʳ commit du core.
⚠️ Le sidecar REST réintroduit du réseau pour ses consommateurs (assumé : c'est leur seul moyen sans binding natif ; reste local/loopback par défaut).

**Alternatives rejetées**

Exiger un compilateur chez le client (`pip install` qui compile) — écrase l'adoption ; la plupart des utilisateurs Python n'ont pas de toolchain Rust.

Réécrire le cœur dans chaque langage — trois implémentations à maintenir, divergence garantie, perte du bénéfice Rust.

REST seul (pas de bindings natifs) — impose un serveur et du réseau à tout le monde, latence et complexité de déploiement pour le cas Python/JS qui est le marché principal.

---

## ADR-010

### Provisioning du modèle hardware-aware (setup intelligent)

**Statut** : ✅ Accepted | **Date** : 2026-06

**Contexte**

ADR-003 a tranché que l'`Embedder` n'auto-télécharge **jamais** le modèle : il reçoit un chemin local. Mais cela laisse ouverte une question : **qui** choisit le modèle, **lequel**, sur **quel device** (CPU / CUDA / Metal), et **quand** le fetch a lieu ?

Deux mauvaises réponses encadrent le bon choix :
- **Download silencieux au premier lancement** (l'approche du plan de dev initial) — réseau surprise qui viole « zéro réseau par défaut », et risque de tirer un modèle inadapté à la machine (OOM sur un laptop faible, ou inférence CPU alors qu'un GPU est dispo).
- **Configuration 100% manuelle** — hostile : l'utilisateur ne connaît pas forcément sa VRAM, son backend GPU, ni quel modèle convient.

Le bon modèle existe déjà dans l'écosystème : AnythingLLM résout ça avec un setup qui **détecte le matériel** et recommande/sélectionne le provider et le modèle adaptés. ForgeMyAI a déjà la même idée avec `fmyai setup` (détecte GPU/RAM, choisit le modèle).

**Décision**

Une étape de **setup explicite et hardware-aware**, orchestrée par le **produit** (jamais par `basemyai-core`), exposée via la CLI (`basemyai setup`) et le premier appel des SDK. Elle :

1. **Détecte les specs** : RAM totale, présence et VRAM d'un GPU (CUDA / Metal / ROCm), nombre de cœurs CPU, OS.
2. **Résout le device Candle** : CUDA > Metal > CPU selon disponibilité.
3. **Sélectionne le modèle d'embedding** : baseline garantie `all-MiniLM-L6-v2` (384 dims, CPU-friendly) **partout** ; un modèle plus capable n'est proposé que sur machine apte (réservé V2 — V1 reste sur le baseline pour préserver la compatibilité `.idx` côté ForgeMyAI, cf. ADR-003 / D1 de l'écosystème).
4. **Fetch explicite** du modèle (consentement utilisateur + vérification d'intégrité par checksum), mis en cache dans `~/.basemyai/models/`.
5. **Persiste le choix** (`model_id`, `dim`, device) dans la config, et le passe ensuite à `basemyai-core.Embedder` sous forme de **chemin + device déjà résolus**.

`basemyai-core` reste agnostique : il reçoit un chemin de modèle et un device, il n'a aucune logique de détection matérielle ni de sélection. Le mécanisme d'inférence est au core ; la décision de *quoi* charger est au produit (mécanisme au core, sens au consommateur).

```
basemyai setup           (ou 1ᵉʳ appel SDK si non configuré)
  ├─ détecte RAM / GPU / VRAM / cœurs / OS
  ├─ device := CUDA > Metal > CPU
  ├─ model  := all-MiniLM-L6-v2 (baseline V1)
  ├─ fetch explicite + checksum → ~/.basemyai/models/
  └─ persiste { model_id, dim, device }
              │
              ▼
basemyai-core.Embedder(model_path, device)   ← reçoit du résolu, ne décide rien
```

**Conséquences**

✅ Bon modèle / bon device pour chaque machine, sans configuration manuelle (façon AnythingLLM).
✅ Respecte « zéro réseau par défaut » : le seul fetch est dans le setup, explicite et consenti.
✅ `basemyai-core` reste agnostique : il reçoit chemin + device résolus, aucune détection matérielle dans le core.
✅ Le GPU est exploité s'il est présent (latence d'inférence réduite) ; repli CPU transparent sinon.
⚠️ La détection matérielle est plateforme-spécifique (NVML pour CUDA, Metal sur macOS, `/proc` + sysinfo sur Linux) → code conditionnel par OS, à tester sur les trois plateformes.
⚠️ V1 reste sur le **seul** modèle baseline pour préserver la compat `.idx` avec ForgeMyAI ; la sélection multi-modèles hardware-aware n'est pleinement active qu'en V2.
⚠️ Si l'utilisateur saute le setup, le premier usage échoue **proprement** avec un message « run `basemyai setup` » — jamais un download surprise.

**Alternatives rejetées**

Auto-download silencieux au premier lancement (plan de dev initial, Phase 2) — réseau surprise, viole « zéro réseau par défaut », peut choisir un modèle inadapté au matériel.

Modèle et device codés en dur — ignore le GPU sur une machine capable, ou provoque un OOM / une inférence trop lente sur une machine faible.

Configuration 100% manuelle (l'utilisateur fournit tout) — hostile ; il ne connaît pas forcément ses specs ML ni le modèle adéquat.

Détection matérielle **dans** `basemyai-core` — violerait l'agnosticité du core (la sélection de modèle est une décision produit, pas une primitive de stockage).

---

## ADR-011

### Pivot vers libSQL (vecteur natif + chiffrement), traits async

**Statut** : ✅ Accepted | **Date** : 2026-06
**Supersede** : ADR-002 (sqlite-vec). **Amende** : ADR-003 (Candle tient), ADR-007 (chiffrement désormais via libSQL).

**Contexte**

ADR-002 prévoyait `sqlite-vec` (extension C à linker) + `rusqlite` + pool `r2d2` + `sqlcipher`. Trois frictions : le **linkage de l'extension** (le risque D4), la compatibilité de build **sqlcipher + sqlite-vec**, et une recherche **brute-force exacte** (pas d'ANN). Le pool était par ailleurs synchrone.

La recherche d'écosystème 2026 a fait émerger **libSQL** (fork SQLite production-ready, « le nouveau standard ») et **Turso DB** (réécriture Rust pure, async-natif, beta). libSQL apporte, **sans extension** : vecteur natif (`F32_BLOB`, `libsql_vector_idx`, `vector_top_k` en ANN), chiffrement au repos intégré, et une API **async**. Validé par un smoke test : libSQL compile sur Windows MSVC **sans CMake** et le vecteur natif fonctionne en mémoire.

**Décision**

- **Backend = libSQL** (crate `libsql`, embarqué local). Vecteur **natif**, plus de `sqlite-vec` ni `rusqlite`/`r2d2`.
- **Traits du core async** pour `Store` et les opérations vectorielles. L'`Embedder` **reste sync** (CPU-bound ; le consommateur l'enveloppe dans `spawn_blocking` si besoin). `MaintenanceTask` async (via `async-trait`).
- **Suppression du trait `VectorIndex`** : les ops vecteur sont natives sur `Store` (`vector_upsert`, `vector_knn`). La seule abstraction laissée aux consommateurs : `Embedder` (apporter le sien) + `Filter` (exprimer son sens).
- **Connexion partagée clonée** : libSQL synchronise l'accès en interne ; nécessaire pour que les bases `:memory:` restent cohérentes.
- **Chiffrement = feature `crypto`** (chiffrement libSQL, exige CMake) — opt-in, déféré. C'est l'équivalent résiduel du risque D4, désormais réduit à une **dépendance de toolchain**, pas un risque de code.
- Filtre paramétré (anti-injection) + validation d'identifiant de table conservés.
- **Chemin de migration vers Turso DB** (pur Rust, zéro C) quand il passera production (V2/V3).

**Conséquences**

✅ Risque D4 (linkage d'extension) **supprimé** : vecteur natif.
✅ **ANN natif** (`vector_top_k`) dès V1 — plus de brute-force jetable.
✅ Chiffrement au repos **intégré** (plus de combo sqlcipher + sqlite-vec à valider).
✅ **Async-natif** → colle à la vision « OS cognitif » (consolidation, appels LLM, retrieval multi-signal, sync).
✅ **SQLite-compatible** → le **graphe** (entités/relations) que la vision réclame se modélise en **tables + CTE récursives dans le même fichier** — pas de Kuzu/Neo4j. Hybride vecteur + graphe + temporel, **mono-fichier**.
✅ Chemin Turso (pur Rust) ouvert pour plus tard.
⚠️ Le chiffrement exige **CMake** → feature `crypto` à provisionner.
⚠️ Connexion partagée = accès sérialisé (acceptable embarqué ; pool multi-connexions pour fichiers à raffiner si besoin).
⚠️ libSQL reste un **fork C** (pas pur Rust) — assumé jusqu'à ce que Turso DB soit production.
⚠️ `vector_top_k` applique le filtre **après** le top-k → sur-échantillonner pour garantir `k` résultats filtrés (TODO).

**Alternatives rejetées**

`rusqlite` + `sqlite-vec` + `sqlcipher` (ADR-002 d'origine) — extension à linker (D4), brute-force, sync, combo sqlcipher fragile.

Turso DB **maintenant** — beta, pas production-ready ; trop risqué pour bâtir V1 dessus.

DB vectorielle ou graphe **externes** (Qdrant, Kuzu, Neo4j) — multi-systèmes à synchroniser, viole le mono-fichier local.

`fastembed`/ONNX pour les embeddings — réintroduit le risque ONNX/Windows ; **Candle (pur Rust) maintenu** (ADR-003).

---

## ADR-012

### Phase 2 Cognition — Graphe, RRF, Oubli adaptatif, Consolidation

**Statut** : ✅ Accepted | **Date** : 2026-06

**Contexte**

ADR-011 a posé le socle libSQL + vecteur natif + async, et noté en conséquence que « `vector_top_k` applique le filtre après le top-k → sur-échantillonner pour garantir `k` résultats filtrés ». La VISION §3 identifie cinq ingrédients d'une mémoire efficace : vecteurs, **graphe**, temporalité, **consolidation** et **oubli**. La pile libSQL + CTE récursives permet de tout tenir dans le même fichier sans système externe. La Phase 2 implémente ces quatre piliers restants dans le crate `basemyai`.

**Décision**

Quatre mécanismes implémentés dans `basemyai` (jamais dans `basemyai-core`) :

**1 — Oversampling KNN (correctif ADR-011 § TODO)**

`vector_top_k` applique le filtre *après* le top-k interne. Quand un filtre est présent, on tire `k × 8` (constante `KNN_OVERSAMPLE`) depuis l'index et on re-trie après jointure. Distance réelle via `vector_distance_cos` exposée dans le SELECT. Implémenté dans `basemyai-core::store::vector_knn`.

**2 — Graphe entités / relations**

Migration v2 : tables `entity(id, agent_id, kind, label, created_at)` + `edge(id, agent_id, src_id, rel, tgt_id, weight, created_at)`.

Traversée multi-sauts via **CTE récursive** :

```sql
WITH RECURSIVE reach(id, kind, label, depth) AS (
    SELECT id, kind, label, 0 FROM entity WHERE id = ?1 AND agent_id = ?2
    UNION                          -- UNION, pas UNION ALL : déduplication + terminaison de cycles
    SELECT e2.id, e2.kind, e2.label, r.depth + 1
    FROM   reach r
    JOIN   edge  ed ON ed.src_id = r.id AND ed.agent_id = ?2
    JOIN   entity e2 ON e2.id = ed.tgt_id
    WHERE  r.depth < ?3
)
SELECT * FROM reach;
```

`UNION` (pas `UNION ALL`) garantit la terminaison sur les cycles sans sentinelles supplémentaires.

**3 — Fusion multi-signal par RRF**

`rrf_fuse(rankings, k)` implémente la *Reciprocal Rank Fusion* :

```
score(doc) = Σ 1 / (60 + rank(doc, signal))
```

`k = 60` (valeur standard, choisie pour minimiser la sensibilité aux variations de rang). Tri final déterministe : score décroissant, puis id croissant. Provenance des signaux conservée dans `Fused::contributions`.

**4 — Oubli adaptatif**

Migration v3 : colonnes `importance REAL` et `last_access INTEGER` sur la table `memory`.

Score de rétention utilisé pour le GC périodique :

```
score = importance + H / (H + max(0, now - last_access))
```

où `H` est la demi-vie en secondes (`recency_half_life_secs`). Decay **hyperbolique** (pas exponentielle) pour deux raisons : (a) libSQL n'expose pas `pow`/`exp`/`ln` comme fonctions SQL ; (b) `0.5^(age/H)` avec `age ≈ 1.78 × 10⁹` s (timestamp Unix) s'arrondit à `0.0` en flottant, rendant tous les souvenirs indiscernables. La forme hyperbolique reste distinguable à toutes les échelles réelles.

Fenêtre de rétention par `agent_id` : `ROW_NUMBER() OVER (PARTITION BY agent_id ORDER BY score DESC)`, suppression des lignes `rn > capacity`.

**5 — Consolidation épisodes → faits**

`consolidate(memory, llm: &dyn LlmInference)` lit les N derniers épisodes (`episodic`), soumet un prompt d'extraction structurée au LLM, parse la réponse JSON, et :
- Peuple le graphe (entités + relations) via `ON CONFLICT DO UPDATE` (idempotent).
- Promeut les faits extraits en `semantic` avec déduplication par contenu exact.

Le pipeline est **idempotent** : relancer la consolidation sur les mêmes épisodes ne duplique ni les entités du graphe ni les faits sémantiques.

**Conséquences**

✅ Les cinq ingrédients de la mémoire hybride (VISION §3) tiennent dans un seul fichier libSQL.
✅ Pas de Kuzu, pas de LanceDB, pas de second système : graphe + vecteur + temporel en un seul fichier.
✅ Oubli adaptatif : capacité par agent bornée, signal de récence réel à toutes les échelles Unix.
✅ Consolidation idempotente : safe à rejouer périodiquement depuis le `MaintenanceWorker`.
✅ Graphe sans cycle infini : `UNION` dans la CTE récursive garantit la terminaison.
⚠️ libSQL n'expose pas `pow`/`exp`/`ln` — tout le calcul de score doit rester en arithmétique pure.
⚠️ `consolidate` nécessite un `LlmInference` injecté — câbler le provider est la responsabilité de l'appelant.
⚠️ Wiring de la consolidation dans `MaintenanceWorker` reporté : `MaintenanceTask::run` reçoit un `&Store`, pas `Arc<Memory>` + LLM provider.

**Alternatives rejetées**

`UNION ALL` dans la CTE récursive — pas de déduplication des nœuds, boucle infinie sur les cycles.

Decay exponentielle `0.5^(age/H)` — underflow à 0.0 à toutes les échelles de timestamps Unix réels ; indiscernable entre les souvenirs.

`pow`/`exp` en SQL — non disponibles dans libSQL (pas de fonction math standard) ; remplacés par arithmétique pure.

Graphe dans un système externe (Kuzu, Neo4j) — deux systèmes à synchroniser, viole le mono-fichier.

---

## ADR-013

### Inférence LLM model-agnostic + provisioning hardware-aware

**Statut** : ✅ Accepted | **Date** : 2026-06

**Contexte**

La consolidation (ADR-012) exige un LLM pour l'extraction structurée. Même philosophie qu'ADR-010 (provisioning hardware-aware pour les embeddings) et ADR-003 (l'`Embedder` ne télécharge jamais) : le composant d'inférence ne doit pas savoir quel backend il utilise, et le choix du modèle doit être adapté à la machine. En 2026, les LLM locaux sont accessibles via de nombreux runners incompatibles en apparence (Ollama, LM Studio, Jan, vLLM, KoboldCPP, LocalAI, GPT4All) mais convergent tous sur l'API OpenAI `/v1/chat/completions`.

**Décision**

**Trait model-agnostic `LlmInference`** (dans `basemyai`, pas dans `basemyai-core`) :

```rust
#[async_trait]
pub trait LlmInference: Send + Sync {
    async fn complete(&self, prompt: &str) -> Result<String>;
    fn model_id(&self) -> &str;
}
```

Injecté comme `Embedder` — le pipeline de consolidation ne sait pas quel backend il appelle.

**Backend universel `OllamaBackend`** — `POST /v1/chat/completions` (API OpenAI-compat). Couvre sans modification de code : Ollama, LM Studio, Jan, vLLM, KoboldCPP, LocalAI, GPT4All. Un seul backend pour 8 runners locaux.

**Table `KNOWN_MODELS`** (20 modèles, juin 2026, trié RAM décroissant) — des modèles 1B à 70B couvrant Llama 3.3, Qwen 3, Gemma 3, Devstral, DeepSeek-R1 distills, Phi-4, Mistral. RAM en Mo (Q4_K_M), mise à jour périodique.

**Détection `detect_llm_options()`** — sonde 8 backends sans jamais échouer (timeout 1 s) :

| Backend     | Port  | Méthode                  |
|-------------|-------|--------------------------|
| Ollama      | 11434 | `GET /api/tags`          |
| LM Studio   | 1234  | `GET /v1/models`         |
| Jan         | 1337  | `GET /v1/models`         |
| GPT4All     | 4891  | `GET /v1/models`         |
| KoboldCPP   | 5001  | `GET /v1/models`         |
| vLLM        | 8000  | `GET /v1/models`         |
| LocalAI     | 8080  | `GET /v1/models`         |
| AnythingLLM | 3001  | `GET /api/ping` (sonde seule) |

AnythingLLM est un **proxy RAG multi-provider**, non utilisable directement pour l'inférence : détecté et signalé, mais exclu de `best_llm_option` (`ram_mb = None`).

**Sélection `best_llm_option()`** — budget mémoire = `VRAM × 90 %` (si GPU) sinon `RAM × 60 %`. Parmi les options qui rentrent dans le budget, prend la plus lourde (= la plus capable). Zéro auto-install : `choose_llm()` retourne `Err` avec hint d'installation si rien ne convient.

**Conséquences**

✅ Un seul backend (`OllamaBackend`) couvre tous les LLM locaux OpenAI-compat sans branching.
✅ Zéro auto-download / zéro auto-install : même philosophie qu'ADR-010.
✅ 20 modèles couvrant toute la gamme (700 Mo → 45 Go) : chaque machine trouve quelque chose.
✅ Detection never fails : retourne une liste vide si rien n'est actif, ne panique pas.
✅ AnythingLLM signalé avec un message d'aide clair (configure Ollama / LM Studio comme backend sous-jacent).
⚠️ KNOWN_MODELS est une table statique — à mettre à jour à chaque nouvelle génération de modèles.
⚠️ Les tags Ollama sont la convention (ex. `qwen3:14b`) ; les autres backends peuvent nommer les modèles différemment → `ram_for()` fait une correspondance par préfixe, approximative pour les runners non-Ollama.
⚠️ `choose_llm()` retourne `OllamaBackend` en dur — pour un futur backend sans API OpenAI-compat, il faudra un variant ou une factory.

**Alternatives rejetées**

Backend Ollama seul — exclut LM Studio, Jan, vLLM, LocalAI et les autres runners courants en 2026.

Détection automatique + installation silencieuse du modèle (`ollama pull`) — viole ADR-010 (zéro action non consentie).

API d'inférence cloud (OpenAI, Anthropic) — viole le privacy-first / 100 % local.

Branching par backend dans le pipeline — `OllamaBackend` + `/v1/chat/completions` unifie tout ; le branching est une fausse complexité.

---

## ADR-014

### Recherche hybride : full-text BM25 (FTS5) fusionné au vecteur par RRF

**Statut** : ✅ Accepted | **Date** : 2026-06

**Contexte**

Le recall purement vectoriel rate les **termes exacts rares** : sigles, identifiants, références, noms propres peu fréquents (« ACME-42 », un UUID, un nom de fichier). L'embedding les noie dans la sémantique. À l'inverse, une recherche par mots-clés seule rate les reformulations. La RRF (`rrf_fuse`, ADR-012) était déjà en place mais sans second signal à fusionner. libSQL embarque **FTS5 + `bm25()`** dans son cœur (vérifié : virtual table + MATCH + ranking BM25 disponibles sans extension externe ni feature).

**Décision**

Index **full-text autonome** `memory_fts` (migration schéma V4) : table virtuelle FTS5 `(id UNINDEXED, agent_id UNINDEXED, content, tokenize='porter unicode61 remove_diacritics 2')`. Racinisation (`porter`) + pliage des accents, en-DB, 100 % local, zéro dépendance nouvelle.

- **Tenue à jour par la façade `Memory`** : INSERT au `remember`, DELETE au `forget` et au `purge_agent`. La migration V4 **backfille** les souvenirs déjà présents (`INSERT … SELECT FROM memory`).
- **`Memory::recall_hybrid(query, k)`** : produit deux classements — vecteur (`vector_knn`) et BM25 (`memory_fts MATCH … ORDER BY bm25`) — tous deux bornés `agent_id` + validité temporelle, puis les fusionne par `rrf_fuse`. Le `score` du `Record` retourné porte le score RRF fusionné.
- **Sûreté MATCH** : la requête libre est tokenisée (alphanumérique), chaque terme cité en littéral (insensible aux mots-clés FTS5 AND/OR/NEAR) et joint par OR (orienté rappel) — pas d'erreur de syntaxe ni d'injection.
- **Surfaces** : exposé via l'outil MCP `recall_hybrid`. REST/SDK : à câbler ultérieurement (même signature que `recall`).

**Conséquences**

✅ Parité (et différenciation) sur la recherche hybride sans quitter libSQL : pas de moteur externe, pas de réseau.
✅ Réutilise `rrf_fuse` (mécanisme pur déjà testé) — la fusion reste agnostique des poids.
✅ Isolation agent + validité temporelle préservées sur les **deux** signaux.
⚠️ `memory_fts` duplique le `content` (pas external-content) : coût disque ~1× le texte. Choix assumé pour la simplicité de synchronisation et l'absence de triggers.
⚠️ FTS5 exige le **nom réel** de la table dans `MATCH`/`bm25` (pas d'alias).
⚠️ `valid_until` n'est pas dans l'index FTS : la validité est filtrée par jointure sur `memory` au moment de la requête (un soft-delete laisse la ligne FTS, masquée au recall).

**Alternatives rejetées**

FTS5 **external-content** (`content='memory'`) — évite la duplication mais impose des triggers de synchronisation et un mapping `content_rowid` ; complexité supérieure pour un gain disque modéré.

Moteur de recherche externe (Tantivy, Meilisearch) — viole le single-file local / zéro-dépendance-externe (pile commune ADR-011).

Pondération linéaire vecteur+BM25 — exige de calibrer des poids hétérogènes ; la RRF s'en affranchit (ADR-012).

---

## ADR-015

### Métriques de distance additionnelles : euclidienne & hamming par re-classement

**Statut** : ✅ Accepted | **Date** : 2026-06

**Contexte**

L'index vectoriel natif libSQL est **cosinus** (`metric=cosine`). Certains cas d'usage veulent une autre métrique : euclidienne (L2, sensible à la magnitude) ou hamming (quantification binaire, rapide). Ré-indexer en plusieurs métriques alourdirait le schéma et le stockage.

**Décision**

`basemyai_core::Metric { Cosine, Euclidean, Hamming }` + `Store::vector_knn_metric(table, query, k, filter, metric)`. Pour `Cosine` : chemin natif inchangé. Pour `Euclidean`/`Hamming` : **sur-échantillonnage** du top-k cosinus (`k × 16`) puis **re-classement en Rust** sur les vecteurs réels récupérés via `vector_extract`. Hamming = nombre de dimensions où le signe diffère (1 bit/dim). Exposé côté `basemyai` par `Memory::recall_with_metric`.

**Conséquences**

✅ Métriques multiples sans index supplémentaire : un seul index cosinus alimente le rappel ANN.
✅ Mécanisme au core (agnostique), sens au consommateur (cohérent ADR-001).
⚠️ Le rappel reste piloté par l'ANN cosinus : pour des métriques très divergentes du cosinus, l'oversample ×16 est best-effort (un voisin L2-proche mais cosinus-lointain peut manquer).
⚠️ Re-classement = lecture des vecteurs des candidats (coût mémoire/CPU borné par `k × 16`).

---

## ADR-016

### AnythingLLM comme backend LLM de premier rang via API workspace-chat

**Statut** : ✅ Accepted | **Date** : 2026-06
**Amende** : ADR-013 (AnythingLLM n'est plus « détection seule » : c'est un backend d'inférence à part entière).

**Contexte**

ADR-013 a fait d'AnythingLLM un cas particulier : il est détecté (via `GET /api/ping`) mais exclu de `best_llm_option` avec `ram_mb = None` et ce commentaire : *« proxy multi-provider, non utilisable directement pour l'inférence »*.

Ce jugement était correct pour le cas général (AnythingLLM ne répond pas à `POST /v1/chat/completions`), mais trop restrictif : AnythingLLM expose une **API workspace-chat propre** authentifiée, `POST /api/v1/workspace/{slug}/chat`, qui :

1. Accepte un corps `{"message": "...", "mode": "chat"}` + header `Authorization: Bearer <api_key>`.
2. Retourne `{"textResponse": "...", "metrics": {...}}` — pas de format OpenAI.
3. Délègue au backend LLM **configuré dans le workspace** (Ollama, LM Studio, OpenAI, Anthropic…).
4. Répond en **moins de 50 ms** (mesure sur `qwen3-vl:4b-instruct` local via Ollama).

L'extraction JSON structurée a été validée en E2E (13 juin 2026) : le modèle `qwen3-vl:4b-instruct` via AnythingLLM extrait correctement des faits d'épisodes au format `{"facts":[{"subject":...,"predicate":...,"object":...,"confidence":...}]}`.

**La friction majeure d'ADR-013 était l'absence d'API key et de workspace slug dans le flux de détection automatique.** Ces informations ne sont pas découvrables sans authentification ; elles doivent être fournies explicitement par l'utilisateur (variables d'environnement ou config).

**Décision**

**1 — Nouveau backend `AnythingLlmBackend`** dans `basemyai/src/provision/llm.rs` :

```rust
pub struct AnythingLlmBackend {
    client:         Client,
    base_url:       String,
    workspace_slug: String,  // slug du workspace AnythingLLM (ex. "mon-espace-de-travail")
    timeout:        Duration,
}

impl AnythingLlmBackend {
    pub fn new(base_url: &str, workspace_slug: &str, api_key: &str) -> Self;
    pub fn with_timeout(mut self, timeout: Duration) -> Self;
}

#[async_trait]
impl LlmInference for AnythingLlmBackend {
    async fn complete(&self, prompt: &str) -> Result<String>;
    fn model_id(&self) -> &str;   // retourne le workspace_slug
}
```

Corps de la requête :

```json
{ "message": "<prompt>", "mode": "chat" }
```

Réponse parsée via `text_response` (champ `"textResponse"` JSON). Erreur explicite si `textResponse` est `null` ou vide.

**2 — Variables d'environnement pour la config AnythingLLM** :

| Variable | Usage |
| --- | --- |
| `BASEMYAI_ANYTHINGLLM_URL` | URL de base (défaut : `http://localhost:3001`) |
| `BASEMYAI_ANYTHINGLLM_KEY` | Clé API Bearer (obligatoire si fallback activé) |
| `BASEMYAI_ANYTHINGLLM_WORKSPACE` | Slug du workspace (obligatoire si fallback activé) |

**3 — Mise à jour de `choose_llm()`** :

La politique de sélection devient **à deux niveaux** :

```text
Niveau 1 (hardware-aware, inchangé)
  detect_llm_options() → best_llm_option() → OpenAiCompatBackend
  Si un modèle direct tient en RAM → retourner ce backend.

Niveau 2 (fallback AnythingLLM, si niveau 1 échoue)
  Si les trois variables d'env. BASEMYAI_ANYTHINGLLM_* sont définies
      → AnythingLlmBackend::new(url, slug, key)
  Sinon → Err(MemoryError::Inference) avec hint (inchangé + nouvelle ligne d'aide
           "ou configurer BASEMYAI_ANYTHINGLLM_KEY + BASEMYAI_ANYTHINGLLM_WORKSPACE")
```

Le niveau 2 n'exige **pas** de connaître le modèle ou sa RAM : AnythingLLM gère ça en interne. `LlmProvision.ram_mb` vaut `None` dans ce cas.

**4 — `probe_anythingllm` inchangée** : la détection reste sans auth (simple `GET /api/ping`). Les informations de configuration (clé, workspace) ne transitent jamais dans la phase de découverte automatique.

**5 — Test E2E gated** : `tests/consolidation_e2e.rs`, annoté `#[ignore]` (jamais exécuté en CI), déclenché manuellement par `cargo test -- --ignored consolidation_e2e`. Lit les trois variables d'env., crée des épisodes, appelle `consolidate()`, vérifie que des entités et arêtes apparaissent dans le graphe. C'est la première exécution E2E du pipeline consolidation→graphe contre un vrai LLM.

**Conséquences**

✅ AnythingLLM devient un backend d'inférence à part entière — aucune modification du code consommateur (`consolidate` injecte un `&dyn LlmInference`, agnostique).
✅ Couvre le cas où Ollama n'est pas accessible directement mais AnythingLLM tourne en proxy (cas fréquent : Ollama configuré dans AnythingLLM mais non exposé sur le port 11434).
✅ Zéro regression : la politique niveau 1 (Ollama/LM Studio directement) est prioritaire et inchangée.
✅ `LlmInference` reste le seul contrat — `AnythingLlmBackend` est un détail d'implémentation.
✅ La validation E2E de la consolidation est enfin possible sans Ollama exposé.
⚠️ Le modèle effectivement utilisé est opaque pour BaseMyAI : `model_id()` retourne le `workspace_slug`, pas le nom du modèle. `ram_mb = None` dans `LlmProvision`.
⚠️ Le prompt passe par le RAG d'AnythingLLM (similarité de workspace) avant d'atteindre le LLM — si le workspace contient des documents, ils peuvent polluer la réponse. Recommandation : utiliser un workspace vide ou dédié BaseMyAI.
⚠️ `mode: "chat"` conserve l'historique de session côté AnythingLLM (par `sessionId`). La consolidation n'envoie pas de `sessionId` → chaque appel est sans état de son côté, mais AnythingLLM crée une nouvelle session à chaque requête. Sans `sessionId`, les sessions orphelines s'accumulent. **Mitigation V2** : passer un `sessionId` fixe par agent (ex. `"basemyai-{agent_id}"`).
⚠️ AnythingLLM n'expose pas `GET /v1/models` → exclu de la détection automatique hardware-aware (`ram_mb = None` reste correct). La RAM consommée dépend du backend sous-jacent configuré dans AnythingLLM.

### Alternatives rejetées

Tenter `/api/v1/openai/chat/completions` — retourne `401` en mode single-user (testé 13 juin 2026) ; ce endpoint nécessite le mode multi-utilisateur avec un JWT distinct. Incompatible avec la configuration par défaut d'AnythingLLM.

Lire la config AnythingLLM (`~/.config/anythingllm/...`) pour extraire automatiquement la clé — dépendance à des détails d'implémentation internes non documentés, fragile et plateforme-spécifique.

Forcer l'utilisateur à configurer Ollama directement plutôt qu'AnythingLLM — UX dégradée pour qui a déjà AnythingLLM en service ; le proxy ajoute des features (RAG sur workspace, logs UI) que l'utilisateur a peut-être intentionnellement choisies.

Ajouter AnythingLLM au niveau 1 (hardware-aware) — impossible sans connaître le modèle et sa RAM, informations non accessibles sans auth. Le niveau 2 (fallback explicitement configuré) est le bon modèle.

---

## ADR-017

### Consolidation par sampling MCP (emprunter le LLM du client) + politique des modes LLM

**Statut** : ⛔ Superseded par **ADR-018** | **Date** : 2026-06
**Amende** : ADR-013, ADR-016 (ajoute une 3ᵉ source d'inférence ; clarifie la politique de bout en bout).

> **Superseded (13 juin 2026)** : le sampling MCP n'est **pas** le levier plug-and-play
> escompté. Vérifié sur sources officielles : Claude Code ne l'implémente pas (feature
> request ouvert, `-32601 Method not found`), et la primitive est **dépréciée** dans le
> protocole (SEP-2577, 2026-07-28). ADR-018 inverse la priorité : LLM côté serveur si
> disponible, sinon **consolidation pilotée par l'agent** (universelle), sampling devenu
> simple option opportuniste. `SamplingBackend` et l'outil `consolidate` restent, recâblés.

**Contexte**

La consolidation (ADR-012) exige un LLM. ADR-013/016 ont câblé deux sources : serveur local OpenAI-compat (Ollama, LM Studio…) et AnythingLLM. Les deux supposent que **l'utilisateur a installé et configuré un LLM** quelque part. Pour une part importante de l'audience visée — les utilisateurs de **Claude Code, Claude Desktop, Cursor, Windsurf, ChatGPT Desktop, Codex** — c'est une friction inutile : **ils ont déjà un LLM**, celui de leur agent. Leur demander d'installer Ollama *en plus* pour que BaseMyAI consolide est absurde.

Le rôle de BaseMyAI vis-à-vis de ces agents n'est pas de *consommer* un LLM : c'est de **leur fournir une mémoire persistante**. Le canal est **MCP** (`basemyai-mcp`, déjà implémenté). Or le protocole MCP expose une primitive **`sampling/createMessage`** : un serveur MCP peut demander au client de produire une complétion LLM. C'est exactement le levier manquant — le serveur **emprunte le cerveau du client**.

Vérifié (13 juin 2026) : `rmcp 1.7` expose `Peer<RoleServer>::create_message(CreateMessageRequestParams) -> CreateMessageResult` côté serveur, et `ClientHandler::create_message` côté client. Un test E2E in-memory (serveur + client MCP reliés par duplex) valide le chemin complet `remember → consolidate (sampling) → graphe peuplé → recall_graph`.

**Décision**

**1 — `SamplingBackend` (dans `basemyai-mcp`, pas `basemyai`)**

Un backend qui implémente `basemyai::LlmInference` en déléguant `complete()` à `peer.create_message(...)`. Il vit dans `basemyai-mcp` (le seul crate qui dépend de `rmcp`) : **le crate mémoire reste agnostique de MCP**. `model_id()` retourne `"mcp-sampling"` (le modèle réel est choisi par le client, connu seulement au retour via `CreateMessageResult::model`).

**2 — Outil MCP `consolidate`**

Nouvel outil : l'agent l'appelle avec un `agent_id` ; le handler récupère le `Peer` depuis le `RequestContext<RoleServer>`, construit un `SamplingBackend`, et exécute `consolidate(memory, &backend)`. Le sampling se produit **pendant l'appel d'outil** : le serveur sous-demande au client, le LLM du client extrait, le graphe est peuplé. Déclenchement **explicite** (déterministe, observable) ; le worker de fond avec `Peer` capturé est reporté en V2 (cycle de vie du peer + multi-sessions HTTP à cadrer).

**3 — Politique des sources de consolidation, ordonnée et explicite**

```text
1. Sampling MCP    — si BaseMyAI tourne comme serveur MCP (outil `consolidate`)
                     → emprunte le LLM du client. Zéro install, zéro clé.
2. LLM local       — Ollama / LM Studio / AnythingLLM (ADR-013/016)
                     → détection hardware-aware, reste sur la machine.
3. Cloud opt-in    — Claude API / OpenAI API (BYOK), UNIQUEMENT si configuré
                     explicitement → sort de la machine (voir implications).
4. Indisponible    — la mémoire (remember/recall/graphe manuel) fonctionne ;
                     la consolidation auto est simplement absente.
```

**4 — Implications de confidentialité, à exposer clairement (exigence produit)**

Chaque mode a un périmètre de données différent ; le produit DOIT le rendre lisible (doc + message au setup) :

| Mode | Où partent les épisodes | Privacy-first ? | Consentement |
| --- | --- | --- | --- |
| **Sampling MCP** | Vers le client MCP que l'utilisateur a **déjà** choisi (Claude Code, ChatGPT…). BaseMyAI n'impose aucun tiers. | ✅ Oui — c'est *son* client, *son* modèle, *son* choix (local ou cloud, décidé par lui). MCP prévoit un **consentement humain** au sampling côté client. | Implicite (le client a sa propre UX de consentement). |
| **LLM local** | Nulle part : reste sur la machine (localhost). | ✅ Oui — 100 % local, le pilier d'origine. | Implicite (serveur local lancé par l'utilisateur). |
| **Cloud opt-in (BYOK)** | Vers Anthropic / OpenAI (selon la clé fournie). Les épisodes — données les plus sensibles — **quittent la machine**. | ⚠️ **Non** — rompt le 100 % local. À n'activer qu'en connaissance de cause. | **Explicite obligatoire** : variable d'env. dédiée + avertissement au démarrage. Jamais de défaut, jamais silencieux. |

Le mode cloud n'est jamais le défaut et ne s'active jamais par simple présence d'une clé d'environnement générique : il exige une variable **dédiée et non ambiguë** (`BASEMYAI_CLOUD_LLM_OPTIN=1` + clé), et émet un avertissement explicite « vos épisodes sont envoyés à `<provider>` » au premier usage.

**Conséquences**

✅ **Vrai plug-and-play** pour les agents MCP : `claude mcp add basemyai …` suffit, la consolidation marche sans aucun LLM installé ni clé.
✅ Le crate `basemyai` **reste agnostique de MCP** : `SamplingBackend` est dans `basemyai-mcp`, derrière le trait `LlmInference`.
✅ Un seul backend de sampling couvre **tous** les hôtes MCP (Claude Code/Desktop, Cursor, Windsurf, ChatGPT Desktop…).
✅ Le sampling reste **privacy-first** : la donnée passe par le client choisi par l'utilisateur, pas par un tiers imposé par BaseMyAI.
✅ La politique à 4 niveaux dégrade proprement : il y a toujours une réponse claire (jusqu'au mode « consolidation absente mais mémoire fonctionnelle »).
⚠️ Le sampling exige une **session MCP active** : il ne marche pas pour les consommateurs SDK Python/Node/REST standalone (eux relèvent des modes local ou cloud).
⚠️ Le client peut **refuser** le sampling (consentement humain) → `complete` remonte une erreur claire ; l'appelant peut retomber sur un autre mode.
⚠️ Le modèle réel du sampling est **opaque** (`model_id = "mcp-sampling"`) et sa qualité dépend du client — un petit modèle local côté client donnera une extraction plus pauvre.
⚠️ Le mode cloud opt-in **viole le pilier 100 % local** : c'est un choix de l'utilisateur, encadré, jamais un défaut. Implémentation déférée (le backend Claude/OpenAI BYOK fera l'objet de son propre câblage, sous cette politique).

**Alternatives rejetées**

Consolidation en tâche de fond via sampling (worker périodique avec `Peer` capturé) — séduisant mais le `Peer` n'est valide que pendant une session ; multi-sessions HTTP et cycle de vie à cadrer. Reporté V2 ; l'outil explicite couvre le besoin V1.

Mettre `SamplingBackend` dans `basemyai` — importerait `rmcp` dans le crate mémoire, violant son agnosticité (ADR-001). Il vit dans `basemyai-mcp`, derrière le trait.

Cloud par défaut / activé par une clé générique (`OPENAI_API_KEY`…) — exfiltration silencieuse des données les plus sensibles. Inacceptable : le cloud est opt-in dédié et explicite, jamais déduit.

Forcer l'installation d'un LLM local pour tous — c'est la friction même que cet ADR supprime pour les utilisateurs d'agents MCP.

## ADR-018

### Consolidation pilotée par l'agent — politique d'inférence à niveaux (supersède ADR-017)

**Statut** : ✅ Accepted | **Date** : 2026-06-13
**Supersède** : ADR-017. **Amende** : ADR-012, ADR-013, ADR-016.

**Contexte**

ADR-017 pariait sur le **sampling MCP** comme levier « plug-and-play » : le serveur emprunte le LLM du client via `sampling/createMessage`. Le test E2E réel dans Claude Code (v2.1.176, 13 juin 2026) a invalidé le pari :

- **Claude Code n'implémente pas le sampling** : `consolidate` remonte `MCP error -32601: Method not found`. C'est documenté comme *feature request* ouvert ([anthropics/claude-code#1785]), pas un bug de notre code.
- **Le sampling est déprécié dans le protocole** : SEP-2577 (2026-07-28) déprécie Roots, Sampling et Logging. *« New implementations should NOT adopt it. »*
- **Aucun autre client majeur** (Claude Desktop, Cursor, Windsurf, ChatGPT) ne confirme le support.

Or BaseMyAI tourne le plus souvent **dans un agent qui est lui-même un LLM** (Claude dans Claude Code). Le bon levier n'est donc pas de *demander* une complétion au client (sampling), mais d'**inverser le contrôle** : laisser l'agent faire l'extraction avec son propre raisonnement, et n'exposer côté serveur que la préparation (épisodes + consigne) et la persistance. C'est universel (outils + prompts MCP, supportés partout), non déprécié, et de meilleure qualité (c'est le modèle de l'agent, pas un petit LLM local).

**Décision**

**1 — `consolidate()` du crate `basemyai` scindé en briques réutilisables**

`consolidation_prompt(memory) -> Option<ConsolidationInput>` (lit les épisodes valides + bâtit le prompt), `parse_extraction(raw) -> Extraction`, `apply_extraction(memory, &Extraction) -> ConsolidationReport` (peuple le graphe + promeut les faits, idempotent). `consolidate(memory, &dyn LlmInference)` compose les trois — **signature inchangée, rétrocompatible**. Les types `Extraction` / `ExtractedEntity` / `ExtractedRelation` deviennent publics (sans dépendance à `schemars` : le crate mémoire reste pur).

**2 — Outil MCP `consolidate` : politique d'inférence à niveaux**

```text
1. Sampling MCP   — SEULEMENT si le client annonce la capability `sampling`
                    (rare ; déprécié). Vérifié via peer.peer_info().capabilities.
2. LLM local      — choose_llm() : Ollama/LM Studio/AnythingLLM détecté ou env.
                    Autonome : le serveur fait l'extraction, l'agent reçoit le bilan.
3. Piloté agent   — sinon : renvoie status="extraction_required" + episodes +
                    instructions. L'AGENT appelant extrait avec son propre LLM,
                    puis persiste via `consolidate_apply`. Universel, zéro install.
```

**3 — Nouvel outil MCP `consolidate_apply`** — reçoit `agent_id` + `facts`/`entities`/`relations` (types `JsonSchema` propres au crate MCP, convertis en `basemyai::Extraction`), appelle `apply_extraction`. Idempotent.

**4 — Nouveau prompt MCP `consolidate_memory`** — pilote le flux de bout en bout en mode interactif : `/mcp__basemyai__consolidate_memory agent_id=X` injecte les épisodes + la consigne ; l'agent extrait et appelle `consolidate_apply`.

**5 — Annotations d'outils** (best-practice MCP) sur les 8 outils : `read_only_hint`, `destructive_hint`, `idempotent_hint`, `open_world_hint=false` (mémoire = monde fermé, local).

**Conséquences**

✅ La consolidation marche **dans Claude Code** (et tout client MCP) sans serveur LLM ni clé, en empruntant le LLM de l'agent — le vrai plug-and-play, supérieur au sampling (qualité du modèle de l'agent).
✅ Le mode autonome (worker de fond, SDK, REST) garde le LLM local/cloud (pas d'agent pour piloter).
✅ Plus de dépendance à une primitive dépréciée ; le sampling reste branché en option opportuniste à coût nul (sauté si non annoncé).

⚠️ Le mode « piloté agent » consomme des tokens de l'agent et suppose qu'il suit la consigne (extraire → `consolidate_apply`). La description de l'outil et le prompt cadrent ce flux.
⚠️ La sélection du niveau dépend de l'environnement (un LLM local détecté prime sur l'agent-driven) ; documenté. Un utilisateur voulant la qualité de l'agent malgré un LLM local utilise le prompt `consolidate_memory`.
⚠️ Le mode cloud opt-in BYOK reste **déféré** (mode hérité d'ADR-017, sous garde-fous).

**Alternatives rejetées**

Garder le sampling en primaire (ADR-017) — invalidé : non supporté par le client cible, déprécié dans le protocole.

Forcer un LLM local embarqué (llama.cpp/mistral.rs in-process) pour l'autonomie totale — lourd ; reporté V2 (le modèle Candle actuel est *embedding-only*, incapable de génération). L'agent-driven couvre le besoin interactif sans ce coût.

Élicitation MCP pour l'extraction — l'élicitation demande une saisie **humaine** structurée, pas une génération LLM ; inadaptée à l'extraction de faits.

[anthropics/claude-code#1785]: https://github.com/anthropics/claude-code/issues/1785
