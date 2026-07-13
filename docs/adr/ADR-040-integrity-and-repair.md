# ADR-040 — Intégrité physique, intégrité logique et modèle de réparation

**Statut** : ✅ Accepted
**Date** : 2026-07-11
**Relation aux ADR existants** : s'appuie sur le format posé par ADR-025
(LSM : WAL/memtable/SST), ADR-030 (AEAD, `crypto.meta`) et ADR-039 (SST par
blocs, `store.meta`, AEAD par section). N'amende aucun format : cet ADR ne
change **rien** sur disque (`format.lock` intact) — il définit comment on
*vérifie*, *diagnostique* et *répare* ce qui existe. C'est le jalon **N9**
du programme production-hardening (`docs/PLAN-NATIVE-ENGINE.md` §6).

## Contexte

Le moteur détecte déjà la corruption **au moment où il la rencontre** :
checksums crc32 par section, AEAD par bloc, cross-checks header/footer/index
à l'ouverture, vérification lazy des data blocks à la première lecture
(N8.4). Mais il n'existe aucun moyen :

- d'auditer un store **entier** à la demande (« ce fichier `.bmai` est-il
  sain ? ») sans attendre qu'une lecture tombe dessus ;
- de distinguer, pour un opérateur, « données primaires perdues » de
  « index dérivé recalculable » ;
- de réparer quoi que ce soit autrement qu'en supprimant le store.

Un moteur qui prétend garder la mémoire d'un agent pendant des années doit
pouvoir répondre à « est-ce sain ? » par un diagnostic précis, et à « c'est
cassé » par un plan de réparation déterministe — jamais par un faux succès.

## Décision

### 1. Classification des données

Toute clé du store appartient à une des trois catégories, et la politique
de réparation en découle mécaniquement :

| Catégorie | Contenu | Politique |
| --- | --- | --- |
| **Primaires** | records mémoire, entités/arêtes du graphe, métadonnées métier, contrat du modèle d'embedding | **Jamais** réécrites par une réparation automatique. Perte partielle ⇒ déclaration explicite « données primaires irrécupérables » (avec la liste exacte), jamais une reconstruction silencieuse. |
| **Dérivées** | postings FTS, stats BM25, `vecmap` inverse, connectivité ANN (voisins DiskANN), caches, bloom filters | Intégralement **recalculables** depuis les primaires. Une corruption ici est toujours réparable par rebuild. Un index vectoriel exigeant un ré-embedding est reconstruit côté `basemyai` (seul endroit où un `Embedder` existe — l'engine n'embarque jamais de modèle, ADR-010). |
| **De contrôle** | `store.meta`, `crypto.meta`, compteurs/epochs (`next_vec_id`…), séquences WAL | Cas par cas : `crypto.meta` est irremplaçable (perdre la DEK = perdre le store — posture ADR-030 assumée) ; les compteurs sont re-dérivables des données (le moteur le fait déjà : `next_vec_id` guéri depuis node ∪ vecmap, ADR-027 §4). |

### 2. API de vérification

```rust
#[non_exhaustive]
pub enum VerifyMode { Quick, FullPhysical, FullLogical }

pub struct VerifyReport {
    pub healthy: bool,            // == errors.is_empty()
    pub files_checked: u64,
    pub blocks_checked: u64,
    pub records_checked: u64,
    pub errors: Vec<IntegrityIssue>,
    pub warnings: Vec<IntegrityIssue>,
}

pub struct IntegrityIssue {
    pub kind: IssueKind,          // enum typée #[non_exhaustive]
    pub path: PathBuf,            // fichier concerné
    pub detail: String,           // diagnostic exact, phrase complète
}
```

- **`Quick`** — O(métadonnées), le même budget d'I/O qu'un `open` : magies,
  versions, checksums et cross-checks de `store.meta`, `crypto.meta`,
  header/footer/index/bloom de chaque SST, bornes et contiguïté des offsets
  de blocs, ordre des clés **entre** blocs (via l'index), scan structurel
  read-only du WAL. Ne décode **aucun** data block — une corruption de
  payload est donc invisible en `Quick`, par construction et documenté tel
  quel.
- **`FullPhysical`** — `Quick` + décodage de chaque data block (crc32/AEAD),
  ordre strict des clés **dans** chaque bloc, cross-check
  first/last/entry_count/tombstone_count bloc ↔ index, aucun faux négatif
  bloom sur l'ensemble réel des clés.
- **`FullLogical`** — `FullPhysical` + cohérence inter-structures :
  record ↔ `vec_id`, `vecmap` ↔ record, `docterms` FTS ↔ postings, stats
  BM25 recalculées et comparées, voisins DiskANN pointant sur des nœuds
  existants, tombstones cohérents, entités/arêtes du graphe référentiellement
  intègres, isolation par agent (aucune clé d'un agent ne référence les
  données d'un autre), métadonnées d'embedding cohérentes.

Règles invariantes, quelle que soit la mode :

1. **`verify` ne modifie jamais le store.** Pas même la troncature de queue
   WAL déchirée que `open` s'autorise : le scan WAL de vérification est
   strictement read-only et rapporte la queue déchirée comme *warning*
   (c'est l'état attendu après un crash, pas une erreur).
2. **Jamais de faux succès.** Toute anomalie détectable est soit une
   erreur (`healthy: false`), soit un warning explicite — jamais avalée.
   Réciproquement une queue WAL déchirée ou un orphelin `*.tmp` (états
   normaux post-crash, que `open` gère) sont des warnings, pas des erreurs :
   crier au loup sur un état sain serait l'autre forme de faux diagnostic.
3. **Diagnostic typé.** Chaque anomalie porte une `IssueKind` stable
   (testable par `match`) plus le détail texte exact — jamais un booléen
   « corrompu quelque part ».
4. Une clé de chiffrement **invalide** n'est pas un problème d'intégrité :
   `WrongEncryptionKey`/`MissingEncryptionKey` restent des erreurs *de
   l'appel* (`Err`), pas des entrées du rapport — on ne peut rien vérifier
   sans la DEK, et l'ambiguïté « mauvaise clé vs corruption » est déjà levée
   par `crypto.meta` (ADR-030).

### 3. Modèle de réparation

```bash
basemyai verify agent.bmai              # Quick
basemyai verify agent.bmai --deep       # FullPhysical (+ --logical plus tard)
basemyai compact agent.bmai             # compaction forcée, opérable
basemyai repair agent.bmai --dry-run    # plan détaillé, aucune écriture
basemyai rebuild-indexes agent.bmai     # reconstruit toutes les données dérivées
```

- `repair --dry-run` produit le **plan** exact (quelle structure, quelle
  action, quelles données primaires sont intactes/perdues) sans écrire un
  octet.
- Une réparation réelle construit le nouvel état **à côté** de l'ancien
  (répertoire/fichiers `*.tmp`), fsync, puis publication par **rename
  atomique** — le même pipeline que chaque écriture du moteur depuis
  ADR-025. Un crash au milieu d'une réparation laisse soit l'ancien état,
  soit le nouveau, jamais un hybride.
- **Aucun écrasement destructif sans backup explicite** : l'ancien état
  n'est supprimé qu'après publication réussie, et `repair` sans `--force`
  refuse de toucher un store dont des données primaires sont en jeu.
- `rebuild-indexes` ne lit que des primaires et ne réécrit que des dérivées
  (catégorie §1) — c'est l'opération toujours-sûre. Le cas particulier du
  ré-embedding (le contrat du modèle a changé ou les vecteurs sont perdus)
  vit côté `basemyai`, pas dans l'engine.

### 4. Tests adversariaux (contrat de N9, prolongé en N11)

Pour chaque structure persistée : partir d'un store valide, appliquer une
mutation adversariale (octet modifié, bloc supprimé, bloc dupliqué, offset
modifié, record orphelin), puis exiger : `verify` produit le diagnostic
typé exact ; une réparation autorisée ne touche pas les données primaires ;
un store non réparable est déclaré tel quel. Le fuzzing systématique des
décodeurs reste le mandat de N11 (ADR-039 en a posé la discipline).

## Phasage (sous-étapes N9)

| Étape | Contenu | Lieu |
| --- | --- | --- |
| N9.1 | Cet ADR | docs |
| N9.2 | `verify_store` moteur : `Quick` + `FullPhysical`, scan WAL read-only, tests adversariaux | `basemyai-engine` |
| N9.3 | `FullLogical` (cohérence record/vecmap/FTS/graphe/vecteur par agent) | `basemyai-engine` (+ isolation côté `basemyai`) |
| N9.4 | Compaction opérable (`compact` public, pas seulement par seuil) | `basemyai-engine` |
| N9.5 | `repair --dry-run` + `rebuild-indexes` (dérivées uniquement) | `basemyai-engine` + `basemyai` |
| N9.6 | Surface CLI `verify`/`compact`/`repair`/`rebuild-indexes` | `basemyai-cli` |

`VerifyMode` est `#[non_exhaustive]` précisément pour que N9.2 puisse
livrer `Quick`/`FullPhysical` sans exposer un `FullLogical` menteur avant
N9.3.

## Conséquences

- (+) Un opérateur peut auditer un store sans le modifier, avec un
  diagnostic exact par structure — le critère de sortie N9 (« jamais de
  faux succès ») devient testable mécaniquement.
- (+) La frontière primaires/dérivées force chaque future structure à
  déclarer sa catégorie — et donc sa politique de réparation — dès sa
  conception (N10 : index temporel = dérivée ; registre d'agents = dérivée).
- (−) `FullLogical` recalcule des structures entières (stats BM25,
  connectivité) : coût O(données), assumé — c'est un audit, pas un health
  check de routine (`Quick` existe pour ça).
- (−) Deux chemins de lecture WAL (replay-avec-troncature à l'open, scan
  read-only au verify) : mutualisés sur le même décodeur pour ne pas
  diverger.
