# Plan d'implementation - Context Engine et Recall Quality Lab

Statut : en cours - Rust, R1.4, socle R1.5 et SDK Python/Node livres  
Portee : crate `basemyai`, surfaces publiques et evaluation deterministe  
Prerequis : moteur natif-only, recall hybride, provenance ADR-036

## 1. Decision produit

BaseMyAI doit compiler un contexte directement consommable par un agent, pas
seulement retourner une liste de souvenirs similaires.

Le contrat produit vise a repondre a cette question :

> A partir d'une requete, d'une politique et d'un budget, quel contexte utile,
> actuel et tracable faut-il fournir au modele ?

Le Context Engine reste une couche de `basemyai`, au-dessus de `Memory` et du
recall existant. Il n'orchestre ni agents, ni outils, ni appels de modeles.

## 2. Principes

- `basemyai-core` reste agnostique metier ; tout le contexte vit dans
  `basemyai`.
- `Memory` reste la frontiere d'isolation : `ContextRequest` ne prend pas un
  second `agent_id`.
- La compilation reutilise `recall_hybrid_with_options`; elle ne duplique pas
  vector search, BM25 ou RRF.
- Le chemin V1 est local, deterministe et sans appel LLM.
- Le budget de tokens estime est une limite dure.
- Chaque item final reste relie a ses identifiants de memoire.
- `TrustLevel` decrit une provenance, pas une garantie de securite. Une
  politique peut filtrer une source, mais ne certifie jamais son contenu.
- Les surfaces publiques adaptent le contrat Rust canonique sans reimplementer
  les politiques.

## 3. Non-objectifs V1

- reecriture ou resume generatif ;
- resolution universelle des contradictions ;
- relations `supersedes` deduites et persistees automatiquement ;
- profils de modeles ou tokenizers telecharges ;
- runtime d'agents, workflows, handoffs ou tool calling ;
- interface graphique avant stabilisation des traces et metriques.

## 4. Pipeline cible

```text
ContextRequest
    -> recall hybride borne
    -> filtres layer/provenance/validite
    -> normalisation et estimation de tokens
    -> deduplication deterministe
    -> selection sous budget
    -> sections et rendu
    -> ContextBundle + citations + exclusions
```

Les etapes de compilation doivent rester pures autant que possible afin d'etre
testees avec des `Record` synthetiques, sans store ni embedder.

Organisation du code :

```text
crates/basemyai/src/context/
  mod.rs        facade Memory, validation et re-exports
  types.rs      contrats publics
  token.rs      estimation de tokens
  compile.rs    filtres, normalisation et deduplication
  selection.rs  utility ranking, quotas et remplacement
  temporal.rs   statut de validite et facteur de fraicheur
  render.rs     sections, Markdown, citations et metriques
```

## 5. Contrat Rust cible

```rust
let bundle = memory
    .compile_context(
        ContextRequest::new("Prepare release 0.2", 8_000)
            .candidate_limit(64)
            .include_procedural()
            .explain(),
    )
    .await?;
```

Le bundle expose :

- les sections et items structures ;
- le rendu final ;
- le nombre de tokens estime ;
- les citations vers les IDs memoire ;
- les exclusions et leur raison lorsque `explain` est actif.

Invariants publics :

- budget et limite de candidats strictement superieurs a zero ;
- limite de candidats bornee pour contenir CPU et memoire ;
- `estimated_tokens <= token_budget` en cas de succes ;
- au moins un ID source par item ;
- ordre stable a donnees et options identiques ;
- aucun score non fini utilise pour trier ;
- aucun type public lie a un fournisseur de modele.

## 6. Roadmap Context Engine

### R1.0 - Contrat et fixtures

- figer `ContextRequest`, `ContextBundle`, sections, citations et exclusions ;
- inventorier les metadonnees reellement disponibles dans `Record` ;
- definir les bornes et valeurs par defaut ;
- produire cinq fixtures verticales avant les politiques avancees.

Sortie : API Rust minimale, aucun changement des chemins de recall existants.

### R1.1 - Collecte et estimation

- collecter via `recall_hybrid_with_options` ;
- conserver rang, score, couche, source et trust derive ;
- ajouter un trait `TokenEstimator` injectable ;
- fournir un estimateur local conservateur sans dependance modele.

Sortie : chaque candidat a un cout borne et une provenance.

### R1.2 - Filtres et deduplication exacte

- appliquer les options procedural/import existantes ;
- permettre une politique plus stricte sur les sources inconnues ;
- normaliser les espaces pour le rendu ;
- dedupliquer le contenu normalise en conservant tous les IDs sources ;
- tracer les fusions exactes separement des exclusions `SourceFiltered` et
  `TokenBudget`.

Sortie : compilation locale deterministe de bout en bout.

### R1.3 - Selection V1 sous budget

- selectionner d'abord dans l'ordre du recall hybride ;
- calculer le cout du rendu complet, titres et citations inclus ;
- rejeter un item qui ferait depasser la limite ;
- stabiliser l'ordre des sections et items.

Sortie : conformite budget a 100 % sur tests de proprietes.

### R1.4 - Utility ranking

Etat : implemente et couvert par des tests deterministes.

Separer score de retrieval et utilite de compilation :

```text
utility = relevance
        * provenance_weight
        * freshness_weight
        * profile_weight
        * novelty_weight
```

- ajouter reservations et quotas par section ;
- utiliser utilite et valeur par token ;
- effectuer une passe de remplacement locale ;
- documenter qu'il s'agit d'un heuristique borne, pas d'un optimum global.

Implementation :

- pertinence normalisee depuis le score RRF, avec fallback sur le rang ;
- poids de couche et de provenance utilises comme priorites, jamais comme
  certification de securite ;
- reservation du meilleur candidat de chaque section lorsque le budget le
  permet ;
- remplissage par utilite par token ;
- remplacement local seulement si l'utilite augmente, sans perdre la derniere
  section representee et sans depasser le budget.

Le facteur de fraicheur est ajoute par R1.5 apres propagation de `Validity`.
Les profils de compilation restent reportes a R1.6.

### R1.5 - Temporalite et conflits

Etat : socle temporel implemente ; supersession et conflits explicites
reportes jusqu'a l'ajout d'une relation persistante fiable.

- exploiter invalidation et expiration deja appliquees par recall ;
- exposer le statut temporel lorsque les metadonnees seront presentes dans le
  contrat public ;
- respecter une relation de remplacement explicite si elle existe ;
- signaler les conflits non resolus au lieu d'inventer une verite courante ;
- garder les requetes historiques capables de retrouver l'ancien etat.

Livre :

- `Validity` propagee du moteur natif jusqu'a `Record` et `ContextItem` ;
- timestamp `compiled_at` dans le bundle ;
- statuts `Current`, `Scheduled` et `Expired` ;
- statut temporel conserve sur chaque exclusion explicative ;
- rejet defensif d'un candidat hors de sa fenetre, meme si le recall filtre
  deja les resultats courants ;
- facteur de fraicheur borne dans `[0.9, 1.0]`, utilise comme departage doux
  dans l'utilite sans ecraser la pertinence ;
- tests de plomberie native, bornes et preference de recence.

Non livre volontairement : detection textuelle de contradictions et relation
`supersedes`. Le depot ne stocke aujourd'hui ni cle de sujet canonique ni lien
de remplacement entre deux souvenirs. Les inferer par similarite produirait des
faux positifs et contredirait la politique conservatrice de ce plan.

### R1.6 - Roles et profils

Profils initiaux : `Balanced`, `Conversation`, `Coding`, `Execution` et
`SafetyCritical`. Ils configurent des poids et quotas, jamais des permissions.

Roles de contexte : faits, contraintes, procedures, evenements, references et
donnees incertaines. Un role est derive de metadonnees explicites ou de la
couche ; V1 n'infere pas une instruction depuis le texte libre.

### R1.7 - Rendus et explicabilite

- renderers texte, Markdown et JSON ;
- contributions retrieval disponibles ;
- clusters de deduplication ;
- raisons d'inclusion et d'exclusion ;
- avertissements de conflit ;
- trace compacte par defaut et detaillee avec limite de taille.

### R1.8 - Surfaces publiques

Ordre de livraison :

1. Rust ;
2. CLI ;
3. MCP et REST ;
4. Python et Node.

Toutes les surfaces conservent enums, valeurs par defaut, erreurs et invariants
du contrat Rust. Des tests de parite empechent leur divergence.

Etat au 2026-07-17 :

- Rust : livre ;
- Python et Node : livres avec bundle structure, rendu, citations, traces,
  typings, exemples et tests runtime ;
- CLI, MCP et REST : encore ouverts. Leur ajout doit adapter le contrat
  canonique sans reimplementer les politiques.

## 7. Recall Quality Lab

### R2.1 - Dataset et runner

Introduire un schema JSONL versionne contenant : cas, memories, requete,
options, `must_include`, `must_exclude`, budget, attentes de provenance et
graine deterministe.

Le runner cree un store isole, charge les donnees, execute recall et compilation,
puis emet un rapport JSON stable et un code de sortie exploitable en CI.

### R2.2 - Metriques retrieval

- Hit@K, Recall@K et Precision@K ;
- Mean Reciprocal Rank ;
- nDCG quand les niveaux de pertinence sont fournis ;
- exact-ID hit rate ;
- latence rapportee separement de la qualite.

### R2.3 - Metriques de bundle

- couverture des items obligatoires ;
- inclusion d'elements interdits ;
- conformite au budget ;
- ratio de tokens dupliques ;
- couverture de provenance ;
- taux de faits obsoletes ;
- fuite de contenu source-filtre ;
- couverture des procedures ;
- conflits non signales.

### R2.4 - Suites minimales

1. pertinence directe ;
2. IDs et termes exacts ;
3. faits obsoletes et remplacement temporel ;
4. provenance importee/inconnue et contenu hostile ;
5. procedures necessaires ;
6. deduplication ;
7. budgets 512, 2 000, 8 000 et 32 000 ;
8. graph hops ;
9. isolation inter-agent ;
10. determinisme.

### R2.5 - CLI et CI

```bash
basemyai eval run datasets/recall-core.jsonl --output report.json
basemyai eval compare reports/baseline.json reports/current.json
```

Le gate devient bloquant apres stabilisation des fixtures. Les premiers
invariants bloques sont : budget, isolation, provenance, determinisme et cas
critiques `must_include`/`must_exclude`.

L'evaluation avec modele ou LLM-as-a-judge reste optionnelle, hors gate
canonique et separee des resultats deterministes.

## 8. Strategie de tests

Tests unitaires : validation, estimation, filtres, deduplication, selection,
raisons d'exclusion et rendu.

Tests de proprietes : permutation des candidats, budget jamais depasse,
provenance jamais perdue, score non fini sans panic et bornes de taille.

Tests d'integration : store natif, recall hybride, isolation, invalidation,
expiration et futures surfaces publiques.

Chaque milestone passe ses tests cibles puis :

```bash
cargo xtask check
```

Les benchmarks ne deviennent publics qu'avec corpus, environnement,
configuration et rapport reproductible.

## 9. Premier lot executable

Etat : implemente et valide par `cargo xtask check` et `cargo xtask test`.

Le premier lot vertical comprend :

1. types `ContextRequest` et `ContextBundle` ;
2. `TokenEstimator` local et injectable ;
3. collecte depuis `recall_hybrid_with_options` ;
4. filtres de provenance compatibles avec ADR-036 ;
5. deduplication exacte avec union des IDs ;
6. selection simple sous budget ;
7. rendu Markdown avec citations ;
8. tests de budget, deduplication, provenance et determinisme.

Les faits obsoletes complexes, profils, CLI/MCP/REST, UI et suite JSONL restent
ouverts apres validation de ce contrat vertical. Les bindings Python et Node
sont livres.

## 10. Definition of Done V1

- `compile_context` est public et documente ;
- le resultat est deterministe et ne depasse jamais le budget estime ;
- provenance, deduplication et citations sont conservees ;
- les exclusions sont observables ;
- toutes les surfaces ont un contrat equivalent ;
- les dix familles de tests qualite existent ;
- la CI bloque les regressions critiques ;
- un rapport compare recall brut, hybride et Context Engine ;
- la documentation distingue fonctions livrees, experimentales et futures.
