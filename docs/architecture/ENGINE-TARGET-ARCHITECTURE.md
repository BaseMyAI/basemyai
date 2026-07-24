# ENGINE-TARGET-ARCHITECTURE — Architecture cible du moteur natif BaseMyAI

**Statut** : Proposé (document de conception — aucun code modifié)
**Date** : 2026-07-19
**Base** : audit `docs/audits/2026-07-engine-architecture-safety-audit.md`
(HEAD `0e742f8`), code réel de `crates/basemyai-engine` et
`crates/basemyai/src/storage/native_store`.
**Décision produit actée** : BaseMyAI fournit un **vrai snapshot
point-in-time du moteur** (S2/S3), pas seulement un snapshot structurel des
SST. Mono-writer logique conservé. Pas de transaction d'écriture, pas de
distribution, pas d'inter-processus.

Ce document remplace, en tant que cible, le brouillon non committé
`docs/adr/ADR-043-native-version-set-snapshots-and-concurrent-compaction.md`
(qui décrivait une cible S1 — voir §18 pour la démonstration que S1 ne peut
pas être la cible finale, et §21 pour le redécoupage ADR qui le supersède).

---

## 1. Décision exécutive

L'architecture cible est le **plus petit modèle complet** qui fournit
durablement les huit propriétés exigées. Elle tient en six mécanismes, tous
définitifs, aucun jetable :

1. **Séquences globales** (`SequenceNumber = u64`) : allouées par
   sous-opération, contiguës par batch, portées par le WAL, publiées
   tout-ou-rien par batch (`visible_sequence`).
2. **Clés internes versionnées** dans memtable et SST :
   `(user_key, sequence, kind)` — plusieurs versions d'une même clé
   coexistent tant qu'un snapshot peut les voir.
3. **`ReadSnapshot` = `(visible_sequence, Arc<SuperVersion>)`** où
   `SuperVersion = (memtable mutable, memtables scellées, Version des SST)`
   — répétabilité des lectures **sans copier la memtable** (elle est
   insert-only et versionnée : filtrer par séquence suffit).
4. **Catalogue durable de l'état publié** (`catalog.meta`, un par
   génération crypto) : SST vivantes, segments WAL vivants,
   `last_published_sequence`, `next_file_id`. Publié par
   tmp → fsync → rename → **sync du répertoire**. La suppression physique
   n'est jamais un acte de publication.
5. **WAL segmenté** : un segment par memtable, retiré par le catalogue quand
   sa memtable est durablement flushée — jamais tronqué en place.
6. **Pipelines de fond** (flush, compaction) publiant par **`VersionEdit`
   appliqué au Version courant au moment du commit**, avec retrait différé
   des fichiers (référence-compté) et GC consciente des snapshots.

**Recommandation finale : T2 — construire directement la fondation S2/S3**
(§22). Il n'existe pas d'étape S1 publique : les formats cibles (séquences,
WAL segmenté, SST versionnées, catalogue) sont posés en un seul bump, puis
les mécanismes s'activent par jalons dont chacun est un sous-ensemble
définitif de la cible (§20).

Le format natif étant expérimental, **un seul bump de format** couvre tout :
`STORE_FORMAT_VERSION 2 → 3`, `WalRecord:3`, `WalEnvelope:2`,
`SstHeader:2`/`SstDataBlock:2`/`SstBlockIndex:3`, `Catalog:1`. Les stores
antérieurs sont rejetés proprement (gate existant) ; migration one-shot par
export/import JSONL déjà livré (`porting.rs`).

---

## 2. Exigences produit

Reprises de la décision produit, traduites en exigences testables :

- **E1 — Vue logique unique** : une opération composée (recall hybride =
  ranking vectoriel + BM25 + hydratation + touch ; `compile_context` ;
  export ; verify logique ; scans paginés longs) lit un état unique à
  travers records, vecmap, index vectoriel, FTS, graphe, index temporels et
  registre d'agents.
- **E2 — Stabilité sous mutation** : écritures, seals, flushes, compactions
  et rotations postérieurs à la création du snapshot n'altèrent pas ce qu'il
  observe.
- **E3 — Publication durable** : l'ensemble des fichiers vivants et la
  frontière de séquence publiée sont des faits durables indépendants du
  contenu du répertoire.
- **E4 — Récupération déterministe** : après tout crash, l'ouverture
  reconstruit exactement l'état publié + la queue WAL rejouable, ou échoue
  typé — jamais une réalité re-devinée depuis le listage.
- **E5 — Lectures jamais bloquées par le fond** : flush et compaction ne
  retiennent le verrou d'écriture que pour leurs bascules O(1).
- **E6 — Ressources bornées** : memtable en octets, batches en octets,
  valeurs bornées, scans paginés, merge streamé.
- **E7 — Extensible sans refonte** : changefeed (N15) = lecture des
  séquences déjà présentes ; group commit (si mesuré) = regroupement des
  fsyncs sans changer séquences/WAL/snapshot/catalogue.
- **E8 — Hors périmètre** : multi-writer, transactions d'écriture,
  snapshots persistants après fermeture, lecture inter-processus.

---

## 3. Invariants normatifs

Numérotés pour référence par les ADR et les tests (chaque invariant doit
avoir au moins un test ou failpoint qui tente de le violer) :

- **I1 (ordre total)** : toute mutation acceptée porte une
  `SequenceNumber` unique ; l'ordre des séquences est l'ordre logique des
  mutations ; dans un batch, les sous-opérations portent des séquences
  contiguës croissantes dans l'ordre de staging.
- **I2 (visibilité tout-ou-rien)** : `visible_sequence` n'avance que d'un
  batch entier à la fois ; aucune lecture n'observe un préfixe strict d'un
  batch.
- **I3 (durabilité avant visibilité)** : une séquence n'est visible que si
  son enregistrement WAL est fsyncé. (L'inverse est permis : durable mais
  jamais devenu visible = brûlée, cf. §6.)
- **I4 (catalogue = vérité)** : un fichier de données (SST, segment WAL)
  est vivant **si et seulement si** le catalogue courant le liste. Présent
  hors catalogue = orphelin librement supprimable ; listé mais absent =
  corruption typée. `open` ne lit *jamais* une donnée hors catalogue.
- **I5 (publication durable avant visibilité RAM)** : aucune nouvelle vue
  mémoire (Version/SuperVersion) n'est installée avant que le catalogue qui
  la décrit soit durable (fichier **et** entrée de répertoire).
- **I6 (rétention par snapshot)** : une version de clé, un fichier SST ou
  un segment WAL référencé par un `ReadSnapshot` actif n'est ni supprimé ni
  réécrit tant que ce snapshot vit. Corollaire GC : une version n'est
  purgeable que si aucun snapshot actif ne peut la distinguer d'une version
  plus récente (§13).
- **I7 (récence par couche, par clé)** : pour toute clé utilisateur, toute
  source plus récente dans l'ordre du catalogue ne détient que des versions
  strictement plus récentes que toute source plus ancienne. (Maintenu par :
  flush en ordre de scellement, sortie de compaction prenant la position de
  son entrée la plus ancienne. C'est l'invariant qui rend la recherche
  newest-first correcte ; une future stratégie par niveaux le reformule par
  niveau sans changer la sémantique.)
- **I8 (WAL couvrant)** : à tout instant, l'union des segments WAL vivants
  contient toutes les mutations visibles non encore couvertes par une SST
  vivante. Un segment n'est retiré du catalogue qu'après que le flush de sa
  memtable est publié durablement (I5).
- **I9 (ids non réutilisés)** : `next_file_id` est monotone, persisté dans
  le catalogue, jamais recalculé depuis le répertoire.
- **I10 (générations)** : un pointeur de génération absent alors qu'un
  répertoire `gen-N` existe est une erreur typée — jamais interprété comme
  « génération 0 », jamais suivi d'une GC.
- **I11 (suppression ≠ publication)** : aucune transition logique ne dépend
  du succès d'une suppression physique ; tout unlink est best-effort,
  compté (`orphan_bytes`), ré-essayé.
- **I12 (mono-writer logique)** : un seul flux d'allocation de séquences et
  d'écriture WAL par store ouvert (le verrou advisory inter-processus
  existant reste tel quel).

---

## 4. Architecture mémoire (vue d'ensemble)

```text
                     Engine (cible)
┌─────────────────────────────────────────────────────────────┐
│ write path (sérialisé, sections critiques courtes)          │
│   seq_allocator: AtomicU64 (last_allocated)                 │
│   visible_sequence: AtomicU64                               │
│   wal_writer: segment courant (file_id, handle, crypto)     │
│                                                             │
│ current: ArcSwap-like<Arc<SuperVersion>>  ← install O(1)    │
│   SuperVersion {                                            │
│     mutable:   Arc<Memtable>      (insert-only, versionnée) │
│     immutable: Arc<[Arc<Memtable>]>  (0..N scellées, N=1)   │
│     version:   Arc<Version>       (SST vivantes, figées)    │
│   }                                                         │
│                                                             │
│ snapshots: registre trié des visible_sequence actifs        │
│   → oldest_active_snapshot_sequence()                       │
│                                                             │
│ commit_lock: sérialise les commits de catalogue (§11)       │
│ retirement:  file de fichiers retirés (Arc-compté) (§14)    │
│ flush_worker / compaction_worker + background_error (§12-13)│
└─────────────────────────────────────────────────────────────┘
```

Verrouillage cible : le gros `RwLock<NativeInner>` du produit cesse d'être
le mécanisme de cohérence des lectures (il le reste pour sérialiser les
écritures produit, ce qui est le rôle I12). Les lectures prennent un
`ReadSnapshot` (une acquisition brève) puis travaillent sans verrou sur des
structures figées — la memtable mutable étant insert-only + versionnée, sa
lecture concurrente est protégée par un verrou court interne au moteur
(§10), remplaçable par une skiplist si la mesure l'exige un jour (pas
maintenant — pas de lock-free spéculatif).

## 5. Architecture disque (vue d'ensemble)

```text
<root>/
  .basemyai.lock        verrou writer advisory (inchangé)
  store.meta            gate de format (STORE_FORMAT_VERSION = 3)
  generation.meta       pointeur de génération crypto (ADR-042, + I10)
  [gen-<g>/]            (racine = génération 0, comme aujourd'hui)
    crypto.meta         enveloppe DEK (inchangé)
    catalog.meta        ★ NOUVEAU — état publié (§9)
    <file_id>.sst       SST versionnées (§5.1)
    <file_id>.wal       ★ segments WAL (§8) — plus de wal.log unique
    *.tmp               transitoires, jamais vivants
```

Espace d'ids **unifié** : `next_file_id` du catalogue nomme SST et segments
WAL (`7.sst`, `9.wal`) — une seule source d'allocation, aucune collision,
aucune inférence depuis le répertoire (I9).

---

## 6. Séquences et visibilité

```rust
pub type SequenceNumber = u64; // 0 = « aucune mutation », première = 1
```

**Attribution : par sous-opération, contiguë par batch.** Un batch de `n`
ops réserve `[base, base+n)` ; l'op `i` porte `base+i`. Un `put`/`delete`
isolé est un batch de 1. Justification contre les deux alternatives :

- *Par batch seul* : ne donne pas d'ordre interne — deux mutations de la
  même clé dans un batch seraient ambiguës dans une memtable versionnée
  (l'actuel « dernier gagne » repose sur l'ordre d'application in-place,
  qui disparaît). L'ordinal interne est donc obligatoire de toute façon ;
  le fusionner dans la séquence (RocksDB fait exactement ce choix) évite un
  deuxième champ dans les clés internes et les SST.
- *Séquence de batch + ordinal séparé* : deux champs partout (clé interne,
  WAL, SST, comparateurs) pour la même information que `base+i`. Rejeté.

**Points du cycle de vie** :

- **Allocation** : dans la section critique du write path, avant l'append
  WAL : `base = last_allocated + 1 ; last_allocated += n`. (Atomique simple
  sous I12 ; prêt pour un leader de group commit qui réserverait une plage.)
- **Durabilité** : le `sync_all` du segment WAL — l'enregistrement WAL
  **porte** `base` (et `n` implicite par son contenu) : le replay ne
  ré-alloue jamais, il relit (déterminisme, E4).
- **Visibilité** : après insertion des `n` versions dans la memtable,
  `visible_sequence.store(base + n - 1)` — une seule publication par batch
  (I2). Entre durabilité et visibilité, aucune lecture ne voit le batch
  (I3) ; un crash dans cette fenêtre le fait rejouer au prochain open —
  correct, il était durable.
- **Récupération de `last_sequence`** :
  `max(catalog.last_published_sequence, max(base+n-1) des enregistrements
  rejoués des segments vivants)`. Les deux bornes sont nécessaires : le
  catalogue couvre ce que les SST persistent (un WAL retiré n'est plus là
  pour témoigner), le replay couvre la queue non flushée.
- **Écriture échouée après réservation** : la plage est **brûlée** — la
  memtable n'est pas touchée, `visible_sequence` n'avance pas, l'erreur
  remonte. Les trous de séquence sont légaux partout (visibilité = `seq ≤
  visible_sequence` ; une séquence brûlée n'existe dans aucune structure).
  Un successeur qui réussit publie au-delà du trou. Le WAL peut contenir un
  enregistrement durable dont l'appelant a reçu une erreur (fsync réussi
  puis erreur de canal) : au replay il redevient visible — c'est
  l'ambiguïté « erreur après commit » documentée, inchangée par ce design.
- **Overflow** : `checked_add` ; à saturation (~5,8 × 10¹⁸ mutations),
  erreur typée `SequenceSpaceExhausted`, store en lecture seule. Pas de
  wrap, pas de ré-époque : irréaliste d'ici là, mais l'échec doit être
  typé, pas UB.
- **Changefeed (E7)** : `changes_since(seq)` = lecture des versions
  `> seq` — les séquences par sous-opération et `last_published_sequence`
  au catalogue sont exactement le curseur qu'il faut ; rien à refondre.

## 7. Clés internes versionnées

```rust
struct InternalKey {
    user_key: Key,            // clé utilisateur actuelle, encodage inchangé
    sequence: SequenceNumber,
    kind: ValueKind,          // Value | Tombstone
}
```

**Ordre (physique en SST, logique en memtable)** : `user_key` ascendant,
puis `sequence` **descendant**. `kind` n'est pas discriminant : une
`(user_key, sequence)` est unique (I1), le champ est porté, pas comparé.
Encodage SST : suffixe 9 octets (`u64` séquence en complément inversé pour
trier descendant en ordre d'octets + 1 octet kind) — les comparateurs de
blocs restent des comparaisons d'octets.

**Sémantique de lecture** :

- `get(snapshot, k)` : dans chaque source (ordre newest-first du
  catalogue), chercher la première entrée `(k, s)` avec
  `s ≤ snapshot.visible_sequence` (un `seek` à `(k, snapshot.seq)` — même
  recherche binaire qu'aujourd'hui, sur clé interne). Premier résultat
  trouvé dans l'ordre des sources = la bonne version (I7 garantit qu'une
  source plus ancienne ne peut pas détenir une version plus récente de
  `k`). `kind = Tombstone` ⇒ `None`, arrêt. **Attention (différence avec
  aujourd'hui)** : une source qui détient `k` uniquement en versions
  `> snapshot.seq` ne termine pas la recherche — on continue vers les
  sources plus anciennes.
- **Scan** : itérateur fusionné (§10/§13) ; pour chaque `user_key`, la
  première version rencontrée avec `s ≤ snapshot.seq` est émise (ou
  supprimée si tombstone), les suivantes du même `user_key` sont sautées.
  Un tombstone visible masque toutes les versions antérieures — identique
  au modèle actuel vu du consommateur.
- **Batch multi-mutations d'une même clé** : séquences contiguës croissantes
  dans l'ordre de staging ⇒ la dernière op du batch a la plus grande
  séquence ⇒ « later ops win » (contrat documenté de `Batch::put`,
  engine.rs:65-67) préservé mécaniquement.
- **Last-write-wins préservé** : une lecture sans snapshot explicite lit à
  `visible_sequence` courant ⇒ toujours la version la plus récente — le
  comportement observable actuel est le cas particulier
  `snapshot = latest`.
- **Index consommateurs inchangés** : `idx/*` continue d'encoder ses clés
  utilisateur (`key::…`) ; le versionnement est un suffixe interne du
  moteur, invisible aux encodeurs de clés. Bloom : **clé utilisateur
  seule** (un point-lookup teste l'existence de la clé, pas d'une version).

**Changements de format induits** (tous dans le bump unique §17) :

| Structure | Changement |
|---|---|
| WAL (`WalRecord:3`) | + `base_sequence: u64` par enregistrement (op simple et batch) |
| Enveloppe WAL (`WalEnvelope:2`) | AAD étendue au `file_id` du segment (anti-splicing entre segments — même logique que l'AAD par section des SST) |
| Memtable (RAM) | `BTreeMap<InternalKeyOrd, Option<Value>>` insert-only |
| SST data block (`SstDataBlock:2`) | entrées = clés internes (`+ seq, + kind`) ; les tombstones existent déjà dans le format (Option) — `kind` les remplace |
| SST block index (`SstBlockIndex:3`) | bornes first/last en clés internes ; `partition_point` inchangé (octets) |
| SST header (`SstHeader:2`) | + `min_sequence`, `max_sequence` (auto-description ; croisés avec `SstMeta` du catalogue par verify) |
| Bloom (`SstBloomFilter:1`) | inchangé |
| Scans | itérateurs par version, dédup par user_key sous le snapshot |
| Compaction | GC par visibilité (§13) au lieu de « dernier écrase » |
| Verify | vérifie l'ordre des clés *internes*, croise min/max seq header ↔ catalogue, et : plusieurs versions d'une clé n'est plus une anomalie |
| Fuzz | cibles décodeurs re-générées pour les nouveaux codecs (mêmes harnais) ; + cible « comparateur de clés internes » (ordre total, anti-symétrie) |

## 8. WAL — segments par memtable (Option B, motivée)

**Décision : Option B** — un segment `<file_id>.wal` par memtable ; le seal
d'une memtable ouvre un nouveau segment ; un segment est retiré du
catalogue quand la SST issue de sa memtable est publiée (I8). Jamais de
troncature en place.

Pourquoi pas l'Option A (WAL unique + checkpoints) : (a) la troncature en
place détruit l'historique dont le changefeed N15 aura besoin comme unité
de rétention ; (b) « offset de checkpoint dans le catalogue » couple le
catalogue au contenu interne d'un fichier — un id de segment vivant est un
fait plus simple à publier et à vérifier ; (c) plusieurs memtables
scellées (N>1 futur) imposeraient des fenêtres [offset, offset) multiples —
le segment les donne gratuitement ; (d) l'invariant de détection exigé
(« un WAL attendu mais manquant doit se voir ») devient trivial :
`live_wals` du catalogue vs disque (I4) — impossible avec un `wal.log`
« vivant parce qu'il existe ». Pas d'Option C plus simple identifiée qui
satisfasse E7 + I8 + détection d'absence.

Cycle de vie d'un segment :

```text
créé (fsync fichier + sync_dir) et publié dans le catalogue AU SEAL qui
  l'ouvre (le commit de seal publie : nouveau segment vivant — §10)
  [exception d'amorçage : le tout premier segment d'un store est publié
   par le commit de création du store]
→ append + fsync par batch (durabilité I3)
→ sa memtable est scellée (le segment ne reçoit plus d'appends)
→ flush de la memtable publié (catalogue : + SST, − segment)   [I8, I5]
→ fichier retiré via la file de retrait (§14) — supprimé quand plus
  aucun snapshot/Version ne le référence (aujourd'hui : rien ne référence
  un WAL après flush ; le changefeed N15 introduira une rétention par
  curseur — le mécanisme de retrait est déjà le bon)
```

Récupération : replay des `live_wals` dans l'ordre croissant de
`start_sequence` ; chaque segment reconstruit sa memtable (la dernière
redevient mutable, les précédentes re-scellées et re-flushables). Queue
déchirée tolérée **sur le dernier segment seulement** — un segment non
terminal tronqué est une corruption typée (il a été scellé complet).

Compatibilité future : group commit = plusieurs batches par `sync_all` du
segment courant (rien ne change au format) ; changefeed = itération des
segments par plage de séquences (le catalogue les liste avec
`start_sequence`).

## 9. Catalogue durable de l'état publié

```rust
struct CatalogState {                 // catalog.meta — Catalog:1
    store_generation: u64,            // == génération crypto du répertoire
                                      // (croisé avec generation.meta/crypto.meta)
    catalog_generation: u64,          // monotone, +1 par commit
    last_published_sequence: SequenceNumber, // max seq couverte par les SST
    next_file_id: u64,                // allocation unifiée SST+WAL (I9)
    live_ssts: Vec<SstMeta>,          // ordre = ordre de couche (I7)
    live_wals: Vec<WalMeta>,          // ordre = start_sequence croissant
}
struct SstMeta { file_id: u64, min_sequence: u64, max_sequence: u64,
                 file_bytes: u64 }    // bornes clés min/max : non requises
                                      // par le full-merge ; ajout additif si
                                      // une stratégie par niveaux arrive
struct WalMeta { file_id: u64, start_sequence: SequenceNumber }
```

**Définitions (ferment I4)** :

- *SST vivante* : listée dans `live_ssts` du catalogue courant.
- *WAL vivant* : listé dans `live_wals` — attendu au replay ; absent =
  `MissingLiveWal` (corruption typée), **pas** « WAL vide ».
- *Orphelin* : fichier `*.sst`/`*.wal`/`*.tmp` présent hors catalogue —
  supprimé best-effort à l'open **avant** tout usage, compté
  (`orphan_bytes`), jamais lu.
- *Corruption* : entrée du catalogue sans fichier (`MissingLiveSst`/
  `MissingLiveWal`), catalogue indécodable, `store_generation` du catalogue
  ≠ génération du répertoire, ou store au format 3 sans `catalog.meta`.
- *Allocation* : `next_file_id` lu du catalogue, incrémenté en RAM,
  persisté par le commit qui publie le fichier. Un crash entre création
  d'un fichier `<id>` et son commit laisse un orphelin ; l'id est
  ré-allouable **uniquement parce que** l'orphelin est supprimé avant tout
  usage à l'open (ordre imposé : GC des orphelins → puis seulement lecture
  des vivants). Aucune inférence `max(dir)+1` ne subsiste nulle part.

**Publication** (I5) : `catalog.meta.tmp` → `write_all` → `sync_all` →
`rename` → `sync_dir` (fsync du répertoire parent, no-op Windows). Le
fichier est un **snapshot complet réécrit à chaque commit** — quelques
centaines d'octets pour ≤ ~10 fichiers vivants ; un journal append-only
type LevelDB MANIFEST est explicitement rejeté à cette échelle (machinerie
replay/compaction-du-manifest/CURRENT sans bénéfice mesurable). Le
`VersionEdit` (§11) est le protocole *mémoire* de mutation, pas le format
fichier.

**Interactions** : `store.meta` reste le gate de format (version 3) ;
`generation.meta` reste le pointeur de génération crypto (avec I10) ;
`crypto.meta` inchangé ; `catalog.meta` vit **dans** le répertoire de
génération et porte `store_generation` — une rotation complète construit le
catalogue de la nouvelle génération avant de publier le pointeur (§16).
Migration : format 3 exigé ; stores 2 rejetés typé (`UnsupportedStoreFormat`
existant) ; chemin de migration = export JSONL (build actuel) → import
(nouveau build) — outil déjà livré, documenté comme procédure.

## 10. Memtables

```text
1 memtable mutable + 0..N scellées — N = 1 initialement, le modèle (liste
dans SuperVersion, WAL segmenté, catalogue) supporte N > 1 sans changement
de format le jour où la mesure le justifie.
```

- **Structure** : table triée par clé interne, **insert-only** (jamais de
  remplacement in-place, jamais de retrait) : `put`/`delete` *ajoutent* une
  version `(k, seq, kind)`. C'est ce qui rend le snapshot gratuit — pas de
  copie, pas de gel : un lecteur à `visible_sequence = s` filtre `seq ≤ s`
  et ne peut par construction pas voir les insertions postérieures.
  Concurrence : accès sous un `RwLock` interne au moteur à sections
  courtes (une insertion / un seek) ; pas de skiplist lock-free tant que
  non mesurée nécessaire.
- **Seuils** : octets (primaire — somme clés internes + valeurs,
  `memtable_target_bytes`, défaut à mesurer, ordre 8 Mio) **et** entrées
  (garde-fou hérité). Dépassement ⇒ seal. Une valeur unique dépassant les
  bornes de §17-E6 est refusée typée avant réservation de séquence.
- **Seal (atomique, section critique courte)** : la mutable devient
  scellée (aucune écriture ne l'atteint plus — le write path ne connaît
  que la nouvelle), une mutable vide + un nouveau segment WAL sont créés,
  un `VersionEdit{add_wals}` est commité, une nouvelle `SuperVersion` est
  installée. Visibilité : avant le seal, versions lues dans la mutable ;
  après, les mêmes versions lues dans la scellée — aucune fenêtre où elles
  manquent (l'installation de SuperVersion est le point de bascule
  atomique).
- **Backpressure** : si N scellées non flushées existent déjà au moment où
  la mutable atteint son seuil, le write path **attend** (stall instrumenté
  `write_stall_micros`) — c'est le mécanisme de borne mémoire, explicite au
  lieu de l'actuel blocage-par-verrou.
- **Durée de vie** : `Arc<Memtable>` détenu par les SuperVersions qui la
  listent et par le flush worker ; libérée quand le flush est publié **et**
  que le dernier snapshot la référençant est tombé.
- **Crash** : les memtables ne sont jamais un état durable ; chaque segment
  WAL vivant les reconstruit (§8, §15).
- **Anciennes versions** : jamais purgées en RAM ; la purge se fait au
  flush (le flush applique la règle de visibilité §13 avec le registre de
  snapshots — les versions qu'aucun snapshot ne peut distinguer ne sont
  pas écrites dans la SST). Cas dimensionnant réel : `touch_last_access`
  ré-écrit le record à chaque recall — sans purge au flush, un hot record
  accumulerait ses versions dans chaque SST.

## 11. VersionSet et VersionEdit

```rust
struct Version {                      // ensemble de SST figé
    catalog_generation: u64,
    ssts: Arc<[Arc<SstHandle>]>,      // ordre de couche (I7)
}
struct VersionEdit {                  // mutation en mémoire, protocole de commit
    add_ssts: Vec<SstMeta>,           // + position d'insertion (couche)
    delete_sst_ids: Vec<u64>,
    add_wals: Vec<WalMeta>,
    delete_wal_ids: Vec<u64>,
    last_sequence: Option<SequenceNumber>,
    next_file_id: Option<u64>,
}
```

**Protocole de commit** (une seule voie pour seal/flush/compaction/
rotation ; `commit_lock` le sérialise) :

```text
1. (hors verrou) préparer : fichiers écrits, fsyncés, renommés, sync_dir
2. prendre commit_lock
3. VALIDER l'edit contre l'état COURANT (pas celui d'où le job est parti) :
   - tout delete_sst_id/delete_wal_id encore vivant ? sinon → conflit
   - store_generation inchangée ? sinon → conflit (rotation passée avant)
4. next_catalog = appliquer l'edit au CatalogState courant
5. écrire catalog.meta durablement (tmp/fsync/rename/sync_dir)   [I5 : D]
6. installer Version'/SuperVersion' (swap d'Arc)                  [V]
7. relâcher commit_lock ; passer les fichiers retirés à la file (§14)
```

- **Flush pendant compaction** : le flush commite son edit
  (`add_ssts=[F]`) pendant que la compaction merge ; au commit de la
  compaction, l'étape 4 applique `(courant ∖ inputs) ∪ {output}` — F,
  absent des inputs, **survit mécaniquement**. (C'est la correction du
  défaut ENG-COR-001 du brouillon S1, ici structurelle et non optionnelle.)
- **Rotation pendant compaction** : la rotation bumpe `store_generation` ;
  l'étape 3 du commit de compaction détecte le mismatch → **abandon** : la
  sortie spéculative est fermée puis passée à la file de retrait comme
  orpheline (elle n'a jamais été cataloguée — un crash avant ce nettoyage
  la fait ramasser par la GC d'open, I4).
- **Deux compactions** : une seule à la fois (queue bornée §12) — le cas
  « une autre a publié avant » se réduit au conflit d'inputs de l'étape 3,
  même issue : abandon propre. Le protocole est déjà correct pour des
  compactions partielles concurrentes futures (tiered) sans modification.
- **Échec à l'étape 5** : catalogue courant intact sur disque, rien
  installé en RAM — l'opération a échoué atomiquement ; les fichiers
  préparés deviennent des orphelins (ramassés à l'open ou par la file).
  Ambiguïté rename-réussi-erreur-ensuite : à la réouverture, l'un des deux
  catalogues est lu — les deux états sont sains (E4).

## 12. Flush en arrière-plan

```text
write pipeline    : write path → mutable → seal (edit: +wal) 
flush pipeline    : scellée → SST (écrite/fsync/rename/sync_dir)
                    → edit {add_ssts:[S], delete_wal_ids:[w]} → commit §11
                    → scellée libérée, segment w retiré (§14)
```

- **Worker** : un thread dédié détenu par l'Engine (crate sync — pas de
  dépendance tokio dans le moteur), queue bornée à N (=1) scellées.
- **Ce que le flush n'exige plus** : aucun verrou pendant la lecture de la
  scellée (figée par construction), l'écriture/fsync du fichier, ni la
  suppression. Sections critiques restantes : le seal (§10) et le commit
  (§11), tous deux O(1).
- **Panique / erreur permanente** : le worker capture ; l'Engine passe en
  `background_error` : les écritures nouvelles échouent typé
  (`BackgroundError { cause }`), les lectures et snapshots continuent
  (l'état publié est sain), `close()` tente un dernier flush synchrone.
  Une erreur transitoire (ENOSPC) est ré-essayée avec backoff borné avant
  de devenir permanente.
- **Fermeture** : `close()` = seal de la mutable si non vide → drain de la
  queue de flush → join des workers → dernier commit. `drop` sans close :
  workers joints, memtables perdues en RAM — le WAL les rejoue (contrat
  actuel conservé).
- **Annulation** : les workers ne sont pas des futures ; l'annulation
  n'existe pas à ce niveau (l'ambiguïté `spawn_blocking` reste une
  propriété de la *surface produit*, documentée — §19-A).

## 13. Compaction concurrente, consciente des snapshots

```text
compaction pipeline : inputs = Version figé (Arc) → merge STREAMÉ
                      (itérateurs par bloc, jamais BTreeMap du store)
                      → SST out (écrite/fsync/rename/sync_dir)
                      → edit {add_ssts:[out]@couche(min inputs),
                               delete_sst_ids: inputs} → commit §11
                      → inputs vers la file de retrait (§14)
```

**Règle de GC par visibilité.** Le registre des snapshots fournit la liste
triée des `visible_sequence` actifs (pas seulement le min — le registre
existe déjà pour `oldest_active_snapshot_sequence`, garder la liste est
gratuit et strictement plus précis). Règle exacte, par clé utilisateur,
versions parcourues newest → oldest :

> Une version `V(s)` est **supprimable** s'il existe une version plus
> récente `V'(s')` de la même clé dans le même passage de merge telle
> qu'aucun snapshot actif `p` ne satisfait `s ≤ p < s'` (aucun observateur
> ne peut distinguer l'état à `V` de l'état à `V'`).
> Un **tombstone** suit la même règle, avec en plus : il ne peut être
> *entièrement* retiré (plutôt que réécrit) que si le merge couvre toutes
> les couches où sa clé peut exister — vrai par construction pour le
> full-merge actuel ; à re-vérifier par niveau si une stratégie partielle
> arrive (la règle est formulée pour y survivre).

Exemple imposé — clé avec `seq 300 → Value(Pro)`, `seq 200 → Value(Free)`,
`seq 100 → Tombstone` (full-merge, latest = 300+) :

| Snapshots actifs (`p`) | Conservées | Pourquoi |
|---|---|---|
| aucun | 300 | seule la plus récente est observable |
| {350} | 300 | 350 voit 300 ; 200 et 100 indistinguables de 300 pour lui |
| {250} | 300, 200 | 250 voit 200 (≤250 la plus récente) ; 100 masquée pour tout observateur |
| {150} | 300, 200, 100 | 150 voit le tombstone (clé absente) — le supprimer ressusciterait la clé *dans le snapshot* ; 200 requis pour d'éventuels p∈[200,299] ? non — seul {150} est actif : 200 est supprimable (aucun p dans [200,300)) → **300, 100** |
| {150, 250} | 300, 200, 100 | 250 exige 200 ; 150 exige 100 |
| {50} | 300 | aucun p dans [100,200), [200,300) ⇒ 100 et 200 supprimables ; 50 ne voit rien (aucune version ≤ 50) |

(La ligne {150} illustre pourquoi la liste complète bat le seul
`oldest` : avec `oldest=150` seul connu, il faudrait conserver 200 par
prudence — la liste permet de la retirer.)

- **Sans snapshot actif** : dégénère exactement en la sémantique actuelle
  (dernier gagne, tombstones purgés au full-merge).
- **Snapshot long** : les versions et fichiers qu'il retient s'accumulent —
  space amplification bornée par ce que le snapshot référence.
  Instrumenté : `active_snapshots`, `oldest_snapshot_sequence`,
  `oldest_snapshot_age`, `retained_by_snapshots_bytes` (estimation). Une
  *alerte* de durée est fournie (log + métrique) ; aucune invalidation
  forcée — un snapshot est un contrat (E2), le casser serait pire que le
  coût qu'il mesure. Si un besoin de borne dure émerge, ce sera une
  décision produit séparée.
- **Full-merge conservé** comme stratégie initiale (sorti du verrou, il
  redevient acceptable) ; tiered/leveled = décision ultérieure **sur
  mesures** post-implémentation — la sémantique snapshot/GC ci-dessus est
  formulée pour n'avoir pas à changer (I6/I7 par niveau).

## 14. File lifecycle — publication logique ≠ suppression physique

```text
retiré du catalogue (commit §11)            [invisible aux nouvelles vues]
→ handle Arc<SstHandle>/segment encore détenu par : Versions vivants,
  snapshots, jobs en cours
→ dernier Arc tombe → push sur la file de retrait
→ deletion best-effort (worker de flush, opportuniste) :
    succès → fin ; échec → reste en file + orphan_bytes, ré-essai
→ à CHAQUE open : disque ∖ catalogue = orphelins → suppression best-effort
  AVANT toute lecture (I4) ; échec = warning + orphan_bytes, jamais bloquant
```

Un échec de `remove_file` n'a **aucune** conséquence logique : le fichier
n'est plus vivant (I4), aucune lecture ne l'atteint, aucun tombstone n'a à
être conservé « au cas où ». (La rustine « garder les tombstones si l'unlink
échoue » de l'audit est explicitement **rejetée** comme mécanisme cible —
elle n'a même pas de sens dans ce modèle.) La résurrection ENG-DUR-002
devient structurellement impossible : l'open ne compose plus sa réalité
depuis le répertoire.

## 15. Récupération (open)

```text
lock writer → store.meta (format 3, sinon rejet typé)
→ génération : pointeur présent → gen-N obligatoire ; ABSENT + gen-N
  présent → ERREUR TYPÉE (I10) ; absent sans gen-N → racine
→ crypto (inchangé)
→ lire catalog.meta (absent sur store format 3 = corruption typée)
  → vérifier store_generation ; vérifier live_ssts/live_wals ⊆ disque
    (manquant = MissingLiveSst/MissingLiveWal — E4, plus jamais silencieux)
→ GC : disque ∖ catalogue supprimé best-effort (avant toute lecture)
→ ouvrir les SST vivantes (lazy, O(métadonnées) — inchangé)
→ replay des live_wals par start_sequence croissant :
    chaque segment → memtable (versions avec leurs séquences du WAL)
    torn tail toléré sur le DERNIER segment seulement
    last_allocated = max(catalog.last_published_sequence, max rejoué)
    visible_sequence = last_allocated   (tout ce qui est durable redevient
                                         visible — contrat actuel)
→ segments non terminaux re-scellés → re-flushés par le pipeline normal
  (pas de chemin de recovery spécial : le flush worker les prend)
→ construire Version/SuperVersion initiale ; catalogue inchangé par l'open
  (l'open ne commite RIEN sauf la première création du store)
```

Déterminisme (E4) : deux replays du même disque produisent le même état —
les séquences sont relues, jamais ré-attribuées ; l'ordre des segments est
celui du catalogue, pas du répertoire.

## 16. Rotation complète (ADR-042, refondue sur le catalogue)

```text
geler les écritures (rotation = opération exclusive, comme aujourd'hui)
→ seal + drain des flushs (l'état = SST vivantes uniquement, WAL vides)
→ construire gen-N+1 : crypto.meta neuve, merge streamé re-scellé
  (mêmes règles GC §13 — les snapshots actifs RETIENNENT l'ancienne
  génération : voir ci-dessous), catalog.meta de gen-N+1 complet
  (store_generation = N+1, catalogue auto-suffisant)
→ publier generation.meta (tmp/fsync/rename/sync_dir racine)      [D+V]
→ bascule RAM (nouvelle SuperVersion sur le catalogue N+1)
→ ancienne génération ENTIÈRE → file de retrait (fichiers Arc-détenus par
  les snapshots pré-rotation qui vivent encore : un ReadSnapshot pris
  avant la rotation reste intégralement lisible — ses handles portent
  leur CryptoContext déjà ouvert, jamais un chemin+clé à re-dériver)
→ GC différée de gen-N quand le dernier snapshot pré-rotation tombe ;
  à l'open : gc_inactive_generations inchangé MAIS après I10
```

Différences vs aujourd'hui : le pointeur est synchronisé au répertoire
(ENG-DUR-004 fermé), la GC est différée par référence au lieu d'immédiate,
et un snapshot survit à la rotation (E2) — au prix documenté que les
octets ancienne-DEK persistent tant que ce snapshot vit (métrique
`retained_by_snapshots_bytes`, posture à inscrire dans
`docs/security/encryption-model.md`).

## 17. Erreurs, bornes et observabilité

**Nouvelles erreurs typées** (toutes `#[non_exhaustive]` comme l'existant) :
`MissingLiveSst`, `MissingLiveWal`, `CorruptCatalog`, `CatalogGenerationMismatch`,
`BackgroundError { cause }`, `SequenceSpaceExhausted`, `ValueTooLarge`,
`BatchTooLargeBytes`, `SnapshotStoreClosed` (usage d'un snapshot après
`close`). Politique de poison des verrous internes : les verrous du moteur
protègent des états reconstructibles depuis le disque — un poison est
converti en `BackgroundError` (écritures refusées, lectures sur l'état
publié continuent), jamais un déni permanent silencieux.

**Bornes (E6)** — refus typé avant réservation de séquence :
`max_value_bytes`, `max_key_bytes`, `max_batch_bytes` (en plus de
`MAX_BATCH_OPS` existant), `memtable_target_bytes`. Valeurs par défaut à
mesurer, pas devinées ici.

**Métriques nouvelles** (chacune liée à une décision) : `fsync_count`
(décide ADR-047), `write_stall_micros` + `write_stall_count` (calibre
memtable/N scellées), `active_snapshots` / `oldest_snapshot_sequence` /
`oldest_snapshot_age` / `retained_by_snapshots_bytes` (décide une
éventuelle borne de rétention), `orphan_bytes` + `pending_deletion_files`
(santé du retrait), `catalog_generation` (corrélation incidents),
`background_error` (gauge 0/1 — alerte opérateur), `wal_segments` (santé
du pipeline). Les compteurs existants (`flush_count`, `compaction_*`,
cache, `bytes_*`) sont conservés tels quels.

## 18. Comparaison obligatoire — Architecture A (S1) vs B (S2/S3)

| Critère | A — S1 seul (`Arc<Version>` SST, memtable non versionnée, WAL actuel) | B — S2/S3 (cette cible) |
|---|---|---|
| Correction | Ferme DUR-001/002 et la compaction bloquante. **Ne satisfait pas E1/E2** : la memtable vivante fuit dans toute « vue » ; recall hybride, export multi-passes, scans paginés inter-verrous restent incohérents sous écriture | Satisfait E1-E7 ; incohérences multi-appels fermées par construction |
| Complexité | Faible (Arc + manifest) | Moyenne-haute : séquences, clés internes, GC par visibilité, segments — c'est le prix réel de la décision produit, non minimisé |
| Formats modifiés | Manifest seul | WAL, enveloppe WAL, SST (3 codecs), catalogue, store.meta — **un** bump groupé |
| Durée d'implémentation | ~2-3 jalons | ~7 jalons (§20) |
| Coût mémoire | Nul | Versions retenues en memtable/SST bornées par les snapshots actifs (purge au flush §10 + GC §13) ; +9 octets/entrée |
| Coût disque | Nul | +9 octets/entrée SST ; versions retenues sous snapshots longs (instrumenté) |
| Coût compaction | Inchangé | Merge streamé (baisse la RAM vs aujourd'hui) ; règle GC par liste de snapshots (CPU négligeable devant l'I/O) |
| Recall sous concurrence | Fusion RRF sur états mixtes (état actuel documenté) | Fusion, hydratation, graphe, contexte sur UN état |
| Changefeed (N15) | Rien : tout à construire (séquences absentes) | Curseur = séquences déjà en place ; segments = unités de rétention |
| Background flush | Possible mais la troncature du WAL unique sous flush concurrent est délicate (fenêtres à prouver) | Naturel (segment par memtable) |
| Risque de refonte à 6 mois | **Élevé et certain** : la décision produit actée exige S2 → clés internes, WAL et SST de A seraient re-bumpés, la memtable réécrite, l'API snapshot S1 dépréciée — trois formats successifs, exactement l'anti-pattern proscrit | Faible : group commit, tiered, changefeed, N>1 scellées sont des extensions sans changement de format |

**Conclusion** : A est rejetée comme cible (contradiction directe avec la
décision produit) **et** comme étape publique : ses seuls artefacts
réutilisables (Arc sur l'ensemble SST, manifest) existent dans B sous une
forme différente (catalogue avec WAL + séquences ; Version dans
SuperVersion) — livrer A d'abord créerait un format de manifest sans WAL ni
séquences et une API snapshot S1, tous deux à remplacer. Les éléments de A
qui appartiennent déjà à B (sync_dir, garde-fous, retrait différé,
VersionEdit) sont livrés par les jalons R1/R3 de B directement.

## 19. Classement des recommandations de l'audit (A / B / C)

**A = primitive permanente** (reste nécessaire dans la cible — implémentable
immédiatement) ; **B = mitigation temporaire rejetée comme solution
finale** ; **C = correction architecturale de racine** (portée par cette
cible).

| Finding | Classement | Justification / destination |
|---|---|---|
| ENG-DUR-001 (pas de catalogue) | **C** | Catalogue §9 (R2) |
| ENG-DUR-002 (résurrection post-compaction) | **C** — racine = §14 (publication ≠ suppression). La « correction minimale » de l'audit (retry + conserver les tombstones si unlink échoue) est **B, rejetée** : elle raisonne dans le modèle où le répertoire est la vérité. Seule la part instrumentation (compter l'échec) est **A** | §14 (R3/R6) ; interim R1 : l'échec d'unlink cesse d'être silencieux (log + compteur), sans mécanisme de correction logique |
| ENG-DUR-003 (sync_dir) | **A** | §9/§14 — requis par I5 pour toujours ; **précède** l'architecture (R1) |
| ENG-DUR-004 (garde-fou génération) | **A** | I10 — définitif ; précède (R1) |
| ENG-COR-001 (draft S1 : publication par remplacement) | **C** | Structurellement fermé par §11 (edit appliqué au courant) — le draft S1 est retiré (§21) |
| ENG-CON-001 (compaction inline sous verrou) | **C** | §12/§13 (R5/R6) |
| ENG-COR-002 (vue multi-appels) | **C** | `ReadSnapshot` §7 + propagation (R7). Interim **A** : documenter le contrat actuel (chaque appel atomique, compositions non) — ce texte reste vrai dans la cible pour les appels *sans* snapshot |
| ENG-CON-002 (poison RwLock) | **A** | Politique §17 (`BackgroundError`, jamais un déni permanent) — permanente, indépendante de la refonte du grand verrou ; précède (R1) |
| ENG-CON-003 (verify sans try_lock) | **A** | try_lock + warning — verify restera un lecteur hors-processus dans la cible ; précède (R1) |
| ENG-RES-001 (bornes tailles) | **A** | §17 — les bornes conditionnent aussi le seal en octets (R2/R4) ; le refus typé peut précéder (R1) |
| ENG-RES-002 (scans produits non bornés) | **A** | La pagination par `scan_agent_page` est définitive ; les snapshots (R7) la rendent en plus *cohérente*. Migration des 4 chemins : peut précéder |
| ENG-TST-001 (gaps de tests) | **A** | R0 — les tests d'invariants (I1-I12) sont l'outillage permanent de la cible |
| ENG-DOC-001 (contrats mensongers) | **A** | Correction immédiate (commentaire sst_block.rs:408, README, CLAUDE.md) — précède |
| ENG-RES-003 (métriques) P3 | **A** | §17 (R0 pour fsync/orphans) |
| ENG-COR-003 (invariant vec_id) P3 | **A** | Documenter l'invariant « aucune référence hors batch » — inchangé par la cible |
| ENG-CON-004 (annulation spawn_blocking) P3 | **A** | Contrat de surface documenté — la cible ne le change pas (workers ≠ futures) |
| ENG-CRY-001 (résidu ancienne DEK) P3 | **A** | Doc posture + `orphan_bytes`/`retained_by_snapshots_bytes` (§16) |

Mitigations **B rejetées comme solution finale** (récapitulatif) : retry
d'unlink comme correction *logique* ; conservation de tombstones sur échec
d'unlink ; réalité logique reconstruite du répertoire ; verrou global comme
garantie de stabilité de vue ; clonage de memtable pour simuler un
snapshot ; blocage des écritures pendant un scan long. Aucune ne structure
la cible ; aucune n'est proposée comme jalon.

## 20. Roadmap — chaque jalon est un sous-ensemble définitif de la cible

Aucun jalon ne construit un mécanisme que le suivant remplace. Le seul
« interim » assumé : entre R2 et R6, certaines opérations gardent le grand
verrou — c'est *l'absence* d'un mécanisme pas encore livré, pas un
mécanisme jetable.

---

**R0 — Tests d'invariants et instrumentation** (précède tout)
- Formats : aucun. Code : tests + `EngineStats` (fsync_count, orphan_bytes).
- Tests qui doivent échouer avant : résurrection post-unlink-échoué
  (failpoint remove_file) ; perte du pointeur de génération (suppression
  directe) ; SST vivante supprimée détectée (les 2 tests-témoins retournés,
  marqués `#[ignore]` jusqu'à R3) ; harnais d'invariants I1-I12 (squelette).
- Failpoints : `remove_file` de compaction, `before_catalog_publish`
  (réservé). Propriétés obtenues : falsifiabilité. Rollback : trivial.
  Bench : baseline re-archivée (référence des jalons suivants).
  Non jetable : ces tests sont la définition exécutable de la cible.

**R1 — Primitives permanentes de publication disque** (classe A)
- Formats : aucun. Code : `sync_dir` après chaque rename (SST, store.meta,
  generation.meta, crypto.meta) ; garde-fou I10 ; unlink non silencieux
  (log + compteur — sans mécanisme logique) ; try_lock verify ; politique
  poison → erreur typée ; bornes de taille refusées typées ; migration des
  4 scans produits vers `scan_agent_page` ; corrections doc (DOC-001).
- Tests : ceux de R0 (garde-fou et unlink passent ici).
- Propriétés obtenues : DUR-003/004 fermés ; DUR-002 *visible* (pas encore
  structurellement fermé). Pas encore : catalogue, snapshots.
- Rollback : revert simple. Bench : coût sync_dir (Linux).
- Non jetable : chaque élément est exigé par I5/I10/I11/E6 pour toujours.

**R2 — Formats cibles : séquences + WAL segmenté + SST versionnées + catalogue**
*(le bump unique — tout le on-disk de la cible, en une fois)*
- Formats : `STORE_FORMAT_VERSION 3` ; `WalRecord:3`, `WalEnvelope:2`,
  `SstHeader:2`, `SstDataBlock:2`, `SstBlockIndex:3`, `Catalog:1` ;
  format.lock mis à jour ; fuzz targets re-générées + comparateur interne.
- Code : codecs + write path séquencé (allocation, WAL porteur de `base`,
  memtable versionnée insert-only, `visible_sequence`) + open par
  catalogue (I4, E4 : MissingLiveSst/Wal typées) + GC d'orphelins à l'open.
  Le flush/compaction restent **synchrones sous verrou** (mécanique
  actuelle) mais publient déjà par commit de catalogue (§11 sans
  concurrence — le protocole est le même, le commit_lock est juste
  incontesté).
- Tests qui doivent échouer avant : détection SST/WAL manquant (tests R0
  dé-ignorés) ; replay déterministe (séquences relues) ; batch
  tout-ou-rien par séquences ; kill-loop étendu au failpoint
  `before_catalog_publish`.
- Propriétés obtenues : E3, E4, I1-I5, I7-I9 ; DUR-001/002 fermés.
  Pas encore : snapshots publics, background, GC par visibilité (sans
  snapshot actif, la règle §13 dégénère en last-write-wins — le full-merge
  actuel est déjà conforme).
- Rollback : revert du jalon ; stores v3 rejetés par le build antérieur
  (typé) ; migration export/import documentée dans les deux sens.
- Bench : write path (+9 o/entrée, seq), open (catalogue), vs R0.
- Non jetable : c'est le format final — tout jalon suivant est du code, pas
  du format.

**R3 — VersionSet + SuperVersion + ReadSnapshot (moteur)**
- Formats : aucun. Code : `Version`/`SuperVersion`/`ReadSnapshot`, registre
  de snapshots, handles Arc, file de retrait (§14), `Engine::snapshot()`,
  `get/scan*` paramétrés par snapshot (lecture à `latest` = snapshot
  implicite éphémère).
- Tests avant : « snapshot répétable sous put/flush/compact » ; « fichier
  retenu par snapshot pas supprimé, supprimé après drop » ; scans paginés
  stables sous écritures.
- Propriétés : E1/E2 au niveau moteur ; I6 ; DUR-002 structurellement clos
  (§14). Pas encore : E5 (fond), propagation produit.
- Rollback : revert (aucun format). Bench : overhead snapshot (attendu ~0).
- Non jetable : structures finales de la cible.

**R4 — Memtable sealing + WAL lifecycle**
- Formats : aucun (segments déjà au format R2). Code : seal atomique,
  N=1 scellée, edit `{add_wals}` au seal, retrait des segments au flush,
  backpressure + `write_stall_*`.
- Tests avant : versions lues identiques avant/pendant/après seal ; kill
  entre seal et flush (replay des deux segments) ; stall instrumenté.
- Propriétés : I8 complet. Pas encore : flush asynchrone (le flush reste
  appelé synchrone, mais opère déjà sur une scellée figée).
- Rollback : revert. Bench : latence de seal.
- Non jetable : le seal et le cycle segment sont finaux.

**R5 — Background flush**
- Formats : aucun. Code : flush worker, queue bornée, `background_error`,
  close/join, purge des versions au flush (règle §13 côté flush).
- Tests avant : écritures pendant un flush long (failpoint de
  ralentissement) ; panique worker → BackgroundError, lectures continuent ;
  close draine.
- Propriétés : E5 pour le flush. Rollback : revert (le flush synchrone de
  R4 est le même code appelé inline). Bench : p99 écriture sous flush.
- Non jetable : worker final.

**R6 — Concurrent compaction + snapshot-aware GC**
- Formats : aucun. Code : compaction worker, merge streamé, règle GC §13
  complète (liste de snapshots), validation d'edit (conflits/rotation),
  abandon spéculatif.
- Tests avant : lecteurs O(normal) pendant compaction longue (critère du
  plan §10) ; flush pendant compaction → rien perdu (test préparé en R0) ;
  table de l'exemple §13 vérifiée version par version ; rotation pendant
  compaction → abandon propre.
- Propriétés : E5 complet ; I6 complet côté GC. Rollback : revert
  (compaction redevient synchrone — même code). Bench : p99 mixte vs N7,
  write/space amp du soak.
- Non jetable : pipeline final.

**R7 — Propagation du snapshot aux index et au Context Engine**
- Formats : aucun. Code : les `Persistent*` lisent via un
  `SnapshotView` (le provider `EngineProvider` existant, paramétré) ;
  attention spécifique : le cache RAM de nœuds vectoriels et
  l'`entry_point`/`count` en RAM reflètent *latest* — les lectures sous
  snapshot lisent META/nœuds à travers le snapshot (cache bypassé ou
  contrôlé par séquence — décision d'implémentation mesurée) ;
  `NativeMemoryStore` : `read_snapshot()` produit + variantes
  `recall`/`compile_context`/`export`/`verify logique` sous snapshot ; le
  recall hybride prend UN snapshot pour ses trois passes de lecture
  (`touch` reste une écriture post-lecture, hors snapshot — write-behind
  assumé).
- API retenue : **contexte de lecture interne + exposition publique
  limitée** — `Memory::read_snapshot()` et paramètres optionnels sur les
  opérations composées (`recall`, `compile_context`, export) ; pas de
  variante `_with_snapshot` sur chaque méthode du trait (bruit d'API sans
  besoin — les appels unitaires restent `latest`).
- Tests avant : recall hybride sous écrivain concurrent = résultats d'UN
  état ; export ↔ verify logique sur le même snapshot = identiques.
- Propriétés : E1 de bout en bout. Non jetable : c'est la surface produit
  finale.

**R8 — Writer queue / group commit (ADR-047) — uniquement si mesuré**
- Déclencheur : `fsync_count`/latences sous charge concurrente réelle
  (métriques de R0) démontrant le besoin. Formats : aucun (le WAL R2
  accepte déjà plusieurs batches par fsync). Emplacement §21-047.
- Non jetable par construction : s'il n'est jamais construit, rien ne
  l'attend.

## 21. Découpage ADR proposé

Le brouillon ADR-043 actuel (S1) est **retiré** (non committé — remplacé,
pas amendé : sa cible S1 contredit la décision produit). Nouveau
découpage — quatre ADR + un conditionnel, chacun avec des invariants
testables indépendamment (aucune fusion supplémentaire : séquences et
catalogue ont des invariants disjoints — I1-I3 vs I4-I5 — et des formats
disjoints ; les fusionner rendrait leurs critères de sortie
indissociables) :

- **ADR-043 — Sequences, visibility and read snapshots** : §6, §7, §10
  (structure versionnée), `ReadSnapshot`/`SuperVersion` (§4), invariants
  I1-I3, I6 (rétention), I7. Formats : WalRecord:3, WalEnvelope:2,
  SstHeader:2, SstDataBlock:2, SstBlockIndex:3. Hors périmètre :
  transactions d'écriture, snapshots persistants, multi-writer.
  Dépendances : aucune. (Jalons R2-R3.)
- **ADR-044 — Durable state catalog and file lifecycle** : §9, §14, §15,
  invariants I4-I5, I8-I11. Format : Catalog:1, STORE_FORMAT_VERSION 3,
  espace d'ids unifié. Ferme ENG-DUR-001/002/004 et l'ex-gap ADR-039 §7.
  Hors périmètre : contenu du changefeed (N15 réutilise, ne modifie pas).
  Dépendances : ADR-043 (le catalogue publie `last_published_sequence`).
  (Jalons R1-R2-R3.)
- **ADR-045 — Immutable memtables, WAL segments and background flush** :
  §8, §10, §12, backpressure, `background_error`, N=1→N. Formats : aucun
  nouveau (segments définis par 043/044). Dépendances : 043, 044.
  (Jalons R4-R5.)
- **ADR-046 — Concurrent compaction and snapshot-aware GC** : §11
  (protocole de commit/validation/abandon), §13 (règle GC + exemple
  normatif), interaction rotation (§16). Dépendances : 043-045.
  (Jalon R6.)
- **ADR-047 — Writer queue and group commit** *(conditionnel)* : §20-R8 ;
  écrit seulement mesures en main. Dépendances : 043 (plages de
  séquences), 045 (write path).

La propagation produit (R7) ne requiert pas d'ADR moteur : c'est
l'application du contrat d'ADR-043 à la surface — une section dans la doc
du trait `MemoryStore` suffit ; si l'API publique `read_snapshot()` du SDK
mérite une trace, ce sera un ADR produit court, séparé.

## 22. Machines d'état

Notation : `[D]` durabilité, `[V]` visibilité, `vs` = `visible_sequence`,
`cg` = `catalog_generation`. « ↺ » = état après crash à ce point.

**Single write / atomic batch** (identiques — un put est un batch de 1)
```text
E0 idle
E1 réservation      RAM: base=last_alloc+1, last_alloc+=n   disque: —
                    vs: inchangé  cg: inchangé
E2 append WAL       disque: segment courant += record(base, ops)
E3 fsync WAL   [D]  ↺ à E2 : record déchiré → tronqué, batch ABSENT (I2)
                    ↺ après E3 : batch durable, redevient visible au replay
                      (même si l'appelant a reçu une erreur ensuite — documenté)
E4 memtable         RAM: n versions (k,seq,kind) insérées   vs: inchangé
E5 publier     [V]  vs = base+n-1 (atomique, un batch entier — I2)
Échec E2/E3 : plage brûlée, memtable intacte, vs inchangé, erreur typée.
```

**Snapshot création / libération**
```text
S1 create : lire vs ; Arc::clone(current SuperVersion) ; registre += vs
            [V du snapshot : figée à cet instant]  disque : rien
S2 lecture: filtre seq ≤ vs sur memtables (insert-only) + SST figées
S3 drop   : registre −= vs ; Arcs relâchés → fichiers/memtables éligibles
            au retrait (§14)   disque : rien
↺ : aucun état disque — un snapshot ne survit jamais au processus (E8).
```

**Memtable seal**
```text
M0 mutable M_k active, segment W_k courant
M1 [commit §11] edit {add_wals:[W_k+1]} :
     W_k+1 créé (fsync + sync_dir) → catalog cg+1 écrit [D] → installer
     SuperVersion' { mutable: M_k+1 vide, immutable: [M_k, …], version }  [V]
     write path bascule sur M_k+1 / W_k+1
   ↺ avant écriture catalogue : W_k+1 orphelin (GC open), M_k reste
     mutable au replay — seal n'a jamais eu lieu
   ↺ après : replay voit W_k (scellée à re-flusher) + W_k+1 (mutable)
M2 M_k figée, en file de flush (backpressure si file pleine)
```

**WAL rotation / retrait** — intégrés au seal (ci-dessus) et au flush
(ci-dessous) ; il n'existe pas de rotation WAL indépendante.

**Flush**
```text
F0 M_k scellée (WAL W_k vivant — I8)
F1 écrire <id>.sst : tmp → write_all → sync_all → rename → sync_dir
   (id allou par next_file_id RAM)                         [D contenu]
   ↺ : orphelin, GC open ; W_k toujours vivant → replay complet
F2 [commit §11] edit {add_ssts:[S_id @ couche top], delete_wal_ids:[W_k],
   last_published_sequence: max_seq(M_k), next_file_id}
   → catalog cg+1 [D+V logique] → SuperVersion' sans M_k, Version'+S_id [V]
   ↺ avant : comme F1. ↺ après : S vivante, W_k orphelin (GC), M_k absente
     du replay — état final correct.
F3 W_k, M_k → retrait quand dernier Arc tombe (§14)
```

**Compaction**
```text
C0 inputs = Version courant figé (Arc) — hors verrou
C1 merge streamé sous règle GC §13 (liste snapshots au démarrage du job ;
   liste re-lue au commit : un snapshot créé PENDANT le merge voit un état
   ≥ inputs, jamais un état que le merge pourrait avoir purgé — les
   versions purgées sont exactement celles qu'aucun snapshot actif au
   moment du merge ne distingue, et un snapshot postérieur a un vs plus
   grand encore)
C2 écrire out.sst (comme F1)   ↺ : orphelin
C3 [commit §11] VALIDER (inputs vivants ? génération inchangée ?) —
   conflit → C5 abandon
   edit {add_ssts:[out @ couche(min inputs)], delete_sst_ids: inputs}
   → catalog cg+1 [D+V logique] → Version' [V]
   ↺ avant : anciens vivants, out orphelin. ↺ après : out vivant, inputs
     orphelins→GC. Jamais d'état mixte (le catalogue est atomique).
C4 inputs → retrait différé (snapshots les retiennent — I6)
C5 abandon : out → retrait comme orphelin ; aucun commit
```

**Catalog commit** — voir §11 (protocole unique) ; c'est LA section
critique de publication : validation → écrire → installer, sous
`commit_lock`, O(1) hors I/O du petit fichier.

**File retirement**
```text
R0 retiré du catalogue (commit)        [invisible aux nouvelles vues — V]
R1 encore référencé (Versions/snapshots/jobs) → attendre Drop
R2 dernier Arc tombe → file de suppression
R3 unlink best-effort : échec → orphan_bytes, ré-essai (worker + open)
↺ n'importe où : fichier hors catalogue = orphelin → GC open (I4).
   La suppression n'est jamais un point de publication (I11).
```

**Recovery** — voir §15 ; points : [D] = rien n'est écrit par l'open
(hors GC best-effort et premier bootstrap) ; [V] = SuperVersion initiale
construite après replay ; `vs = last_alloc` recalculé déterministe.

**Full key rotation** — voir §16 ; points de durabilité :
catalog.meta de gen-N+1 (dans son répertoire) **puis** generation.meta +
sync_dir racine [D+V] ; ↺ avant pointeur : gen-N+1 = orpheline complète
(supprimée par gc_inactive_generations — désormais **après** le garde-fou
I10) ; ↺ après pointeur : gen-N = retirée, GC différée/ré-essayée.

**Shutdown avec jobs actifs**
```text
X0 close() : refuser les nouvelles écritures (typé)
X1 seal mutable si non vide → drain file de flush (workers terminent)
X2 compaction en cours : laissée finir OU abandonnée (C5) — décision
   d'implémentation ; les deux sont sûres (commit atomique ou orphelin)
X3 join workers ; drain file de suppression (best-effort)
X4 snapshots encore vivants : leurs Arcs survivent au close() du store ?
   NON — décision : close() échoue typé (SnapshotStoreClosed) si des
   snapshots actifs existent, OU les invalide (lectures → erreur typée).
   Retenu : invalidation typée (un drop de process ne demande pas la
   permission ; le contrat E8 « aucun snapshot persistant après
   fermeture » l'impose).
↺ pendant shutdown : identique à un crash ordinaire — aucun état spécial.
```

## 23. Risques de la solution choisie

1. **Complexité réelle de S2** (non minimisée) : clés internes + GC par
   visibilité + segments touchent chaque couche. Mitigation : R2 groupe
   tout le format en un jalon fortement testé (fuzz re-généré, model-based
   étendu aux snapshots) ; les jalons suivants sont du code sur format
   stable.
2. **Croissance des versions sous snapshot long** (`touch_last_access`
   est un amplificateur naturel) : instrumentée
   (`retained_by_snapshots_bytes`), purge au flush, alerte de durée —
   mais pas de borne dure (contrat E2). Risque assumé et mesuré.
3. **Cache vectoriel vs snapshots** (R7) : le cache RAM des nœuds reflète
   latest ; le bypass sous snapshot peut coûter en latence de recall
   snapshoté. À mesurer ; repli : cache indexé par séquence.
4. **Un seul bump groupé (R2)** : gros jalon. Mitigation : codecs +
   comparateurs livrables et fuzzables indépendamment avant le câblage ;
   le kill-loop existant rejoue tel quel sur le nouveau format.
5. **Write stall mal calibré** (N=1 scellée) : mesuré par
   `write_stall_*` ; passage à N>1 sans changement de format si besoin.
6. **Dérive de conception pendant l'implémentation** : les invariants
   I1-I12 sont la spécification exécutable — tout écart doit modifier ce
   document et l'ADR concerné, pas silencieusement le code.

## 24. Recommandation finale

**T2 — construire directement la fondation S2/S3.**

- T1 (S1 final) : rejeté — contredit frontalement la décision produit
  (E1/E2 insatisfaisables sans versionner la memtable, §18).
- T3 (S1 comme étape interne) : rejeté — l'examen §18 montre que les
  artefacts spécifiques à S1 (manifest sans séquences ni WAL, API snapshot
  structurel) seraient remplacés, créant la séquence de formats
  intermédiaires proscrite ; les éléments de S1 qui survivent (sync_dir,
  retrait différé, commit par edit) sont déjà des jalons de B (R1, R3) —
  il n'y a rien à « passer par S1 » pour obtenir.
- T4 : aucune architecture alternative plus petite identifiée qui
  satisfasse E1-E7 — retirer n'importe lequel des six mécanismes du §1
  casse une exigence (sans séquences : pas de snapshot sans copie ; sans
  catalogue : pas d'E3/E4 ; sans segments : I8 indémontrable sous flush
  concurrent ; sans versions : pas d'E1/E2 ; sans edit-commit : perte
  ENG-COR-001 ; sans retrait différé : résurrections).

Premier pas : valider ce document, rédiger ADR-043/044 (les deux formats),
puis R0.
