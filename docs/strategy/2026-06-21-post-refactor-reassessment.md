# BaseMyAI — Réévaluation stratégique post-refactor

**Date** : 2026-06-21
**Type** : réévaluation (pas une nouvelle recherche de marché)
**Document parent** : `docs/strategy/2026-06-18-agent-memory-database-research.md`
**Directive d'origine** : *BaseMyAI is not a SQLite wrapper. It is an AI-native memory database. SQLite/libSQL is only the first storage backend unless research proves otherwise.*

> **Pourquoi ce document existe.** La recherche du 2026-06-18 a été écrite,
> **acceptée (ADR-019) et exécutée**. Re-dérouler le même prompt produirait un
> doublon périmé. Ce document fait autre chose : il **confronte les
> recommandations d'origine à ce qui a réellement été construit en 3 jours**,
> identifie là où la réalité a divergé (et pourquoi), et **redéfinit les
> prochaines étapes** — qui ne sont plus « avant refactor » mais « avant
> lancement ». Le refactor est fait.

---

## 1. Executive summary

La recherche du 2026-06-18 a bien orienté le produit. Trois jours plus tard, la
quasi-totalité de ses recommandations « avant refactor » sont **livrées et
testées** : ADR-019 (`.bmai` + frontière backend), ADR-020 (`StorageEngine`
orienté mémoire, `Filter` confiné), format `.bmai` documenté et écrit dans
`bmai_meta`, CLI complète, tests d'isolation adversariaux, doc « not a vector
DB », et — point que la recherche reconnaissait comme son trou — **un benchmark
concurrentiel réellement exécuté** vs Mem0+Qdrant.

Trois conclusions nouvelles, qui n'étaient **pas** disponibles le 2026-06-18 :

1. **Le différenciateur n'est pas la performance brute.** Le benchmark
   2026-06-21 le prouve : à `infer=False`, Mem0+Qdrant écrit à ~8 % de la
   latence de BaseMyAI, et **en lecture Mem0 est plus rapide** (113 ms vs
   168 ms p50). Les moteurs de stockage sont comparables. Le « 62× » sur le
   write-path est entièrement la **taxe LLM synchrone** de Mem0, pas une
   supériorité de libSQL. La thèse produit doit donc s'appuyer sur le
   **contrat** (pas de taxe LLM au write, temporalité/isolation/chiffrement
   natifs), **pas** sur la vitesse. C'est un repositionnement de message, pas de
   produit.

2. **Le risque n°1 n'est plus la sur-ingénierie — c'est la distribution.** Le
   moteur (Phase 1 + Phase 2) et les surfaces (MCP, REST, CLI, bindings) sont en
   place et dépassent le plan. Mais **rien n'est publié** : crates.io / npm /
   PyPI = 0. Le produit existe et n'est installable par personne. Le centre de
   gravité du travail a basculé de l'architecture vers le *go-to-market*.

3. **Une tension de sécurité ouverte mérite un ADR, pas un patch.** `agent_id`
   n'est pas lié à l'identité authentifiée (Bearer partagé) : sur une instance
   REST/MCP multi-agent non fiable, un agent peut usurper l'`agent_id` d'un
   autre. C'est cohérent avec le mono-déploiement local actuel, mais contredit
   frontalement le marketing « isolation = invariant de sécurité ». À statuer
   explicitement.

**Recommandation nette** : ne pas relancer de cycle de recherche/refactor.
Geler le périmètre moteur, **publier core→basemyai→npm→PyPI**, et faire passer
le message de « rapide » à « le bon contrat mémoire, en local, sans taxe LLM ».

---

## 2. Méthode — ce qui a changé depuis le 2026-06-18

Sources internes relues (postérieures à la recherche d'origine) :

- `docs/ADR-019-agent-memory-database-format-and-engine.md` (Accepted)
- `docs/ADR-020-memory-store-trait.md` (suivi ADR-019)
- `docs/ADR-021-libsql-reader-pool.md`, `docs/ADR-022-memory-event-broadcast.md`
- `docs/status.md` (2026-06-20, déclarée **source de vérité**)
- `docs/format/bmai-v1.md`, `docs/not-a-vector-db.md`,
  `docs/zero-network-after-setup.md`
- `docs/benchmarks/local-memory-vs-mem0-qdrant.md` (**run faisant autorité
  2026-06-21**)
- `docs/TODO.md` (plan M0→M7 ; archivé depuis sous `docs/archive/TODO-2026-06.md`), `docs/PRD.md`

Limite assumée : pas de nouvelle revue concurrentielle exhaustive. Le paysage
(Mem0, Zep/Graphiti, Letta, LangMem, Cognee, Supermemory, Hindsight, Qdrant /
LanceDB / Chroma) n'a pas changé de structure en 3 jours ; le tableau de la
recherche d'origine reste valide. Ce document ne le re-dérive pas, il s'y
réfère et ne corrige que les deltas pertinents (§6).

---

## 3. Carte de score — recommandations d'origine vs réalité construite

| Recommandation (2026-06-18) | Statut 2026-06-21 | Preuve / écart |
|---|---|---|
| Garder libSQL en V1, le cacher derrière le framing produit | ✅ Fait | ADR-019 Accepted |
| Exposer `.bmai` même si libSQL en interne | ✅ Fait | `bmai_meta` (`format=basemyai-memory`, `format_version=1`, `storage_engine=libsql`, `embedding_dim=384`), `BMAI_FORMAT_VERSION=1`, `docs/format/bmai-v1.md` |
| Créer un `StorageEngine` **minimal orienté mémoire**, pas SQL générique | ✅ Fait | ADR-020 : `MemoryStore`/`LibsqlMemoryStore`, `tests/storage_contract.rs` ; `Filter`/`Value` confinés (plus dans `memory/mod.rs` ni `cognition/`) |
| Préparer un backend natif sans l'implémenter | ✅ Fait | `EngineCapabilities`, `EngineKind::Libsql`, doc ; aucun moteur natif écrit |
| Réduire la V1 à la boucle `open→remember→recall→inspect` + CLI + un SDK | 🟡 Partiel | CLI ✅ complète ; mais **toutes** les surfaces existent déjà (MCP, REST, Node, Python) — l'inverse du « ne pas tout livrer en même temps ». Pas un échec : c'est de l'avance, mais le focus *distribution* est dilué. |
| MCP > REST comme canal prioritaire 2026 | ✅ Confirmé | `basemyai-mcp` = surface la plus aboutie (8 outils, stdio+HTTP, sampling ADR-018) |
| Studio en V1.5, Tauri en V2 | ⏸️ Correctement reporté | Aucune dette cachée |
| Tests d'isolation adversariaux + anti-injection | ✅ Fait | `tests/p1_isolation_adversarial.rs`, documenté `SECURITY.md` |
| Matrice Implemented/Planned (source de vérité) | ✅ Fait | `docs/status.md` (2026-06-20) |
| Bench libSQL + comparatif marché | ✅ **Fait** (le trou reconnu est comblé) | `docs/benchmarks/local-memory-vs-mem0-qdrant.md`, run 2026-06-21, chiffres réels |
| Bench KNN scalabilité 10k/100k/1M, stress 1h | 📋 Non commencé | M6 ouvert — **seul gros pan technique non démarré** |
| Publier (core→basemyai, npm, PyPI) | ❌ **Zéro** | `basemyai-core` dry-run vert ; **rien de publié** |

**Lecture** : sur ~12 recommandations « avant refactor », 9 livrées, 1
correctement reportée, 2 ouvertes (scalabilité KNN, publication). Le document
d'origine a tenu. Le travail restant n'est plus de l'architecture.

---

## 4. Ce que le benchmark a réellement prouvé (et la vérité inconfortable)

La recherche d'origine se terminait sur : *« benchmark conceptuel, pas un
benchmark de latence sur machine cible »*. Ce trou est désormais comblé, et le
résultat **corrige la stratégie de message**.

Run 2026-06-21 (i7-13620H, RTX 4060 8 GiB, Win11, corpus 500), p50 :

| | write p50 | recall p50 |
|---|---:|---:|
| BaseMyAI (embed+store) | **76 ms** | **168 ms** |
| Mem0 `infer=False` (embed+store, sans LLM) | 82 ms (1,08×) | **91 ms** |
| Mem0 `infer=True` (embed+**LLM extract**+store) | 4 714 ms (**62×**) | 113 ms |

Trois faits stratégiques :

1. **Les moteurs de stockage sont à égalité.** libSQL natif vs Qdrant : ~8 %
   d'écart au write. Vendre BaseMyAI comme « plus rapide que Qdrant » serait
   malhonnête et réfutable en une commande.
2. **Le « 62× » est la taxe LLM, pas le stockage.** Mem0 lance une extraction
   LLM synchrone à chaque `.add()` (98 % de sa latence write). BaseMyAI fait
   l'extraction dans un `consolidate()` séparé, explicite et *backgroundable*
   (ADR-018). **C'est le vrai argument** : « pas de taxe LLM sur le chemin
   d'écriture chaud », à toujours citer avec la ligne `infer=False`.
3. **BaseMyAI perd en lecture, et le doc le dit.** 168 ms vs 91/113 ms : le
   ré-embedding Candle de la requête à chaque `recall` domine. La doc benchmark
   l'assume explicitement (« never imply BaseMyAI wins on read latency — it does
   not »). **Implication produit** : un cache d'embedding de requête / un chemin
   `recall_by_vector` (vecteur déjà calculé) devient une optimisation V1.5 à
   valeur marketing, pas un détail.

Conclusion : le différenciateur est **le contrat** (privacy, temporalité,
isolation, pas de taxe LLM au write), **jamais la vitesse**. Tout le copy doit
être réécrit dans ce sens.

---

## 5. Divergences et tensions à résoudre

### 5.1 « Core agnostique » vs « memory database core » — tension assumée mais non tranchée

La recherche signalait que `basemyai-core` « types/traits mémoire » contredit
l'invariant historique « core agnostique métier ». Le refactor a **gardé les
deux** : le core reste agnostique (`StorageEngine`/`EngineCapabilities`,
`tests/agnosticity.rs` toujours vert) et le contrat mémoire
(`MemoryStore`/`LibsqlMemoryStore`) vit dans `basemyai`. C'est un bon compromis,
mais deux zones restent hors du trait par décision documentée (ADR-020) :
`memory/porting.rs` et `maintenance/{gc,forgetting}`. **À acter** : ce n'est
pas une dette, c'est une frontière. Le risque est qu'un futur second backend
doive ré-implémenter ces deux zones. À documenter comme « non couvert par le
contrat backend » dans `bmai-v1.md`, sinon la promesse « backend swappable »
est partiellement fausse.

### 5.2 `agent_id` non lié à l'identité authentifiée — contradiction marketing/sécurité

`docs/status.md` / TODO M6.1 : sur REST/MCP, le Bearer est partagé entre agents
locaux → un agent peut se déclarer `agent_id` d'un autre (confused deputy /
fuite cross-tenant). **Reporté volontairement** (décision 2026-06-20), cohérent
avec le mono-déploiement local. **Mais** le pitch dit « isolation = invariant de
sécurité, fuite cross-agent structurellement impossible ». Au niveau SQL, oui.
Au niveau réseau multi-agent, non. **Action** : un ADR explicite (clés API
scopées par agent ou dérivation depuis le token) **et** une formulation honnête
du README : « isolation SQL stricte ; le binding identité↔agent_id sur surface
réseau partagée est V2 ». Ne pas laisser le marketing dépasser la garantie.

### 5.3 Tout livré en même temps — l'inverse de la consigne « resserrer la V1 »

La recherche recommandait de **ne pas** livrer Python+Node+REST+MCP+CLI
ensemble. Ils existent tous. Ce n'est pas un défaut de qualité (chacun est
testé), mais c'est une **dilution du focus de lancement** : 5 surfaces à
publier, documenter et maintenir au lieu d'1-2 excellentes. **Action** :
choisir **une** surface de lancement (recommandé : **MCP + Python**, cf. §7) et
marquer les autres « disponibles, support best-effort » jusqu'à traction.

### 5.4 Les docs de plan (TODO.md, CLAUDE.md) sont en retard sur le code

`status.md` recense 7 contradictions résolues (CLAUDE.md « reste ouvert » obsolète,
TODO M2/M3 décrivant des bindings « à créer » qui existent, etc.). `status.md`
fait foi, mais **CLAUDE.md et TODO.md mentent encore au lecteur occasionnel**.
**Action** : un pass de réconciliation (1 h) pour aligner CLAUDE.md §Statut et
les en-têtes M2/M3/M4 de TODO.md sur `status.md`. Sinon chaque nouvel agent/dev
repart sur une carte fausse.

---

## 6. Lecture concurrentielle — deltas pertinents

Le tableau complet de la recherche d'origine reste valide. Seuls les deltas qui
changent une décision :

- **La perf n'est pas un axe de bataille gagnable** (cf. §4). Contre Qdrant/
  LanceDB/Chroma, BaseMyAI ne doit jamais se comparer en throughput vectoriel.
  L'axe est *contrat mémoire + privacy locale + fichier unique chiffré*.
- **Mem0 reste le concurrent de positionnement le plus direct** (« drop-in
  memory »), et le benchmark donne enfin une réponse factuelle à « pourquoi pas
  Mem0 ? » : *parce que Mem0 paie une taxe LLM de 4,7 s par écriture en config
  par défaut, et envoie tes données dans une stack à orchestrer ; BaseMyAI est
  un fichier `.bmai` local, chiffré, sans LLM au write.*
- **Zep/Graphiti reste le concurrent de crédibilité** (graphe temporel). BaseMyAI
  a un graphe (CTE récursive) et de la consolidation, mais pas la maturité
  temporelle de Graphiti. Ne pas prétendre l'égaler ; revendiquer « local-first
  embedded » comme l'axe orthogonal.
- **Supermemory / Hindsight** confirment que **MCP + UX cross-tool** est le bon
  canal 2026 — ce que le code reflète déjà (MCP = surface la plus aboutie).

Aucune raison nouvelle de remettre en cause libSQL en V1. La directive tient :
*le backend n'est pas le produit.*

---

## 7. Re-priorisation — les vrais trous maintenant

Le travail n'est plus « décider l'architecture ». C'est **rendre le produit
installable, honnête et adopté**. Par ordre de levier :

**P0 — Distribution (le seul vrai bloqueur de valeur)**
1. Publier `basemyai-core` puis `basemyai` sur crates.io (dry-run core déjà vert).
2. Publier le wheel PyPI et le package npm (workflows déjà présents, jamais
   déclenchés). **Python d'abord** (marché agents principal).
3. Choisir la surface de lancement : **MCP + Python SDK**. REST/Node restent
   dispo, support best-effort.

**P1 — Message honnête (réécriture, pas code)**
4. Réécrire README/landing autour du **contrat**, pas de la vitesse. Toujours
   citer `infer=False` à côté du « 62× ». Ne jamais impliquer une victoire en
   lecture.
5. Réconcilier CLAUDE.md / TODO.md avec `status.md` (§5.4).

**P2 — Combler les deux trous techniques restants**
6. Bench KNN scalabilité 10k/100k/1M + stress mémoire Candle 1h (M6) — sinon la
   promesse « tient à l'échelle » est non prouvée.
7. ADR sur le binding identité↔`agent_id` (§5.2). Décider, même si la décision
   est « hors scope V1, documenté ».

**P3 — Adoption (après publication)**
8. Wrappers LangChain / LlamaIndex (TODO M3, toujours manquants) — c'est le pont
   vers les personas 2.
9. Optimisation lecture (cache d'embedding requête / `recall_by_vector`) pour
   refermer l'écart de 77 ms vs Mem0 — V1.5.

**À NE PAS faire maintenant** : backend natif `.bmai`, Tauri, sync, Turso,
multi-modèles, nouveau cycle de recherche. Tous correctement reportés ;
les rouvrir serait une fuite devant le travail de distribution.

---

## 8. Risques (mis à jour)

| Risque | Impact | Prob. | Mitigation |
|---|---:|---:|---|
| **Produit jamais publié** (perfectionnisme moteur) | Critique | **Haut** | Geler le périmètre, publier cette semaine, traiter la distribution comme P0 |
| Message « plus rapide » réfuté publiquement | Haut | Moyen | Pivoter le copy vers le contrat ; citer `infer=False` ; assumer la lecture plus lente |
| Marketing « isolation invariant » > garantie réelle (réseau) | Haut | Moyen | ADR auth + README honnête (§5.2) |
| Maintenir 5 surfaces dilue la qualité | Moyen | Haut | 1 surface de lancement (MCP+Py), reste best-effort |
| Scalabilité KNN non prouvée (>100k) | Moyen | Moyen | Bench M6 avant tout claim de scale |
| Docs de plan trompeuses (CLAUDE/TODO) | Moyen | Haut | Pass de réconciliation sur `status.md` |
| Lecture 2× plus lente que Mem0 freine l'adoption | Moyen | Moyen | Cache embedding requête (V1.5) ; honnêteté entre-temps |
| Frontière backend incomplète (porting, maintenance) | Faible-Moyen | Moyen | Documenter « hors contrat » dans `bmai-v1.md` |

---

## 9. Recommandation finale

La direction du 2026-06-18 était la bonne et a été exécutée. **Ne pas
re-décider.** La phase « réfléchir avant de coder » est close ; la phase
« publier et être honnête » s'ouvre.

1. **Geler le moteur.** Phase 1 + Phase 2 sont suffisantes pour une V1.
2. **Publier** core→basemyai→PyPI→npm. C'est le seul travail qui crée de la
   valeur maintenant.
3. **Réécrire le message** autour du contrat (privacy, temporalité, isolation,
   pas de taxe LLM), jamais de la vitesse.
4. **Trancher** la tension `agent_id`↔auth par un ADR, et aligner le README sur
   la garantie réelle.
5. **Prouver l'échelle** (KNN bench, stress 1h) avant tout claim de scale.
6. **Reporter** sans culpabilité : natif, Tauri, sync, multi-modèles, Studio.

Phrase directrice mise à jour :

> **BaseMyAI n'est pas un wrapper SQLite, ni un vector store plus rapide. C'est
> le fichier `.bmai` local, chiffré et temporel pour la mémoire privée d'agents
> — et son avantage n'est pas la vitesse, c'est le contrat.**

---

## 10. Prochaines étapes concrètes (avant *lancement*, plus avant refactor)

1. `cargo publish -p basemyai-core` puis `-p basemyai` (core déjà dry-run vert).
2. Déclencher `python-wheels.yml` + publier PyPI ; idem npm.
3. Réécrire README §positionnement + landing autour du contrat (citer benchmark
   honnêtement).
4. Réconcilier CLAUDE.md §Statut et TODO.md M2/M3/M4 avec `status.md`.
5. Écrire l'ADR « identité authentifiée ↔ `agent_id` » (décision, même
   négative).
6. Lancer le bench KNN scalabilité (criterion) + stress Candle 1h (M6).
7. Documenter dans `bmai-v1.md` les zones hors contrat backend (porting,
   maintenance).
8. Écrire les wrappers LangChain / LlamaIndex (pont persona 2).
9. (V1.5) Cache d'embedding de requête / `recall_by_vector` pour la latence
   lecture.

---

## 11. Prompt recommandé pour la suite (orienté *distribution*, plus refactor)

```text
Agis comme un staff engineer Rust + release engineer.

Contexte : BaseMyAI (Agent Memory Database locale) a son moteur (Phase 1+2) et
ses surfaces (MCP, REST, CLI, bindings Node/Python) implémentés et testés, mais
RIEN n'est publié (crates.io / npm / PyPI = 0). Le refactor StorageEngine
(ADR-020) et le format .bmai (ADR-019) sont faits. Lis docs/status.md (source de
vérité), docs/TODO.md (M1-M7), docs/benchmarks/local-memory-vs-mem0-qdrant.md.

Objectif : amener BaseMyAI à un premier `pip install basemyai` qui marche, sans
toucher au moteur ni rouvrir l'architecture.

Contraintes :
- Ne pas modifier la sémantique mémoire ni les invariants (chiffrement
  obligatoire, agent_id, temporalité, zéro réseau implicite, SQL paramétré).
- Surface de lancement prioritaire : MCP + Python. REST/Node restent
  best-effort.
- Le message public s'appuie sur le CONTRAT (privacy, temporalité, isolation,
  pas de taxe LLM au write), jamais sur la vitesse. Toute citation du benchmark
  inclut la ligne infer=False et assume la lecture plus lente.

Livrables :
1. Plan de publication ordonné (core → basemyai → PyPI → npm) avec les
   bloqueurs réels par étape.
2. Réconciliation CLAUDE.md / TODO.md avec status.md.
3. Brouillon d'ADR « identité authentifiée ↔ agent_id ».
4. README §positionnement réécrit autour du contrat.
5. Plan de bench KNN scalabilité + stress 1h (M6), sans l'exécuter si l'env ne
   le permet pas.
```

---

## Annexe — pourquoi ce document plutôt qu'une nouvelle recherche

La recherche du 2026-06-18 répond déjà aux 11 questions stratégiques du prompt
d'origine (SQLite V1, `.bmai`, `StorageEngine`, backend natif différé,
différenciateurs, V1/V1.5/V2, Studio, business model open-core, risques, revue
PRD/ADR). Ces réponses sont devenues des **décisions** (ADR-019/020) et du
**code**. Les re-générer aurait produit un doublon. La valeur, à J+3, est de
**vérifier que les paris ont tenu** (oui, §3), d'**exploiter la donnée nouvelle**
que la recherche n'avait pas (le benchmark, §4) et de **repointer l'effort** là
où le risque a migré : de l'architecture vers la distribution et l'honnêteté du
message (§7).
