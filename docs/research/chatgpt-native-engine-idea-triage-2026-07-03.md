# Triage — idées d'une session ChatGPT sur le moteur natif (2026-07-03)

- Date : 2026-07-04
- Statut : **analyse / triage** (pas un ADR — aucune idée retenue ici n'est actée ; elle devra passer par un ADR dédié le moment venu, au bon jalon N-quelque-chose)
- Source : une conversation ChatGPT du 2026-07-03 proposant un plan complet "BaseMyAI Native Memory Database" (12 piliers, programmes A→L, roadmap V0.2→V1.0). Le message ChatGPT s'auto-corrige en fin de conversation ("oui sur la direction, non sur l'exécution en bloc").
- But de ce doc : séparer ce qui vaut la peine d'être noté pour plus tard de ce qui contredit des décisions déjà prises ou l'ordre déjà séquencé dans `PLAN-NATIVE-ENGINE.md` / `TODO-NATIVE-ENGINE.md`.

> Garde-fou transverse inchangé : `basemyai-core` reste **agnostique métier**.
> Rien ci-dessous ne justifie de l'enfreindre.

---

## 0. Cadrage — pourquoi ce triage plutôt qu'adopter le plan tel quel

Le plan ChatGPT est généré sans connaissance de l'état réel du repo : il propose
`ADR-023` à `ADR-029` alors que `ADR-024` (pivot natif) et `ADR-025` (fondation
LSM, spike N1 clos le 2026-07-04) existent déjà et portent un contenu différent.
Il propose aussi un langage de requête (BML) comme quasi-immédiat (Programme E,
V0.4), alors que `TODO-NATIVE-ENGINE.md` (N6) trace explicitement l'inverse :
*décision produit préalable — `remember`/`recall` sans langage de requête est un
avantage documenté à protéger*, cf. `docs/research/surrealdb-gap-analysis.md`
§0 (« l'agent appelle `remember`/`recall`, pas du SQL »). Le plan ChatGPT défait
donc, sans le savoir, un choix de positionnement déjà tranché.

Le reste du plan (12 piliers, 12 programmes parallèles, roadmap V0.2→V1.0) est
la reformulation grandiose d'un chantier déjà séquencé et gated : N0→N6, une
case ne se coche que si le critère de sortie est vérifié (test/CI/chiffre). Le
risque n'est pas l'ambition — elle est déjà assumée (« pas un projet vibe code
en 2 semaines », [[native-engine-pivot]]) — c'est de remplacer un ordre strict
(harnais avant moteur, N2 avant N3, etc.) par 12 fronts simultanés.

**Conclusion du triage : rien ici ne devient un ADR ni une todo actionnable
maintenant.** Ce sont des idées à ressortir au bon moment, quand le jalon
correspondant s'ouvrira (N4+, N6, ou "parallèle — indépendant du moteur").

---

## 1. Idées à retenir, classées par jalon où elles redeviennent pertinentes

### Recall trace / explain (pertinent dès N3, mûrit jusqu'à N5)

Concept : chaque `recall` produit une trace structurée — stratégie utilisée,
candidats vus, scores par signal (vecteur/keyword/graphe/temporel), raisons du
classement final, filtres appliqués. Exemple de forme (JSON) :

```json
{
  "recall_id": "rec_01J...",
  "strategy": "hybrid_temporal_graph",
  "candidates_seen": 124,
  "returned": [{
    "record_id": "mem_01J...",
    "final_score": 0.91,
    "why": ["agent scope matched", "fact is currently valid", "semantic similarity high"],
    "scores": {"vector": 0.84, "keyword": 0.62, "graph": 0.18}
  }]
}
```

Pourquoi c'est solide : `recall_hybrid` fait déjà de la fusion RRF multi-signal
(ADR-012) — la trace n'ajoute aucun nouveau mécanisme de scoring, elle rend
visible ce qui existe déjà. C'est un candidat naturel pour différencier le
produit sans reconstruire quoi que ce soit de risqué. À évaluer concrètement
quand `basemyai-engine` aura un recall planner natif (N3/N4), pas avant —
tracer un moteur libSQL de transition a peu de valeur si le format change sous
les pieds.

### Policy engine mémoire (pertinent à partir de N5, avant bascule de défaut)

Concept : règles déclaratives appliquées à l'écriture/l'oubli — ex. détection
"secret-like" au `remember`, expiration automatique de préférences faibles,
suppression physique sur demande RGPD avec preuve de compaction. Une partie
existe déjà en pointillé : `AdaptiveForgetting` (décroissance hyperbolique),
purge RGPD dans `Memory`. L'idée à retenir n'est pas "construire un moteur de
règles", c'est **rendre ces règles déclaratives et inspectables** plutôt que
codées en dur — pertinent pour le marché entreprise/compliance, mais seulement
une fois que N5 (parité + chiffrement natif + hardening) est en vue, pas avant.

### Identity Capsule (idée, pas de jalon assigné — à re-discuter après N5)

Concept : un agent a une identité versionnée (rôle, objectifs, scope de mémoire
autorisé, profil de confiance) au-delà du simple `agent_id` isolant les
requêtes. Intéressant pour la continuité inter-session d'un agent, mais c'est
une extension de *sens* (ce que `basemyai` porte), jamais du *mécanisme* core —
et ça n'a de valeur que si `remember`/`recall` restent la seule surface agent
(cf. §0). À ne considérer qu'après N5, et seulement si un besoin produit concret
apparaît (pas en spéculatif).

### Eval harness façon LoCoMo/LongMemEval (pertinent en continu, indépendant du moteur natif)

Concept : un jeu de scénarios mémoire (préférence qui change, fait corrigé,
entité renommée, requête adversariale cross-agent) mesuré séparément du coût
LLM (embed latency / planner latency / index latency / rerank latency ne
doivent pas être mélangés — sinon on répète le piège classique de confondre
coût moteur et coût modèle). Contrairement au reste, **ceci ne dépend pas du
moteur natif** : ça peut se construire contre le backend libSQL actuel dès
maintenant, un peu comme le bench KNN M6 existant
(`docs/benchmarks/m6-knn-results-2026-07-01.md`). Bon candidat pour la section
"Parallèle — indépendant du moteur" de `TODO-NATIVE-ENGINE.md` si quelqu'un veut
s'y mettre avant N2.

### Repair/doctor tooling (déjà en germe côté CLI, à muscler à N5/N6)

Concept : `basemyai doctor`/`verify`/`repair` sur un conteneur `.bmai` —
vérification de manifest, détection de segment tronqué, reconstruction d'index.
Le CLI actuel a déjà `verify`, `stats`, `gc`. L'idée à garder n'est pas une
nouvelle commande à créer maintenant, c'est que **N2 (harnais crash-consistency)
et ce tooling partagent la même détection de corruption** — quand N2 définira
"qu'est-ce qu'un segment corrompu", le doctor CLI doit consommer la même
logique plutôt qu'en réinventer une.

---

## 2. Idées explicitement écartées (contredisent une décision déjà prise)

- **BML (langage de requête) en V0.4 / quasi-immédiat** — contredit N6
  (« décision produit préalable, pas surface agent ») et le pitch protégé dans
  `surrealdb-gap-analysis.md` §0. Reste une idée pour un outil interne/CLI
  *après* Phase 4, pas un livrable de mi-parcours.
- **12 programmes lettrés en parallèle (A→L)** — contredit l'ordre strict de
  `TODO-NATIVE-ENGINE.md` (harnais avant moteur, N2 avant N3...). Un seul front
  à la fois tant que N2 n'est pas clos.
- **Renumérotation ADR-023→029** — collision avec `ADR-024`/`ADR-025` déjà
  existants et actés ; toute nouvelle décision issue de ce triage suivra la
  numérotation réelle du moment (`docs/ADR.md` fait foi).
- **"Packed .bmai single-file format"** — mentionné comme cible V1.0 ; pas
  d'objection de fond, mais aucun signal que le format directory actuel (choix
  N1/ADR-025) soit un problème réel — à ne considérer que si le format
  directory montre une limite concrète (distribution, taille de conteneur).

---

## 3. Liens

[[native-engine-pivot]] — décision pivot + état N1/N2.
`docs/PLAN-NATIVE-ENGINE.md`, `docs/TODO-NATIVE-ENGINE.md` — séquencement N0→N6 qui fait foi.
`docs/research/surrealdb-gap-analysis.md` — pitch protégé (`remember`/`recall` sans langage de requête).
`docs/adr/ADR-024-native-engine.md`, `docs/adr/ADR-025-native-engine-storage-foundation.md` — décisions actées.
