# ADR-043 — Version set immuable, snapshots de lecture et compaction concurrente (N13)

**Statut** : 🟡 Proposed dans l'ensemble — **§1 (manifest) implémenté et clos
(J1/J2, 2026-07-20)** ; **§2/§3 amendés le 2026-07-20** (publication par
`VersionEdit`, correction ENG-COR-001 exigée par l'audit §9 avant tout code)
et **implémentés côté J3 le même jour** (`store/version.rs`,
`Engine::snapshot()`, suppression différée — flush/compaction toujours sous
le verrou exclusif) ; la part J4 de §3 (merge hors verrou, test
flush-pendant-compaction) reste non implémentée. Acceptation à la clôture
de N13.
**Date** : 2026-07-19

## Amendement 2026-07-20 — §1 implémenté

Implémenté conformément à §1 ci-dessous, avec une simplification délibérée
par rapport au texte original : au lieu de la tolérance additive décrite
(« absence sur un store pré-N13 ⇒ reconstruit une seule fois »),
`STORE_FORMAT_VERSION` a été bumpé 2→3 (`format/store_meta.rs`). Comme
`check_or_create_store_meta` compare cette version par égalité stricte,
aucun store antérieur à ce build ne peut de toute façon atteindre le code du
manifest — la tolérance additive n'aurait jamais été exercée. Le seul cas où
`manifest.meta` est légitimement absent sous un store déjà en version 3 est
un bootstrap (store neuf, ou crash entre la publication d'une SST et celle
du manifest qui devait l'y ajouter) : traité en publiant immédiatement un
manifest couvrant ce que le listage disque trouve, sans erreur. Cohérent
avec la recommandation de l'audit (§10 : « bump recommandé ») et la
politique du projet (format expérimental, aucune rétrocompatibilité due
avant gel — `PLAN-NATIVE-ENGINE.md` §1).

Ordre de publication réel (non détaillé dans le texte original) :
**SST → manifest → troncature WAL** dans `flush()` — le manifest doit être
durable *avant* la troncature, jamais après, sinon un crash entre les deux
ferait traiter par erreur une SST fraîchement flushée (et donc son WAL déjà
tronqué) comme un orphelin à la réouverture.

Preuves : `crates/basemyai-engine/tests/corruption_smoke.rs`
(`deleted_sst_is_detected_once_catalog_lands`, plus corruption de
`manifest.meta`), `tests/failpoints.rs`
(`before_sst_manifest_publish_leaves_the_new_sst_an_orphan_and_wal_recovers`),
`tests/compaction_remove_retry.rs`
(`orphan_after_persistent_remove_failure_never_resurrects_a_deleted_key` —
ferme ENG-DUR-002 pour de bon, pas seulement compté comme le correctif J0
minimal le faisait). `verify --logical` confronte aussi le manifest
(`store/verify.rs`, `IssueKind::LiveSstMissing`/`SstManifestCorrupt`).

**§2/§3 restent à faire** : le protocole de publication de compaction décrit
plus bas (remplacement complet du `Version`) contient toujours le défaut
ENG-COR-001 (perte d'une SST flushée pendant une compaction concurrente) —
non exercé par l'implémentation actuelle de §1 (verrou exclusif inchangé,
aucune concurrence de compaction encore introduite), mais doit être corrigé
en « version edit » avant que §2/§3 ne soient implémentés (J3/J4).

## Amendement 2026-07-20 — §2/§3 réécrits : publication par `VersionEdit` (ENG-COR-001)

Le corps normatif de §2/§3 ci-dessous est **réécrit** par cet amendement —
l'exigence de l'audit (« Amendement d'ADR-043 avant acceptation »,
ENG-COR-001) est ainsi remplie *avant* toute implémentation de J3/J4.
Ce qui change par rapport au texte original :

1. **Publication de compaction = edit, jamais un remplacement.** Le texte
   original publiait comme ensemble complet un `Version` calculé depuis le
   snapshot d'entrée du merge — toute SST publiée entre-temps par un flush
   concurrent en était absente, donc reclassée orpheline par le manifest et
   supprimée à la réouverture (le scénario exact d'ENG-COR-001). Le
   protocole corrigé publie un `VersionEdit { added, deleted }` appliqué au
   `Version` courant **au moment du commit**, sous l'exclusion finale :
   `V_next = (V_current_au_commit ∖ deleted) ∪ added`. Toute SST apparue
   depuis le snapshot d'entrée est conservée d'office. Même propriété que
   les `VersionEdit` de LevelDB/RocksDB — mais **en RAM seulement** : le
   fichier `manifest.meta` reste une liste complète réécrite à chaque
   publication (jamais un journal de deltas + CURRENT, machinerie proscrite
   par l'audit §11 tant que le nombre de SST reste petit).
2. **Granularité de la suppression différée : par fichier, pas par
   `Version`.** L'alternative « Drop sur `Version` » retenue par le texte
   original est incorrecte dès qu'une SST est partagée entre plusieurs
   `Version` successifs — ce que chaque flush produit déjà
   (`V1 = V0 ∪ {S6}` partage S1..S5 avec V0) : le Drop de V1 ne peut pas
   savoir si V0 (retenu par un snapshot long) lit encore S1..S5. La
   correction garde l'esprit de la décision (« `Arc`/`Drop` **est** déjà le
   mécanisme, gratuit, testé par le compilateur ») en l'appliquant à la
   granularité où elle est correcte : un `Arc<SstHandle>` par fichier,
   partagé entre tous les `Version` qui le contiennent, avec un flag
   `retired` armé par l'edit qui retire l'id du manifest. Le fichier n'est
   physiquement supprimé qu'au drop du **dernier** `Arc` d'un handle
   `retired` — jamais avant, et jamais pour un handle encore vivant dans le
   manifest (sinon le drop de l'`Engine` supprimerait le store entier).
3. **`Engine::snapshot()` est un snapshot S1, étiqueté comme tel** (audit
   §6) : il fige *les fichiers*, pas *la vue* — la memtable reste vivante,
   une écriture postérieure au snapshot reste visible via l'API `Engine`,
   et le `Snapshot` lui-même ne lit **que** les couches SST figées. Pas de
   vue point-in-time (S2/MVCC : rejeté, aucun consommateur — ne construire
   que sur besoin produit démontré).
4. **Découpage J3/J4** (roadmap de l'audit §10) : J3 livre `Version`
   immuable + `Snapshot` + publication par edit + suppression différée +
   métrique « snapshots actifs », **sous le verrou exclusif existant**
   (flush et compaction restent `&mut self` ; `V_current_au_commit` y est
   trivialement égal au snapshot d'entrée, l'edit est donc équivalent au
   remplacement — mais le chemin edit avec sa validation est implémenté dès
   J3, pour que J4 ne soit qu'un changement de verrouillage, pas de
   protocole). J4 sort le merge du verrou et active le test
   flush-pendant-compaction.
5. **Rotation complète non couverte par les snapshots** : `rotate_key_full`
   change de répertoire de génération, de DEK et GC l'ancien répertoire
   entier (`gc_old_generation`, best-effort). Un `Snapshot` pris avant une
   rotation complète peut devenir illisible après elle (erreur I/O typée à
   la prochaine lecture, jamais un panic ni de données corrompues) —
   assumé et documenté sur l'API : la rotation complète est une opération
   rare, exclusive, dont le contrat (« plus aucun octet sous l'ancien
   DEK ») est incompatible avec la rétention de fichiers par des lecteurs.
6. **Block cache** : les ids de SST ne sont jamais réutilisés au sein d'une
   génération (`next_sst_id` dérive du scan non filtré, ENG-DUR-002), donc
   l'invalidation par id au moment du *retrait* (l'edit) reste correcte
   telle quelle ; une lecture via un `Snapshot` n'alimente pas le cache de
   l'`Engine`.

### Invariants (testables) imposés par cet amendement

- **INV-VS-1 — Immutabilité.** Un `Version` publié n'est jamais muté ; la
  liste d'ids vue par un `Snapshot` est identique bit-à-bit pour toute la
  vie du snapshot, quel que soit le nombre de flush/compactions concourants.
- **INV-VS-2 — Publication atomique.** Le seul point de visibilité est le
  remplacement de `current: Arc<Version>` (sous `&mut self` en J3, sous
  l'exclusion brève en J4). Un lecteur clone l'`Arc` une fois en entrée et
  ne relit jamais `current` en cours d'opération.
- **INV-VS-3 — Compaction = edit.** La compaction publie
  `VersionEdit { added, deleted }` appliqué au `Version` courant au commit,
  jamais un ensemble calculé au début du job.
- **INV-VS-4 — Validation des `deleted`.** Chaque id de `deleted` doit être
  présent dans le `Version` courant au commit ; sinon erreur typée, aucun
  manifest publié, `current` inchangé.
- **INV-VS-5 — Conservation d'office.** Toute SST du `Version` courant au
  commit qui n'est pas dans `deleted` est dans `V_next` — en particulier
  toute SST flushée pendant le merge (le cœur d'ENG-COR-001).
- **INV-VS-6 — Suppression différée.** Aucune SST n'est physiquement
  supprimée tant qu'un `Arc<Version>` vivant la référence ; un échec de
  suppression au drop est best-effort (l'orphelin est ramassé à l'ouverture
  suivante par la confrontation au manifest, J2), jamais une panique.
- **INV-VS-7 — Manifest ≡ V_next.** Le `manifest.meta` publié liste
  exactement les ids de `V_next`, et `manifest_generation` est strictement
  croissante sous le publieur unique sérialisé.
**Relation aux ADR existants** : ferme le gap explicitement laissé ouvert par
ADR-039 §7 (*« Le manifest des SST vivantes… reste le périmètre d'ADR-040/N9
»*) et re-signalé non fermé par ADR-040/N9 (`corruption_smoke.rs`) et N11.3
(`docs/status.md`, `docs/PLAN-NATIVE-ENGINE.md` ligne 819 : *« un
manifest/version-set (candidat naturel : N13/ADR-043) reste nécessaire »*).
Prolonge le patron « petit fichier marqueur, tmp+fsync+rename, absence/version
inattendue = signal typé » déjà posé par `store.meta` (ADR-039 §7) et
`generation.meta` (ADR-042 §3, N12) — ADR-043 l'applique à un niveau plus fin
et plus fréquent (chaque flush/compaction, pas seulement chaque rotation
complète de clé). N'amende ni ADR-025 (fondation LSM) ni ADR-039 (format SST
par blocs) : le format des SST elles-mêmes ne change pas, seule la manière
dont l'ensemble des SST vivantes est publié et référencé change. C'est le
jalon **N13** du programme production-hardening
(`docs/PLAN-NATIVE-ENGINE.md` §10).

## Contexte

Vérifié dans le code réel (pas supposé) :

- `Engine` (`crates/basemyai-engine/src/store/engine.rs:153-192`) n'a **aucun
  verrouillage interne** : `get`/`scan_prefix`/`scan_range`/`scan_range_page`
  prennent `&self` (`engine.rs:703,735,761,805`), `flush`/`compact_now`/
  `apply_batch` prennent `&mut self`. Toute la concurrence est déléguée à
  l'appelant.
- Le seul appelant en production, `NativeInner`
  (`crates/basemyai/src/storage/native_store/mod.rs:135`), l'enveloppe dans
  un `std::sync::RwLock<NativeInner>` (`mod.rs:69,135`) : les lectures pures
  prennent un verrou de lecture concurrent (`with_inner_read`, mesuré ~3×
  plus rapide que séquentiel sur 64 lectures mixtes — doc module
  `mod.rs:41-59`), les écritures un verrou d'écriture exclusif
  (`with_inner`, `mod.rs:433-445`), toujours pris et relâché **à l'intérieur**
  d'un `tokio::task::spawn_blocking`, jamais tenu à travers un `.await`.
- **Le problème concret** : `Engine::flush` déclenche `compact()`
  automatiquement dès que `self.ssts.len() > compaction_sst_threshold`
  (`engine.rs:895-897`, seuil par défaut 4,
  `EngineOptions::default().compaction_sst_threshold = 4`, `engine.rs:143`).
  `compact()` est un full-merge naïf : **toutes** les entrées vivantes de
  **toutes** les SST sont matérialisées dans un `BTreeMap` en RAM
  (`engine.rs:924-933`) avant réécriture — le même profil que le soak 1M de
  N11.4 (ADR-042 §3.2). Comme `flush`/`compact` exigent `&mut Engine`, cette
  passe complète s'exécute **sous le verrou d'écriture exclusif** de
  `NativeInner` : tout lecteur concurrent (`vector_ranking_ids`,
  `keyword_ranking_ids`, `recall_vector`, …) est bloqué pour toute la durée
  de la compaction, pas seulement pour la durée du flush qui l'a déclenchée.
  C'est exactement le manque documenté en toutes lettres par le module doc
  (`mod.rs:56-59` : *« Les écritures restent sérialisées entre elles… lever
  *ça* exigerait de faire du moteur lui-même un multi-écrivain, hors
  périmètre N5.5 »*) et par le critère de sortie du plan §10 (*« aucun reader
  bloqué pendant toute une compaction »*).
- **Aucun manifest des SST vivantes n'existe.** À l'ouverture,
  `Engine::open_inner` reconstruit `self.ssts` par **listage du répertoire**
  (`sst_block::scan_existing(&dir, …)`, `engine.rs:352`) — aucun fichier
  n'affirme indépendamment « ces N id sont l'ensemble vivant ». `compact()`
  supprime les anciennes SST immédiatement et en best-effort
  (`fs::remove_file`, `engine.rs:950`), sûr aujourd'hui uniquement parce que
  la compaction détient le seul accès exclusif à tout l'engine — rien
  n'empêcherait une SST vivante d'être orpheline (supprimée à tort, ou
  absente après un crash mal placé) sans qu'aucune vérification ne le
  détecte. Ce gap est **explicitement pinné par test** :
  `verify_full_logical_does_not_catch_a_deleted_sst_either`
  (`crates/basemyai-engine/tests/corruption_smoke.rs`, N11.3) prouve que même
  `verify_store` en mode `FullLogical` — le plus profond disponible
  (ADR-040) — ne détecte pas la suppression d'une SST vivante, faute de toute
  source de vérité indépendante listant les SST attendues. ADR-039 §7 avait
  déjà anticipé et nommé ce manque (*« Le manifest des SST vivantes… reste le
  périmètre d'ADR-040/N9 »*), et réservé un failpoint pour ça
  (`before_manifest_publish`) — repris entre-temps par la publication de
  `store.meta` (ADR-039 §7, N8.9), qui ne liste **que** le format, jamais les
  fichiers.
- **Aucun group commit** : `Wal::append`/`append_batch`
  (`crates/basemyai-engine/src/store/wal.rs:115-159`) font chacun exactement
  un `write_all` + un `sync_all()` par appel. Chaque `put`/`delete`/
  `apply_batch` de `NativeInner` acquiert son propre verrou d'écriture
  exclusif via un `spawn_blocking` séparé (`mod.rs:433-445`) — sous une
  charge d'écritures concurrentes (agent multi-thread, ingestion batch),
  chaque appel paie son propre fsync, alors qu'un fsync couvre déjà un batch
  entier de records (`wal.rs:120-127`). C'est le levier mesuré comme
  « suivant côté débit » par la baseline N7
  (`docs/benchmarks/n7-engine-baseline-2026-07-10.md` §Ce que la baseline
  prouve) et cité comme dette N13 §10.4 par l'analyse de latence N6
  (`docs/benchmarks/n6-recall-latency-analysis-2026-07-17.md`).

Trois manques distincts, une seule racine commune : l'ensemble des SST
vivantes n'est ni **publié comme un fait indépendant sur disque** (manifest),
ni **référencé de façon à survivre à un remplacement concurrent** (version
set + snapshot). Tant que ces deux propriétés manquent, compaction et lecture
concurrente ne peuvent être découplées sans risquer une lecture sur un
fichier déjà supprimé.

## Décision

### 1. Manifest des SST vivantes — ferme le gap ADR-039 §7 / N11.3

Nouveau petit fichier, un par **génération** (le répertoire introduit par
ADR-042 §3 — `gen-<n>/` pour un store roté, la racine pour un store en
génération 0), nommé `manifest.meta` (nom à confirmer en implémentation,
même réserve que `generation.meta` en ADR-042 §3.2) :

```text
magic:              u32
manifest_generation: u64   // incrémenté à chaque publication (flush qui
                            // ajoute une SST, ou compaction qui en remplace)
live_sst_ids:       Vec<u64>  // longueur-préfixée, ids des SST constituant
                               // l'ensemble vivant actuel — ordre du plus
                               // ancien au plus récent, même invariant que
                               // `Engine::ssts` aujourd'hui
crc32:              u32
```

- Publié par le **même idiome** que `store.meta`/`crypto.meta`/
  `generation.meta` : tmp+fsync+rename, jamais d'écriture in-place.
- Publié à **chaque** mutation de l'ensemble de SST vivantes : après
  `flush()` pousse une nouvelle SST (`engine.rs:892`) et après `compact()`
  remplace l'ensemble (`engine.rs:943`) — deux nouveaux sites d'appel,
  toujours **après** que la SST elle-même est fsyncée et renommée
  durablement (même règle d'ordre qu'ADR-025 pour WAL vs SST).
- Nouveau failpoint dédié `before_sst_manifest_publish` (distinct de
  `before_manifest_publish`, qui reste le site de publication de
  `store.meta` — ADR-039 §7 l'avait réservé pour cet usage précis avant que
  N8.9 ne le réattribue ; réutiliser le même nom ici prêterait à confusion
  entre deux fichiers différents).
- **À l'ouverture** : `scan_existing` continue de lister le répertoire (elle
  reste nécessaire pour détecter un orphelin issu d'un crash), mais le
  résultat est désormais **vérifié contre** `manifest.meta` plutôt
  qu'accepté tel quel :
  - un id présent dans le manifest mais absent du disque ⇒
    `EngineError` typée (nouvelle variante, ex. `MissingLiveSst { id, path
    }`) — **c'est exactement le trou que N11.3 a pinné comme non détecté**,
    fermé par construction plutôt que par heuristique de `verify`.
  - un id présent sur disque mais absent du manifest ⇒ orphelin, silencieux
    et attendu (crash entre l'écriture de la SST et la publication du
    manifest) — supprimé en best-effort à l'ouverture, même posture que le
    nettoyage best-effort de `gen-<n>/` orphelin en ADR-042 §3.2.
  - absence totale de `manifest.meta` sur un store créé par ce build ⇒
    corruption typée ; absence sur un store pré-N13 ⇒ reconstruit une seule
    fois à la première ouverture en écriture (même politique additive que
    `StoreMeta:1→2` en ADR-039 : le manifest est stampé, pas rétro-imposé
    comme prérequis d'ouverture en lecture).
- `verify`/`repair` (ADR-040) gagnent un mode de vérification structurel
  supplémentaire : le manifest devient la source de vérité que
  `FullLogical` peut confronter au disque, fermant précisément le gap que
  N11.3 a documenté comme non couvert par le mode le plus profond existant.

### 2. Version set immuable + snapshots de lecture *(réécrit par l'amendement ENG-COR-001 ci-dessus)*

Remplace la mutation in-place de `self.ssts: Vec<BlockSstFile>`
(`engine.rs:170`, `mem::replace` en `engine.rs:1013`) par une structure
`Version` immuable, publiée atomiquement, avec un handle partagé **par
fichier** :

```rust
struct SstHandle {
    file: BlockSstFile,   // descripteur immuable ; ouvre le fichier à la demande
    retired: AtomicBool,  // armé par l'edit qui retire cet id du manifest
}
struct Version {
    manifest_generation: u64,
    ssts: Vec<Arc<SstHandle>>, // oldest → newest, même invariant qu'aujourd'hui
}
```

- `Engine` détient `current: Arc<Version>` au lieu de `ssts: Vec<...>`
  directement. `get`/`scan_*` travaillent sur une référence stable prise au
  début de l'appel — **jamais** sur `self.current` relu en cours de route.
- **Toute publication est un `VersionEdit { added, deleted }`** appliqué au
  `Version` courant au moment du commit :
  `V_next = (V_current ∖ deleted) ∪ added`, les `deleted` identifiés par id
  et **validés présents** dans `V_current` (INV-VS-4). Un flush est l'edit
  `{ added: [S_new], deleted: [] }` ; une compaction est l'edit
  `{ added: [S_out], deleted: inputs }` où `inputs` sont les ids de son
  ensemble d'entrée. Le manifest publié liste exactement les ids de
  `V_next` (INV-VS-7) ; le remplacement de `self.current` est strictement
  postérieur à la publication de `manifest.meta` (§1), même ordre que
  WAL-après-SST en ADR-025.
- **`Engine::snapshot() -> Snapshot`** : nouvelle méthode publique,
  `Arc::clone(&self.current)` enveloppé — un **snapshot S1** (audit §6) :
  il fige les *fichiers* SST, pas la *vue* (la memtable reste vivante et
  n'est pas capturée) ; ses lectures ne voient que les couches SST du
  `Version` figé. Un appelant qui a besoin d'un ensemble de fichiers stable
  pour une opération longue (merge de compaction J4, audit d'intégrité)
  l'obtient sans tenir aucun verrou au-delà de l'appel qui produit le
  `Snapshot`.
- **Suppression différée, jamais immédiate** : contrairement à
  `compact()` aujourd'hui (`fs::remove_file` inline, `engine.rs:1031`), une
  SST retirée par un edit est marquée `retired`, et son fichier n'est
  physiquement supprimé qu'au drop du **dernier** `Arc<SstHandle>` — donc
  quand plus aucun `Version` (ni snapshot) ne la référence. Un handle
  jamais `retired` ne supprime rien à son drop (le drop de l'`Engine` ne
  détruit pas le store). Échec de suppression = best-effort : l'orphelin
  est ramassé à l'ouverture suivante (confrontation manifest, J2). Aucune
  primitive nouvelle de bas niveau — `Arc`/`Drop` reste le mécanisme, à la
  granularité fichier où il est correct.
- Invariant recherché, formulé comme celui d'ADR-042 §3.3 pour éviter le
  même piège (« exactement un » est faux dans la fenêtre où deux générations
  coexistent) : **à tout instant, le `Version` que `manifest.meta` désigne
  est intégralement lisible ; tout `Version` antérieur encore en mémoire
  reste lisible jusqu'au dernier snapshot qui le référence, puis ses SST
  exclusives sont supprimées — jamais avant.**

### 3. Compaction concurrente *(réécrit par l'amendement ENG-COR-001 ci-dessus ; spécifié ici, implémenté en deux temps J3 puis J4)*

Avec §1/§2 en place, `compact()` change de forme :

- La passe de merge (matérialisation en RAM, écriture de la nouvelle SST) se
  fait à partir d'un **snapshot** du `Version` courant — elle **ne retient
  pas** le verrou d'écriture de `NativeInner` pendant cette passe. Les
  écritures entrantes (`put`/`delete`/`apply_batch`) continuent d'être
  acceptées dans le memtable/WAL sous de brèves acquisitions de verrou
  séparées, exactement comme aujourd'hui pour les lectures concurrentes
  (`mod.rs:46-51`).
- L'étape finale, sous exclusion brève (verrou d'écriture le temps d'une
  rename + d'un remplacement d'`Arc`, pas le temps du merge complet),
  publie le **`VersionEdit` de la compaction** —
  `{ added: [S_out], deleted: inputs }` — appliqué au `Version` courant
  **à cet instant-là**, jamais au snapshot d'entrée du merge : d'abord
  validation `deleted ⊆ V_current` et calcul de
  `V_next = (V_current ∖ deleted) ∪ added`, puis `manifest.meta` listant
  `V_next`, puis bascule de `self.current`.
- Une SST écrite par un `flush()` survenu **pendant** la compaction (donc
  absente de l'ensemble d'entrée du merge) est **conservée d'office** : le
  flush l'a publiée dans le `Version` courant avant le commit de la
  compaction, elle n'est pas dans `deleted` (les `deleted` sont exactement
  les inputs du merge), donc elle est dans `V_next` et dans le manifest
  (INV-VS-5) — la compaction suivante la ramassera. C'est la correction
  d'ENG-COR-001 : le texte original publiait la sortie du merge comme
  ensemble complet, ce qui aurait reclassé cette SST orpheline (et ses
  données perdues à la réouverture).
- **J3** implémente ce protocole (edit, validation, suppression différée)
  sous le verrou exclusif existant — `V_current` au commit y est
  trivialement le snapshot d'entrée. **J4** sort le merge du verrou sans
  changer le protocole, et active le test flush-pendant-compaction.
- Critère de sortie du plan §10 rendu concret (J4) : **aucun lecteur
  (`get`, `scan_*`, ou un `Snapshot` déjà pris) n'est bloqué pendant toute
  la durée d'une compaction** — seulement pendant la bascule finale, dont
  le coût est O(1) (remplacement d'`Arc` + une rename), pas O(données
  vivantes).

### 4. Writer pipeline — group commit

- Un unique thread/verrou dédié à l'écriture du WAL (déjà de facto le cas :
  un seul writer actif à tout instant via le verrou d'écriture exclusif de
  `NativeInner`) accumule les `put`/`delete`/`apply_batch` qui arrivent
  pendant qu'un fsync WAL est déjà en cours, et les publie en **un seul**
  `Wal::append_batch` + **un seul** `sync_all()` — au lieu d'un fsync par
  appel entrant (`wal.rs:115-159` aujourd'hui). Le contrat de durabilité par
  appelant est inchangé : chaque appelant continue d'attendre son propre
  fsync avant de considérer son écriture durable, seul le **nombre** de
  fsyncs physiques baisse sous contention.
- Reste **mono-écrivain logique** — le group commit fusionne des fsyncs, il
  ne fait pas de deux `Engine` actifs simultanément. Aucun changement au
  verrou advisory inter-process introduit par ADR-042 §3.2.
- Mesuré, pas supposé : le gain doit être chiffré (le plan §10 l'exige comme
  critère de sortie explicite — *« gain mesuré du group commit »*) avant
  d'être considéré acquis, sur le même harnais que la baseline N7
  (`docs/benchmarks/n7-engine-baseline-2026-07-10.md`).

### 5. Multi-writer complet — explicitement hors périmètre de cette PR

Repris tel quel du plan (§10, § 2 tableau ligne N13) : un second écrivain
concurrent n'est construit **que si mesuré nécessaire** une fois group
commit + compaction concurrente + `forget_many` (déjà livré, ADR-041) en
place — jamais pour « afficher une feature ». Aucune primitive multi-writer
(partitionnement de memtable, verrouillage à grain fin sur le WAL) n'est
proposée par cet ADR. Si le besoin se confirme, le point de départ naturel
est le `Version`/manifest décrit ici (comme ADR-042 §3.4 le notait déjà pour
la rotation : le chemin d'évolution connu-bon vers un multi-writer passe par
un tag de clé/génération par fichier au-dessus d'un version set — c'est
exactement la structure que §1/§2 posent).

## Alternatives rejetées

- **Verrouiller à grain fin par SST au lieu d'un version set immuable** :
  rejeté — exigerait un état mutable partagé par fichier (verrou par SST),
  une classe de bugs (deadlock entre le verrou de compaction et celui de
  lecture) que le `Version` immuable élimine par construction : un lecteur
  ne voit jamais un état partiellement muté, il voit un `Arc` figé ou un
  autre `Arc` figé, jamais un entre-deux.
- **Repousser le manifest à l'implémentation de la compaction concurrente,
  au lieu de le faire d'abord** : rejeté — le manifest ferme un gap de
  sécurité des données déjà pinné par test (N11.3) **indépendamment** de la
  concurrence ; le construire en premier (§1 avant §2/§3) donne une valeur
  livrable même si la compaction concurrente devait être repoussée.
- **GC immédiat par comptage de références atomique au lieu de suppression
  différée liée au `Drop` de `Version`** : envisagé, écarté pour la première
  passe — un compteur atomique par SST ajoute un état mutable partagé de
  plus à synchroniser correctement (incrément/décrément à chaque clonage/
  drop de `Snapshot`) là où laisser `Arc`/`Drop` faire ce travail (le
  compteur de références **est** déjà le mécanisme, gratuit, testé par le
  compilateur) suffit et ne demande aucune primitive nouvelle. Une
  alternative purement RAM (rien ne référence formellement les SST sur
  disque au-delà du process) reste acceptable ici car le manifest (§1) est
  la source de vérité durable — la suppression physique différée n'a besoin
  d'aucune trace sur disque de son propre état.
  *Révision (amendement ENG-COR-001, 2026-07-20)* : la moitié « `Drop` sur
  `Version` » de cette puce était insoutenable telle quelle — une SST est
  partagée entre `Version`s successifs (chaque flush produit
  `V1 = V0 ∪ {S_new}`), donc le Drop d'un `Version` ne peut pas savoir si
  un autre `Version` vivant lit encore ses SST. Ce qui est retenu (voir §2
  amendé) garde exactement l'esprit de la puce — `Arc`/`Drop` comme seul
  mécanisme, aucun compteur manuel incrémenté à la main — mais à la
  granularité *fichier* (`Arc<SstHandle>` partagé), la seule où il est
  correct.
- **Construire le multi-writer directement dans cette PR** (le lecteur
  naturel pourrait supposer que « version set + snapshots » implique déjà un
  multi-writer) : rejeté explicitement — voir §5 ; le plan proscrit
  d'anticiper une machinerie « pour afficher une feature », et group commit
  seul peut déjà fermer l'essentiel de l'écart de débit mesuré par la
  baseline N7 sans le risque d'un vrai multi-écrivain.
- **Réutiliser le nom de failpoint `before_manifest_publish` pour le nouveau
  manifest de SST** : rejeté — ce nom désigne déjà, depuis N8.9, la
  publication de `store.meta` (un fichier différent, un rôle différent) ;
  le réutiliser pour un second fichier romprait la correspondance 1:1
  nom-de-failpoint ↔ site-de-publication que le reste du moteur maintient
  (`crypto.meta`, `generation.meta` ont chacun leurs propres sites).

## Conséquences

- (+) Ferme un gap de sécurité des données documenté et pinné par test
  depuis N9/N11.3 (suppression silencieuse d'une SST vivante non détectée
  même par `verify --logical`), indépendamment de tout gain de concurrence.
- (+) Débloque le critère de sortie N5.5/N13 déjà écrit dans le module doc
  du store natif (`mod.rs:56-59`) sans faire du moteur un multi-écrivain :
  la compaction cesse de bloquer les lecteurs pour sa durée complète.
- (+) Réutilise exclusivement des idiomes déjà en place dans le moteur
  (tmp+fsync+rename, `Arc`/`Drop` pour la durée de vie, group commit comme
  fusion pure de fsyncs) — même discipline « aucune primitive cryptographique
  ou de bas niveau nouvelle » qu'ADR-042 §3.2 a appliquée à la rotation
  complète.
- (−) Un `Version` remplacé mais encore référencé par un snapshot long
  retient ses SST sur disque au-delà de leur remplacement logique — un
  snapshot qui ne se libère jamais (bug appelant, ou opération anormalement
  longue) devient un espace-leak potentiellement non borné. À documenter et,
  si mesuré nécessaire, borner par une durée de vie maximale de snapshot en
  implémentation — pas un critère de sortie de cette PR de décision.
- (−) Le manifest ajoute une écriture disque (tmp+fsync+rename) à **chaque**
  flush, pas seulement à chaque compaction — coût à mesurer contre la
  baseline N7 (`docs/benchmarks/n7-engine-baseline-2026-07-10.md`) avant de
  clore N13 ; s'il s'avère significatif, un batching de la publication (un
  manifest par groupe de flushes plutôt que par flush) reste une option de
  suivi, pas décidée ici.
- (−) Group commit introduit une fenêtre de latence ajoutée pour le premier
  appelant d'un groupe (il peut attendre qu'un second appelant arrive avant
  que le fsync parte) — classique de tout group commit, à borner par un
  délai maximal configurable en implémentation, mesuré avant d'être figé en
  dur.

## Critères de sortie (adaptés du plan §10, rendus testables)

- [ ] Sous charge mixte (écritures continues déclenchant flush/compaction +
  lectures concurrentes), aucune lecture (`get`/`scan_*`/un `Snapshot` déjà
  pris) n'est jamais bloquée par une compaction en cours — mesuré par un
  test qui lance une compaction longue (dataset volumineux) et confirme que
  des lectures concurrentes complètent en O(lecture normale), pas O(durée de
  la compaction).
- [ ] Une SST vivante supprimée manuellement du disque (même scénario que
  `verify_full_logical_does_not_catch_a_deleted_sst_either`) est désormais
  détectée à l'ouverture **et** par `verify --logical`, via le manifest —
  test qui remplace/étend le test N11.3 existant pour affirmer la détection
  positive plutôt que documenter l'absence de détection.
- [ ] Aucune SST n'est supprimée du disque tant qu'un `Snapshot` la
  référence encore — test qui prend un snapshot, déclenche une compaction
  qui la remplacerait, vérifie que le fichier existe toujours et reste
  lisible par le snapshot, puis vérifie sa suppression après libération du
  snapshot.
- [ ] Crash injecté (fail-point `before_sst_manifest_publish` et aux autres
  étapes de flush/compaction) laisse toujours soit l'ancien `manifest.meta`
  (ancien `Version` intégralement lisible), soit le nouveau (nouveau
  `Version` intégralement lisible) — jamais un état où le manifest référence
  un id absent du disque.
- [ ] Gain du group commit mesuré et documenté (nombre de fsyncs physiques
  sous N écritures concurrentes, avant/après) sur le harnais `engine_bench`
  existant — comparé à la baseline N7.
- [ ] Latences p99 (lecture, écriture, compaction) documentées sous charge
  mixte, comparées à la baseline N7.
- [ ] `cargo xtask ci` vert, `cargo xtask test-crash-consistency` étendu aux
  nouveaux sites de fail-point manifest.
- [ ] Nouvelle entrée `format.lock` pour `Manifest:1` (ou nom retenu en
  implémentation), documentée dans le module doc au même niveau de détail
  que `store.meta`/`generation.meta`.

## Points signalés pour revue humaine avant implémentation

- **Nom du fichier et de la struct** (`manifest.meta`/`Manifest` proposés
  ici) — à confirmer, même réserve que `generation.meta` en ADR-042.
- **Granularité de suppression différée** : `Arc`/`Drop` par `Version`
  (proposé) contre compteur de références explicite par SST — la première
  option est plus simple et suffit tant qu'aucun besoin de diagnostic fin
  (« combien de snapshots retiennent cette SST précise ») n'apparaît.
- **Fréquence de publication du manifest** (chaque flush contre batché) —
  dépend de la mesure du coût réel, non tranchée ici (voir Conséquences).
- **Délai maximal de group commit** — valeur par défaut non proposée ici,
  à mesurer en implémentation comme les paramètres Argon2id d'ADR-042 l'ont
  été (proposition motivée, pas un nombre gravé avant benchmark réel).
