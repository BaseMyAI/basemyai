# Audit architectural et de sûreté — `basemyai-engine` (2026-07)

Audit contradictoire du moteur de stockage natif (`basemyai-engine`) et de son
intégration dans `basemyai`, mené avant l'ouverture de N13. Objectif : trouver,
prouver et hiérarchiser tout ce qui peut provoquer une perte silencieuse, une
incohérence, un faux sentiment de durabilité, une contention évitable ou une
consommation non bornée — **le code réel comme seule source de vérité**, jamais
un ADR ou un changelog.

---

## 0. État exact audité

| | |
|---|---|
| Date de l'audit | 2026-07-19 |
| Branche | `dev` |
| Commit | `0e742f823aa49a15aebf29028fc71325df4e561d` (`docs: close out R1.8…`) |
| Working tree | **Non propre**, mais les modifications ne touchent pas le moteur : `.github/workflows/ci.yml`, `Cargo.toml` racine, `basemyai-cli/*` (câblage eval), `basemyai-eval/*`, `xtask`, docs. **`crates/basemyai-engine/` et `crates/basemyai/src/storage/` sont identiques à HEAD.** |
| Fichiers non committés pertinents | `docs/adr/ADR-043-native-version-set-snapshots-and-concurrent-compaction.md` (brouillon, évalué en §6/§9 comme *hypothèse*, pas comme décision) |
| Rust | `rustc 1.95.0 (2026-04-14)`, `cargo 1.95.0` |
| OS | Windows 11 Home 10.0.26200 (audit statique valable toutes plateformes ; les points POSIX-spécifiques sont signalés comme tels) |
| Commandes exécutées | voir §12 |

Toutes les références `path:ligne` de ce rapport pointent dans l'état ci-dessus.

---

## 1. Executive summary

**Existe-t-il un risque de perte silencieuse ?** Oui, trois chemins distincts,
tous liés à la même racine : *l'ensemble des fichiers vivants n'est publié
nulle part comme un fait durable*.

1. Une SST vivante supprimée (utilisateur, antivirus, backup défaillant, crash
   mal placé) est **invisible** à `open` comme à `verify --logical` — gap
   connu, épinglé par deux tests qui documentent l'absence de détection
   (ENG-DUR-001, P0).
2. Après une compaction, la suppression des anciennes SST est best-effort et
   sans ordre durable : un échec partiel (ou un réordonnancement des unlinks
   par le FS après crash) peut **ressusciter une clé supprimée** — la SST
   fusionnée n'a plus le tombstone, l'ancienne SST porteuse de la valeur est
   toujours là (ENG-DUR-002, P0).
3. Aucun `fsync` de répertoire parent après aucun `rename` du moteur. Le
   commentaire de `write_new` qui déclare ce manque « pas un gap de
   correction » repose sur un ordre (rename durable avant troncature WAL) que
   rien ne garantit au niveau FS (ENG-DUR-003, P1). Cas extrême : la fenêtre
   de rotation complète où la perte du rename du pointeur de génération +
   la GC déjà exécutée conduisent `open` à **recréer un store vide et à
   supprimer `gen-N` avec toutes les données** (ENG-DUR-004, P1).

**Existe-t-il un risque de lecture incohérente ?** Au niveau d'un appel
produit : non — chaque méthode `MemoryStore` s'exécute intégralement sous une
seule acquisition du `RwLock`. Au niveau multi-appels (recall = ranking
vectoriel + ranking BM25 + hydrate + touch = 4 acquisitions distinctes), la
vue peut mélanger jusqu'à 4 états — toléré aujourd'hui, jamais spécifié
(ENG-COR-002, ⚪).

**Garanties solides (prouvées par le code + tests réels) :**

- Durabilité par appel au niveau *fichier* : WAL `write_all` + `sync_all`
  avant retour (`store/wal.rs:139-161`), batch = un enregistrement, un
  checksum, un fsync — atomicité de batch prouvée par crash-tests réels
  (kill-loop 7 modes, clair + chiffré) et model-based tests.
- Ordre flush : SST fsyncée et renommée **avant** troncature WAL
  (`store/engine.rs:876-899`), au niveau *programme*.
- Composition d'index : `put_many`/`forget`/`update`/`touch` = **un** batch
  WAL couvrant record + vecmap + index temporel + FTS + nœuds vectoriels +
  allocateur (`idx/memory/persistent.rs:210-271,293-327,337-356`). L'état RAM
  des index n'est mis à jour **qu'après** le succès d'`apply_batch`
  (`idx/vector/persistent.rs:352-358`) — aucune divergence RAM/disque sur
  erreur.
- Générations crypto : AAD liée à la génération, testée jusqu'au niveau AEAD
  (`engine.rs:1488-1580`), bornes d'abort de rotation couvertes par le
  kill-loop.
- Wire-distrust systématique : 24 cibles fuzz (une par décodeur, matrice
  nightly complète vérifiée dans `fuzz.yml`), troncature/bit-flip testés « à
  chaque coupe ».

**Garanties seulement accidentelles (tenues par le gros verrou) :** l'absence
de résurrection pendant la compaction, la sûreté de `next_sst_id` recalculé
par listage, la suppression immédiate des SST remplacées, la stabilité de la
vue d'un scan — tout cela ne tient que parce que `compact()` détient l'accès
exclusif à l'engine entier et qu'aucun autre acteur ne touche le répertoire.
Toute concurrence future (N13) invalide ces protections implicites si le
catalogue durable n'arrive pas *d'abord*.

**N13 peut-il commencer ?** Oui, **redécoupé** (recommandation **B**, §13) :
le manifest est justement ce qui ferme les P0 — mais (a) deux corrections
préalables minuscules doivent passer avant (fsync répertoire + garde-fou
« pointeur absent mais `gen-N` présent = erreur »), et (b) le brouillon
ADR-043 contient un défaut de conception dans sa §3 (publication de la
compaction par *remplacement complet* du version set : une SST flushée pendant
la compaction serait retirée de l'ensemble vivant — perte de données) qui doit
être corrigé en « version edit » avant implémentation (ENG-COR-001, P1).

**Faut-il un snapshot complet ?** Non. Le besoin réel est **S1** (ensemble de
SST figé par `Arc<Version>`) — nécessaire et suffisant pour la compaction
concurrente. S2 (memtable figée, séquences) n'est exigé par aucun chemin
produit actuel ; les clés internes versionnées et le MVCC ne sont pas
justifiés (§6).

**Manifest SST seul ou catalogue plus large ?** Manifest par génération
listant les SST vivantes + un compteur de publication, rien de plus (§7). Ni
le WAL (nom fixe, unique par génération), ni des séquences (liées à S2, non
requis), ni l'allocation (dérivable du manifest) n'ont besoin d'y figurer —
tant que WAL segmenté et S2 n'existent pas.

**Cinq priorités absolues :**

1. ENG-DUR-003 — fsync du répertoire parent après chaque rename de
   publication (correctif minuscule, débloque la validité de tout le reste).
2. ENG-DUR-004 — garde-fou d'ouverture : pointeur de génération absent +
   répertoires `gen-N` présents ⇒ erreur typée, jamais « génération 0 + GC ».
3. ENG-DUR-001 — manifest des SST vivantes (le §1 du draft ADR-043, corrigé).
4. ENG-DUR-002 — suppression différée/pilotée par manifest des SST
   remplacées (ferme la résurrection ; vient mécaniquement avec le manifest à
   l'open, mais le chemin d'erreur de `remove_file` doit être testé).
5. ENG-COR-001 — corriger le protocole de publication de compaction du draft
   ADR-043 (version *edit*, pas remplacement) avant toute implémentation.

---

## 2. Architecture réellement observée

### 2.1 Écriture simple (`put`/`delete`)

```text
NativeMemoryStore::put_memory (trait_impl.rs:198)
→ spawn_blocking + RwLock.write() pris DANS la closure (mod.rs:427-441)
→ PersistentMemoryIndex::put_many → compose UN Batch (persistent.rs:210-271)
→ Engine::apply_batch (engine.rs:673-696)
   → Wal::append_batch : encode → [seal AEAD] → seek(End) → write_all
     → sync_all (wal.rs:139-161)          ← POINT DE DURABILITÉ (fichier)
   → memtable.put/delete (RAM)            ← POINT DE LINÉARISATION
   → maybe_flush (seuil = 1000 ENTRÉES, pas d'octets — engine.rs:967-972)
→ retour appelant → relâche le verrou d'écriture
```

Le point de linéarisation réel est **la mise à jour de la memtable sous le
verrou d'écriture de `NativeInner`** — l'`Engine` lui-même n'a aucun verrou
interne ; `get`/`scan_*` prennent `&self`, `put`/`flush` `&mut self`
(engine.rs:644-717). Le point de durabilité est le `sync_all` du WAL — **au
niveau fichier seulement** ; la création du fichier `wal.log` lui-même dans
son répertoire n'est jamais synchronisée (voir ENG-DUR-003).

### 2.2 Batch atomique

`Batch` (engine.rs:53-111) : liste ordonnée d'ops, dernier gagne. Encodage :
un seul enregistrement WAL externe `WalOp::Batch` contenant les sous-ops,
un CRC couvrant le tout, **un** `write_all` + **un** `sync_all`
(wal.rs:120-137). Rejeté avant écriture si > `MAX_BATCH_OPS = 10 000`
(`format/wal.rs:162`) — borne en *ops*, pas en octets. Crash à toute étape :
le replay (wal.rs:65-109) s'arrête au premier enregistrement externe incomplet
et tronque la queue → tout-ou-rien pour le batch entier. Prouvé par
`encrypted_batch_kill_reopen_verify_loop` (kill réel) et le model-based test.

### 2.3 Lecture point (`get`, engine.rs:703-718)

```text
memtable (Some(v) | Some(tombstone) → stop | None → continuer)
→ SSTs du plus récent au plus ancien :
    bloom → recherche binaire dans l'index de blocs → UN bloc lu
    (cache de blocs consulté d'abord, clé (sst_id, block_no))
→ premier hit (valeur OU tombstone) gagne
```

Invariant « jamais un scan complet par lookup » instrumenté
(`point_lookup_full_sst_read`, engine.rs:184) et épinglé à zéro par test.

### 2.4 Scans

`scan_prefix`/`scan_range` (engine.rs:735-781) : merge SSTs oldest→newest puis
memtable dans un `BTreeMap` **entièrement matérialisé** ; seuls les blocs
chevauchant la plage sont décodés. `scan_range_page` (engine.rs:805-871) :
version bornée `O(sources × limit)` avec protocole de frontière correct
(chaque source lue jusqu'à `limit`, frontière = min des dernières clés des
sources tronquées, re-lecture au-delà). **Stabilité de vue : garantie
uniquement à l'intérieur d'un appel** — le protocole `next_start` entre pages
est cohérent *si l'état ne change pas entre les appels*, ce que seul le verrou
appelant assure. Les boucles de pagination du produit (`scan_for_forgetting`,
trait_impl.rs:130-163) bouclent **à l'intérieur d'une seule closure sous un
seul verrou de lecture** — donc cohérentes ; c'est une propriété de
l'appelant, pas du moteur.

### 2.5 Flush (engine.rs:876-899)

```text
memtable → Vec<(Key, Option<Value>)> (copie intégrale RAM)
→ BlockSstFile::write_new (sst_block.rs:309-428) :
    assemble TOUT le fichier en RAM (Vec file_bytes, :384-391)
    → écrit <id>.sst.tmp → write_all → sync_all           [durable: contenu]
    → fs::rename(tmp, <id>.sst)                            [PAS de fsync dir]
→ wal.reset() : set_len(0) + sync_all (wal.rs:175-190)     [durable: troncature]
→ next_sst_id += 1 ; ssts.push ; memtable.clear
→ si ssts.len() > compaction_sst_threshold (4) → compact() INLINE
```

Fenêtres de crash : avant rename → orphelin `.tmp` ignoré à l'open, WAL
rejoue (sain) ; entre rename et reset → SST + WAL coexistent, replay
idempotent (testé, `before_wal_truncate_leaves_sst_and_wal_coexisting…`) ;
**rename non durable + reset durable → perte du memtable entier**
(ENG-DUR-003 — aucune garantie d'ordre au niveau FS sans fsync répertoire).
Échec de `wal.reset()` après rename réussi : erreur retournée, memtable
conservée, `next_sst_id` non incrémenté, retry ré-écrase le même id — chemin
propre, testé (`io_faults.rs`). Ce point est une **force** du code actuel.

### 2.6 Compaction (engine.rs:924-957)

```text
full merge : TOUTES les SSTs, oldest→newest, dans un BTreeMap RAM
→ tombstones ÉLIMINÉS (sûr aujourd'hui : le merge couvre tout, il n'existe
  aucune couche plus ancienne — sûr UNIQUEMENT tant que c'est vrai)
→ write_new(id = next_sst_id)  [même séquence tmp/fsync/rename]
→ mem::replace(ssts, vec![new]) ; pour chaque ancienne :
    let _ = fs::remove_file(...)   ← best-effort, erreurs IGNORÉES,
    block_cache.invalidate_sst(id)    aucun ordre durable
```

Tout cela **sous le verrou d'écriture exclusif de `NativeInner`** (déclenché
depuis `flush()` lui-même appelé par `put`) : lecteurs ET écrivains bloqués
pendant O(store), RAM pic ≈ 2× données vivantes (BTreeMap + `file_bytes`).

### 2.7 Ouverture et récupération (`open_inner`, engine.rs:284-401)

```text
create_dir_all → verrou writer advisory (.basemyai.lock, try_lock,
  engine.rs:1023-1044) → check_or_create_store_meta (version gate ADR-039,
  refus typé des stores pré-format ; création tmp/fsync/rename)
→ resolve_active_generation (engine.rs:1140-1161) :
    generation.meta absent → (racine, gen 0)   ← AUCUN contrôle qu'aucun
    présent → gen-N obligatoire, sinon erreur     gen-N n'existe (ENG-DUR-004)
→ crypto.meta : présence = source de vérité du mode ; gen≠0 sans crypto.meta
  = erreur typée (bien) ; gen 0 sans crypto.meta + clé fournie + pas de
  wal/sst → CRÉE une crypto.meta neuve (fenêtre ENG-DUR-004)
→ ssts = scan_existing(dir) : LISTAGE du répertoire, tout <id>.sst chargé,
  orphelins .tmp ignorés (sst_block.rs:797-816)
→ next_sst_id = max(id)+1 — dérivé du CONTENU du répertoire
→ WAL replay torn-tail tolérant + troncature de la queue déchirée
→ gc_inactive_generations : SUPPRIME tout gen-N ≠ courant (engine.rs:1115-1134)
```

### 2.8 Rotation complète (`rotate_full`, engine.rs:539-640)

Construction complète de `gen-N+1` (crypto.meta neuve, merge intégral RAM
old-DEK → une SST new-DEK, WAL vide fsyncé) **avant** publication ; échec
avant publication = rollback par `remove_dir_all`, instance intacte ;
`publish_generation` = tmp/fsync/rename du pointeur (**sans fsync du
répertoire racine**) ; ensuite bascule mémoire infaillible puis
`gc_old_generation` best-effort qui **supprime immédiatement** wal.log,
crypto.meta et toutes les SST de l'ancienne génération (engine.rs:1071-1101).
Boundaries testées par failpoints + kill-loop (`full_rotation_abort_…`) — mais
uniquement pour des crashs *de processus*, jamais pour un réordonnancement de
métadonnées FS (voir ENG-DUR-004).

---

## 3. Guarantees matrix

| Garantie | Promise actuelle | Réalité du code | Preuve | Statut |
|---|---|---|---|---|
| `put`/`delete` durable au retour | « Durable once this returns Ok » (engine.rs:642) | Vraie au niveau fichier (fsync WAL) ; la *création* de `wal.log` dans son répertoire n'est jamais synchronisée — un crash très tôt après la création du store peut perdre le fichier entier sur certains FS | `wal.rs:139-161` ; aucun fsync dir nulle part (grep `sync_all` : uniquement sur fichiers) | 🟡 |
| Batch tout-ou-rien après crash | ADR-025 + doc `apply_batch` | Un enregistrement externe, un CRC, un fsync ; replay tout-ou-rien | `wal.rs:120-137,65-109` ; kill-loop batch clair+chiffré ; model-based | ✅ |
| Ordre SST-avant-truncate-WAL | ADR-025, doc `flush` | Vrai en ordre programme ; **non garanti en ordre de persistance FS** sans fsync répertoire après rename | `engine.rs:883-889` ; `sst_block.rs:406-414` (déviation auto-documentée, justification fausse) | 🟡 |
| Pas de résurrection d'une clé supprimée | model-based (`ever_deleted` re-vérifié après compaction) | Vraie sur chemin nominal ; **fausse si la suppression d'une ancienne SST échoue partiellement** (erreurs ignorées `let _ =`) ou si les unlinks sont réordonnés par un crash | `engine.rs:943-955` ; aucun test du chemin d'erreur de `remove_file` | 🔴 |
| SST vivante manquante détectée | — (jamais promis) | Indétectable à l'open ET par `verify --logical` ; épinglé par deux tests qui documentent le trou | `corruption_smoke.rs::deleted_sst_…_no_manifest_yet` ; `io_faults.rs:222-244` | 🔴 |
| Réouverture = état pré-crash | kill-loop | Vraie pour crash de processus (7 modes × ~20 cycles, CI Linux+Windows) ; jamais testée pour perte de métadonnées FS (power loss) | `tests/crash_consistency.rs` ; `ci.yml:181-199` | 🟡 |
| Lecture cohérente par appel produit | doc module `native_store` | Chaque méthode `MemoryStore` = une closure sous une acquisition unique du `RwLock` | `mod.rs:427-461` ; `trait_impl.rs` (toutes les méthodes) | ✅ |
| Vue cohérente multi-appels (recall fusionné, hybrides) | — (jamais spécifié) | 2 à 4 acquisitions distinctes par recall ; états potentiellement différents entre passes | `trait_impl.rs:253-298,434-463` | ⚪ |
| Isolation agent structurelle | ADR-006 | Clés préfixées par agent, post-filtre systématique, breach vérifiée par verify logique | `inner.rs:42-44` ; `verify_logical.rs` (`AgentIsolationBreach`) | ✅ |
| Ancienne clé inutilisable après rotation complète | ADR-042 | Prouvé jusqu'au niveau AEAD (vieux ctx + vieux crypto.meta vs WAL/SST neufs) | `engine.rs:1488-1580` | ✅ |
| Mono-écrivain inter-process | ADR-042 §3.2 | Verrou advisory OS tenu toute la vie de l'Engine | `engine.rs:1023-1044` | ✅ |
| `verify` read-only et fiable | ADR-040 | Read-only prouvé (snapshot répertoire) ; **mais ne prend pas le verrou writer** : verify d'un store actif peut lire des fichiers en cours de remplacement | `verify.rs` (aucune prise de verrou) ; `integrity.rs:17-21` | 🟡 |
| Index dérivés jamais publiés sans primaires | ADR-027 §3 | Un batch WAL unique par opération composée ; RAM mise à jour après succès seulement | `persistent.rs` (memory), `persistent.rs:331-483` (vector) | ✅ |

---

## 4. Findings

### ENG-DUR-001 — L'ensemble des SST vivantes n'a aucune existence durable indépendante

**Sévérité :** P0
**Catégorie :** Durabilité
**Statut :** Bug confirmé (épinglé par deux tests qui documentent l'absence de détection)

#### Constat

À l'ouverture, `self.ssts` est reconstruit par listage du répertoire
(`scan_existing`, engine.rs:352 → sst_block.rs:797-816) et `next_sst_id =
max(id)+1` (engine.rs:353). Aucun fichier n'affirme « ces N ids sont
l'ensemble vivant ». Conséquences vérifiées, au-delà de la seule SST
manquante :

- **Vivant/orphelin indistinguable** : un `<id>.sst` présent est vivant par
  définition ; un absent n'a jamais existé, par définition. Les deux
  définitions sont fausses après un crash mal placé ou une intervention
  externe.
- **Publication logique inexistante** : le « commit » d'un flush est le
  rename lui-même ; celui d'une compaction est la *suppression* des anciennes
  SST — une opération destructive fait office de publication.
- **`verify` structurellement aveugle** : la vue logique est reconstruite à
  partir des SST *présentes* — il n'existe rien à confronter.
- **Réparation impossible** : `repair` ne peut pas savoir qu'il manque
  quelque chose.
- **Compaction concurrente impossible à sécuriser** : sans fait durable
  « quel ensemble est vivant », toute suppression différée ou publication
  atomique n'a pas de fondation.

#### Preuve dans le code

- `crates/basemyai-engine/src/store/engine.rs:352-353` (`scan_existing` +
  `next_sst_id`)
- `crates/basemyai-engine/src/store/sst_block.rs:797-816` (listage)
- Tests documentant le trou :
  `tests/corruption_smoke.rs::deleted_sst_is_currently_silent_data_loss_no_manifest_yet`
  et `tests/io_faults.rs:222-244::verify_full_logical_does_not_catch_a_deleted_sst_either`
  (assertion : `report.healthy` **après** suppression d'une SST vivante).

#### Scénario de panne

```text
Store sain : 3 SSTs (0,1,2), WAL vide (flush + reset propres)
→ un backup/antivirus/utilisateur supprime 1.sst (ou un crash pré-N13
  laisse le répertoire dans un état où le rename de 1.sst n'a jamais persisté
  alors que le reset WAL a persisté — cf. ENG-DUR-003)
→ réouverture : scan_existing trouve (0,2) ; next_sst_id = 3 ; aucun signal
→ toutes les clés dont la version la plus récente vivait dans 1.sst ont
  silencieusement disparu OU sont revenues à une version antérieure (si 0.sst
  en portait une) — la seconde variante est pire : donnée ANCIENNE servie
  comme actuelle
→ verify --logical : « healthy »
```

#### Impact

Perte ou régression silencieuse de données, non détectable, non réparable.

#### Pourquoi l'architecture actuelle produit ce problème

Le moteur publie chaque *fichier* durablement (tmp/fsync/rename) mais ne
publie jamais *l'ensemble*. C'est le symptôme central du modèle « état logique
implicite » : WAL unique + memtable unique + SST découvertes physiquement +
compaction destructive-comme-commit.

#### Correction minimale

Le §1 du brouillon ADR-043 (manifest par génération : `manifest_generation` +
`live_sst_ids`, tmp/fsync/rename, publié après chaque flush/compaction,
vérifié à l'open : manifest∖disque = erreur typée, disque∖manifest = orphelin
supprimé). Voir §7 pour le contenu logique recommandé et les deux corrections
à apporter au brouillon.

#### Correction cible

Manifest + version set immuable (§6) — le manifest d'abord, livrable seul.

#### Test qui doit échouer avant correction

Les deux tests existants, retournés en assertions positives (suppression
détectée à l'open **et** par verify) — exactement ce que le brouillon ADR-043
prévoit dans ses critères de sortie.

#### ADR requis

ADR-043 (partie manifest), amendé — voir §9.

#### Dépendances

ENG-DUR-003 (le manifest lui-même doit être publié avec fsync répertoire,
sinon il reproduit le défaut qu'il corrige).

---

### ENG-DUR-002 — Résurrection possible de clés supprimées après compaction (suppression best-effort, sans ordre)

**Sévérité :** P0
**Catégorie :** Durabilité / Cohérence
**Statut :** Risque démontré (chemin d'erreur reproductible ; variante crash dépendante du FS)

#### Constat

`compact()` élimine **tous** les tombstones de la sortie (engine.rs:933 —
sûr uniquement si les anciennes couches disparaissent réellement), puis
supprime les anciennes SST une par une avec `let _ = fs::remove_file(...)`
(engine.rs:950) : erreurs **ignorées silencieusement**, aucune synchronisation
de répertoire, aucun ordre durable entre les unlinks.

#### Preuve dans le code

- `crates/basemyai-engine/src/store/engine.rs:943-955` (`let _ =
  fs::remove_file`, commentaire : « failing to remove … is a space leak, not
  a correctness issue » — **faux** dans le cas partiel démontré ci-dessous)
- Aucun test n'exerce un échec de `remove_file` pendant la compaction
  (`failpoints.rs::during_compaction_failure…` teste l'échec du *merge*, pas
  du nettoyage ; `io_faults.rs` teste des tmp en lecture seule, pas des
  suppressions refusées).

#### Scénario de panne

```text
SST-1 : put(k, v)          (flush ancien)
SST-2 : tombstone(k)       (delete flushé)
→ compaction : merge = {k absent} (tombstone éliminé), écrit SST-3 (durable)
→ suppression best-effort, ordre du Vec (oldest first) :
   remove_file(1.sst)  → ÉCHOUE (handle antivirus/backup/indexeur Windows —
                          sharing violation ; ou EACCES) → erreur IGNORÉE
   remove_file(2.sst)  → réussit
→ session courante : correcte (ssts en RAM = [SST-3] seule)
→ réouverture : scan_existing trouve (1, 3) ; get(k) :
   SST-3 (plus récente) → pas d'entrée, pas de tombstone → continuer
   SST-1 → k = v        → LA VALEUR SUPPRIMÉE EST DE RETOUR
```

Variante sans erreur : crash/power-loss entre les deux unlinks, ou après les
deux si le FS persiste l'unlink de 2.sst mais pas celui de 1.sst (aucun fsync
de répertoire n'ordonne quoi que ce soit). Même résultat.

#### Impact

Donnée supprimée qui revient — pour un moteur de *mémoire d'agent*, c'est un
`forget()` (potentiellement une demande de suppression utilisateur) qui se
défait silencieusement. Gravité aggravée par le contexte privacy-first du
produit.

#### Pourquoi l'architecture actuelle produit ce problème

L'élimination des tombstones dans la sortie est couplée à une hypothèse (« les
anciennes couches ont disparu ») que le mécanisme de suppression ne garantit
pas. La suppression physique *est* la publication logique — une opération
best-effort tient lieu de commit.

#### Correction minimale

Deux options indépendantes du manifest, applicables immédiatement :
(a) ne pas ignorer l'échec de `remove_file` — re-tenter, et si l'échec
persiste, **conserver les tombstones dans la SST fusionnée** de cette passe
(dégradation sûre) ou marquer le store « compaction incomplète » ;
(b) ordonner : ne supprimer les anciennes qu'après un fsync répertoire
consécutif au rename de la fusionnée.

#### Correction cible

Manifest (ENG-DUR-001) : à l'open, une SST hors manifest est un orphelin
*ignoré puis supprimé* — SST-1 résiduelle devient inoffensive par
construction. La suppression différée du version set (§6) ferme le reste.

#### Test qui doit échouer avant correction

Failpoint sur `remove_file` de la première ancienne SST (échec injecté) +
réouverture + `get(k)` : doit rester `None`. Aujourd'hui ce test retournerait
`Some(v)`.

#### ADR requis

Couvert par ADR-043 amendé (publication logique) ; la correction minimale (a)
n'exige aucun ADR.

#### Dépendances

Aucune pour la correction minimale ; la cible dépend d'ENG-DUR-001.

---

### ENG-DUR-003 — Aucun fsync de répertoire parent après aucun rename ; la justification du code est invalide

**Sévérité :** P1
**Catégorie :** Durabilité
**Statut :** Risque démontré (plateforme-dépendant ; non reproduit en pratique sur ext4/NTFS ordonnés, garanti nulle part par POSIX)

#### Constat

Tous les sites de publication — SST (`sst_block.rs:406`), `store.meta`
(engine.rs:1229), `generation.meta` (engine.rs:1067), `crypto.meta`
(`crypto.rs`) — font tmp → `sync_all` → `rename` **sans jamais synchroniser
le répertoire parent**. La déviation est auto-documentée
(sst_block.rs:408-414) avec la justification : « si le rename ne survit pas,
la SST n'existe simplement pas et les données rejouent depuis le WAL non
tronqué ». Cette justification suppose que *si la troncature WAL a persisté,
alors le rename antérieur a persisté aussi* — un ordre que POSIX ne garantit
pas sans fsync du répertoire : le rename est une mutation du *répertoire*,
la troncature une mutation de *l'inode du WAL* ; ce sont des métadonnées
indépendantes que le FS peut retirer dans n'importe quel ordre. C'est
précisément le piège documenté par l'étude ALICE (Pillai et al., OSDI'14,
« Crash-Consistency: All File Systems Are Not Created Equal ») et la raison
pour laquelle LevelDB/RocksDB fsyncent le répertoire après le rename du
MANIFEST/CURRENT (LevelDB `env_posix.cc` : `SyncDirIfManifest` ;
RocksDB wiki, « fsync the directory after creating new files »).

#### Preuve dans le code

- `crates/basemyai-engine/src/store/sst_block.rs:406-414` (rename + déviation
  documentée)
- `crates/basemyai-engine/src/store/engine.rs:888-889` (`fail_point!` puis
  `wal.reset()` immédiatement après le retour de `write_new`)
- `engine.rs:1050-1069` (`publish_generation`), `engine.rs:1214-1231`
  (`write_store_meta`) — même motif
- Aucun test possible in-process (le réordonnancement exige une couche
  d'injection type dm-log-writes) — absence vérifiée dans `tests/`.

#### Scénario de panne

```text
1000 puts → flush automatique :
  5.sst.tmp écrit + fsync            [contenu durable]
  rename(5.sst.tmp → 5.sst)          [entrée de répertoire : page cache]
  wal.set_len(0) + sync_all          [troncature : journalisée/durable]
→ panne machine avant que le FS n'ait retiré l'entrée de répertoire
→ réouverture : 5.sst absent (ou 5.sst.tmp présent, ignoré), WAL vide
→ les 1000 écritures — toutes confirmées « durables » à l'appelant —
  ont disparu, silencieusement, verify sain
```

Sur ext4 (ordered, journalisé) et NTFS, les métadonnées sont en pratique
retirées dans l'ordre, ce qui explique qu'aucun harnais existant ne l'ait vu
— le kill-loop tue le *processus*, jamais la persistance des métadonnées. Le
risque réel concerne les FS à métadonnées désordonnées, btrfs selon les
modes, les VM/conteneurs avec cache hôte, et tout FS réseau.

#### Impact

Perte silencieuse d'un memtable entier (jusqu'à `memtable_flush_threshold`
écritures confirmées) sur panne machine ; affaiblit aussi `store.meta`,
`generation.meta` (cf. ENG-DUR-004) et le futur manifest.

#### Pourquoi l'architecture actuelle produit ce problème

Décision explicite (« not portable on Windows, the primary dev/CI target ») —
mais l'impossibilité Windows n'implique pas l'inutilité Unix : sur Windows,
`FlushFileBuffers` sur le volume ou l'absence d'API n'empêche pas de faire le
fsync du répertoire sous Unix et un no-op sous Windows.

#### Correction minimale

Helper `sync_dir(path)` : `File::open(dir)?.sync_all()` sous `cfg(unix)`,
no-op sous Windows. Appelé après **chaque** rename de publication (SST,
store.meta, generation.meta, crypto.meta, futur manifest) et avant toute
opération dont la sûreté dépend de la durabilité du rename (troncature WAL,
GC de génération, suppression d'anciennes SST). Coût : un fsync de répertoire
par flush — mesurable, très probablement négligeable devant le fsync SST.

#### Correction cible

Identique (c'est un correctif ponctuel, pas un chantier).

#### Test qui doit échouer avant correction

Non falsifiable in-process. À défaut : test de *présence structurelle* (le
helper est appelé sur chaque site — testable par failpoint sur le helper), et
documentation honnête de la limite dans le doc de module à la place de la
justification actuelle.

#### ADR requis

Aucun (correctif de conformité à ADR-025, pas une nouvelle décision). Une
ligne dans le CHANGELOG et la correction du commentaire suffisent.

#### Dépendances

Aucune. Bloque la validité d'ENG-DUR-001 (le manifest hérite du défaut sinon).

---

### ENG-DUR-004 — Fenêtre de rotation complète : la perte du pointeur de génération détruit la génération courante à l'ouverture suivante

**Sévérité :** P1
**Catégorie :** Durabilité
**Statut :** Risque démontré (même dépendance plateforme qu'ENG-DUR-003, mais conséquence maximale et défense d'ouverture absente)

#### Constat

Séquence de `rotate_full` gen 0 → gen 1 (engine.rs:613-639 + 1071-1101) :
`publish_generation` (rename **non suivi d'un fsync répertoire**) puis
`gc_old_generation` qui supprime *immédiatement* `wal.log`, `crypto.meta` et
toutes les `*.sst` de la racine. À l'ouverture, `resolve_active_generation`
(engine.rs:1140-1161) traite l'absence de `generation.meta` comme « génération
0 » **sans vérifier qu'aucun `gen-N` n'existe**, puis `open_inner` avec une
clé et une racine vide **crée une `crypto.meta` neuve** (engine.rs:334-348) et
`gc_inactive_generations` (engine.rs:1115-1134) **supprime tout `gen-N` ≠
courant** — c'est-à-dire `gen-1` avec l'intégralité des données.

#### Preuve dans le code

- `engine.rs:613` (`publish_generation`) → `engine.rs:1067` (rename sans sync
  dir) → `engine.rs:638` (`gc_old_generation` immédiat)
- `engine.rs:1071-1087` (unlinks best-effort de la racine)
- `engine.rs:1140-1147` (pointeur absent ⇒ racine, gen 0, sans garde-fou)
- `engine.rs:384` (`gc_inactive_generations` appelé à *chaque* open)
- Tests existants (`full_rotation_abort_boundaries…`, failpoints
  `before/after_full_rotation_publish`) : couvrent l'abort **du processus** à
  chaque borne — jamais la non-persistance du rename lui-même.

#### Scénario de panne

```text
rotate_full réussit (vu du processus) :
  gen-1/ complet et durable → rename(generation.meta.tmp → generation.meta)
  [page cache] → GC racine : unlink wal.log, crypto.meta, *.sst
→ panne machine : le FS a persisté les unlinks (ou une partie) mais pas le
  rename du pointeur
→ réouverture avec la nouvelle clé : pas de generation.meta → « gen 0 » ;
  racine sans crypto.meta/wal/sst → CRÉE crypto.meta neuve (store vide) ;
  gc_inactive_generations SUPPRIME gen-1/
→ perte totale et irréversible, sans aucune erreur
```

Même sans réordonnancement : si seuls *certains* unlinks racine ont persisté
et pas le rename, l'ouverture échoue au mieux en `CorruptCryptoMeta`/
`WrongEncryptionKey` (racine incohérente) — mais `gc_inactive_generations` a
potentiellement déjà détruit `gen-1` avant que l'erreur ne soit levée ? Non —
la GC est appelée en fin d'`open_inner` (engine.rs:384), après le chargement
crypto : dans cette variante l'open échoue avant la GC. La variante
catastrophique exige la racine *entièrement* nettoyée + pointeur perdu.

#### Impact

Destruction totale du store. Probabilité faible (fenêtre courte + FS devant
réordonner), conséquence maximale, et la défense coûte trois lignes.

#### Pourquoi l'architecture actuelle produit ce problème

Deux décisions se composent mal : « pointeur absent = génération 0 » (légal
pour les stores legacy) et « GC agressive des générations non courantes à
chaque open » (nettoyage des rotations interrompues). Chacune est raisonnable
seule ; ensemble, elles font de la perte du pointeur un ordre de destruction.

#### Correction minimale

Dans `resolve_active_generation` : pointeur absent **et** au moins un
répertoire `gen-N` présent ⇒ `CorruptGenerationMeta` typée (« pointer missing
but generation directories exist — refusing to treat as generation 0 »).
Jamais de GC dans cet état. C'est un état impossible hors crash — le
signaler est strictement meilleur que le « réparer » en détruisant.

#### Correction cible

Correction minimale + fsync du répertoire racine après le rename du pointeur
et **avant** la GC (ENG-DUR-003).

#### Test qui doit échouer avant correction

Simulation directe : créer un store, `rotate_key_full`, supprimer
`generation.meta` (simule la perte du rename), rouvrir avec la nouvelle clé.
Aujourd'hui : ouverture « réussie » sur un store vide + `gen-1` détruit. Après
correction : erreur typée, `gen-1` intact.

#### ADR requis

Aucun (resserrement d'invariant ADR-042 §3.3, pas une nouvelle décision) ;
à noter dans l'ADR-043 si le manifest arrive dans la même passe.

#### Dépendances

ENG-DUR-003 pour la moitié fsync ; le garde-fou est indépendant.

---

### ENG-COR-001 — Le protocole de publication de compaction du brouillon ADR-043 perd les SST flushées pendant la compaction

**Sévérité :** P1 (bloque N13 en tant que défaut de conception, pas de code)
**Catégorie :** Cohérence
**Statut :** Risque démontré (sur le texte du brouillon non committé, évalué comme hypothèse)

#### Constat

Le brouillon (`docs/adr/ADR-043-…md` §3) décrit : merge depuis un snapshot du
`Version` courant hors verrou, puis « publier `manifest.meta` puis basculer
`self.current` vers le nouveau `Version` ». Pour une SST écrite par un flush
survenu *pendant* la compaction, il affirme : « elle apparaît dans le Version
publié par ce flush… la compaction suivante la ramassera. Aucune donnée n'est
perdue ». C'est faux si la bascule finale de la compaction *remplace*
l'ensemble : le `Version` de la compaction a été construit à partir de
l'ensemble d'entrée (sans la SST du flush) ; le publier comme ensemble complet
**retire la SST du flush de l'ensemble vivant** — et le manifest, désormais
source de vérité, la classera orpheline à la prochaine ouverture (supprimée).

#### Preuve

- Brouillon §2 : `Version { manifest_generation, ssts }` publié par « unique
  remplacement atomique de `self.current` » ; §3 : la bascule finale publie
  le Version issu du merge. Aucune mention d'une fusion avec les Versions
  publiés entre-temps.
- Comparaison : LevelDB/RocksDB publient des `VersionEdit` (deltas : fichiers
  ajoutés/retirés) appliqués sous verrou au Version *courant au moment du
  commit* — jamais un remplacement par un ensemble calculé au début du job.
  C'est exactement la propriété qui manque ici.

#### Scénario

```text
Version V0 = {S1, S2, S3, S4, S5} ; compaction démarre sur snapshot V0
→ pendant le merge : un flush publie V1 = {S1..S5, S6} (manifest gen k+1)
→ compaction termine : sortie S7 = merge(S1..S5) ; publie V2 = {S7}
  (manifest gen k+2) — S6 n'y est pas
→ toutes les écritures du flush S6 disparaissent de l'ensemble vivant ;
  à la réouverture S6 est « orpheline », supprimée
```

#### Impact

Perte de données introduite précisément par le chantier censé fermer la perte
de données. Détectée maintenant, elle coûte un paragraphe ; détectée en
implémentation, elle coûte un format.

#### Correction minimale

Amender le brouillon : la publication de compaction est un **edit** appliqué
sous l'exclusion brève finale au Version courant *à cet instant* :
`V_next = (V_current ∖ inputs) ∪ {output}`, et le manifest écrit l'ensemble
résultant. Les inputs sont identifiés par id — toute SST apparue depuis le
snapshot d'entrée est conservée d'office.

#### Test qui doit échouer avant correction

(Après implémentation N13) : flush concurrent injecté entre le début du merge
et la publication → toutes les clés du flush lisibles après publication et
après réouverture.

#### ADR requis

Amendement d'ADR-043 avant acceptation.

#### Dépendances

ENG-DUR-001 (même chantier).

---

### ENG-CON-001 — Flush et compaction inline sous le verrou d'écriture exclusif : lecteurs et écrivains bloqués O(store), RAM O(store)

**Sévérité :** P1
**Catégorie :** Concurrence / Ressources
**Statut :** Dette structurelle (connue, documentée, mesurée par N6 — c'est l'objet de N13)

#### Constat / Preuve

`put` → `maybe_flush` → `flush` → (seuil 4 dépassé) → `compact()` — tout dans
la même closure `with_inner` sous verrou d'écriture (mod.rs:427-441).
`compact()` matérialise toutes les entrées de toutes les SST dans un
`BTreeMap` (engine.rs:927-933) puis `write_new` matérialise le fichier entier
en RAM (sst_block.rs:384-391) : pic ≈ 2× données vivantes. Idem
`rotate_full` (engine.rs:570-583). Pendant ce temps, tout lecteur
(`recall_*`, `stats`, …) et tout écrivain attendent. La doc du module le
reconnaît en toutes lettres (mod.rs:56-59).

#### Scénario de charge

Store 1M records (~le soak N11.4) : le 1000ᵉ put d'un burst déclenche flush +
full-merge → l'appelant de *ce* put paie la compaction entière ; tous les
recalls concurrents s'empilent sur le RwLock pendant des secondes ; RSS double.

#### Impact

Latence p99 catastrophique et non bornée par la taille de l'opération ;
pic RAM non borné par une option ; aucun backpressure (le blocage *est* le
backpressure).

#### Corrections

Minimale : rien avant N13 (le seuil et le full-merge sont assumés « correct
d'abord », ADR-025). Cible : §2/§3 d'ADR-043 amendé (version set + compaction
hors verrou, exclusion brève à la bascule) ; le streaming du merge
(itérateurs par bloc au lieu de `BTreeMap` + `file_bytes`) est le complément
Ressources — il peut arriver après la concurrence, mais devrait être noté
comme critère de sortie N13 ou suivi explicite.

#### Test qui doit échouer avant correction

Le critère de sortie du plan §10 déjà écrit : lectures concurrentes en
O(lecture normale) pendant une compaction longue.

#### ADR requis / Dépendances

ADR-043 ; dépend d'ENG-DUR-001/ENG-COR-001.

---

### ENG-COR-002 — La vue multi-appels n'est ni cohérente ni spécifiée

**Sévérité :** P2
**Catégorie :** Cohérence
**Statut :** Limitation assumée de fait, jamais documentée comme contrat

#### Constat / Preuve

Un `recall` fusionné = `vector_ranking_ids` (verrou lecture, état A) +
`keyword_ranking_ids` (état B) + `hydrate` (état C) + `touch` (verrou
écriture, état D) — quatre acquisitions (`trait_impl.rs:360-432,434-463`).
Chemins hybrides : deux passes documentées (mod.rs:52-56). Entre deux
acquisitions, un `forget`/`put` concurrent peut faire apparaître un id dans un
ranking et pas dans l'autre, ou le faire disparaître avant `hydrate` (absorbé
en silence — l'id est simplement omis). Aucune anomalie *interne* à un record
(jamais de record déchiré) ; l'anomalie est *inter-records* et *inter-passes*.
`compile_context`/export : chaque appel store est cohérent ; l'export par
agent tient dans une seule closure (porting.rs:64-68) — cohérent par agent.

#### Impact

Résultats de recall légèrement incohérents sous écriture concurrente (fusion
RRF calculée sur deux états). Bénin aujourd'hui (un agent = son propre
écrivain, en pratique) ; devient observable avec les surfaces REST/MCP
multi-clients.

#### Corrections

Minimale : documenter le contrat (chaque appel est atomique ; les
compositions ne le sont pas) dans le doc du trait `MemoryStore`. Cible : si
un besoin produit réel émerge (pas avant), un niveau S2 — voir §6, non
recommandé maintenant.

#### Test / ADR

Pas de test à faire échouer (comportement à documenter, pas à corriger) ;
aucun ADR.

---

### ENG-CON-002 — Empoisonnement permanent du RwLock produit après une panique

**Sévérité :** P2
**Catégorie :** Concurrence
**Statut :** Risque démontré

#### Constat / Preuve

Toute panique dans une closure d'écriture empoisonne le
`std::sync::RwLock<NativeInner>` ; chaque appel suivant retourne « verrou …
empoisonné » (mod.rs:434-437, 454-457) **pour toujours** (process long-vivant
type serveur REST/MCP). Contraste dans le même workspace : le cache vectoriel
récupère explicitement le poison (`lock_cache`,
idx/vector/persistent.rs:177-184) avec une justification écrite. Le moteur
sous le verrou, lui, est conçu pour rester cohérent après interruption
arbitraire (c'est tout l'objet du kill-loop) — l'état RAM des index est mis à
jour après `apply_batch` seulement ; une panique à mi-closure laisse au pire
un état RAM en retard sur le disque, exactement l'état qu'une réouverture
répare.

#### Scénario

Un bug quelconque (ou un OOM d'allocation pendant une compaction inline —
cf. ENG-CON-001) panique dans `with_inner` → le serveur MCP répond « verrou
empoisonné » à chaque requête jusqu'au restart humain.

#### Corrections

Minimale : au choix (décision à acter) — (a) récupérer le poison mais
**réouvrir/re-synchroniser** `NativeInner` depuis le disque (l'état durable
est la source de vérité) plutôt que continuer sur l'état RAM suspect ; (b)
transformer le poison en erreur typée `StorePoisoned` documentée, avec un
conseil opérateur. Continuer aveuglément (comme `lock_cache`) n'est *pas*
correct ici : contrairement au cache, `NativeInner` porte des états dérivés
(allocateur, entry_point) dont la divergence RAM/disque serait dangereuse.

#### Test qui doit échouer

Injecter une panique via failpoint dans une écriture ; vérifier que l'appel
suivant réussit (après re-sync) ou échoue typé — aujourd'hui : erreur de
chaîne générique pour toujours.

#### ADR / Dépendances

Aucun ADR ; indépendant.

---

### ENG-CON-003 — `verify`/`repair` ne prennent pas le verrou writer : audit d'un store actif = résultats faux

**Sévérité :** P2
**Catégorie :** Concurrence
**Statut :** Risque démontré

#### Constat / Preuve

`verify_store` lit les fichiers directement, sans tenter le verrou advisory
(`store/verify.rs` — aucune référence au lock ; voulu pour rester read-only,
`integrity.rs:17-21`). Un `basemyai verify` lancé pendant qu'un serveur
MCP/REST tient le store : une compaction concurrente peut supprimer une SST
entre le listage et sa lecture (erreur io présentée comme anomalie), ou le
WAL peut être tronqué à mi-scan → faux positifs de corruption, ou faux
« healthy » sur une vue mixte.

#### Correction minimale

`try_lock` non bloquant en début de verify : s'il échoue, avertissement
explicite « store en cours d'utilisation — rapport potentiellement faux » (ou
refus, décision produit). Ne verrouille rien de plus.

#### Test

Verify pendant une boucle flush/compact dans un autre thread : aujourd'hui
résultat non déterministe ; après : warning déterministe.

---

### ENG-RES-001 — Bornes absentes sur les tailles unitaires : valeur, clé, octets d'un batch, memtable en octets

**Sévérité :** P2
**Catégorie :** Ressources
**Statut :** Dette structurelle (exploitable par entrée utilisateur via les surfaces)

#### Constat / Preuve

- Aucune borne sur la taille d'une valeur ou d'une clé au niveau `Engine`
  (préfixes u32 → jusqu'à 4 GiB *par valeur* dans un enregistrement WAL,
  `format/wal.rs:87`).
- `apply_batch` borné en *ops* (10 000, `format/wal.rs:162`) mais pas en
  octets — 10 000 × contenu de 1 MiB = un enregistrement WAL de ~10 GiB,
  encodé intégralement en RAM avant écriture (`wal.rs:134-136`). Le produit
  borne `forget_many` en octets (ADR-041 §7.4) mais pas `put_memory_batch`
  (`inner.rs:84-114` — la taille vient du texte utilisateur).
- Seuil de flush en **entrées** (1000), pas en octets (engine.rs:120,143) :
  1000 contenus de 10 MiB = memtable de 10 GiB avant tout flush, copiée
  intégralement en Vec au flush (engine.rs:880), fichier assemblé en RAM
  (sst_block.rs:384).

#### Impact

Un client REST/MCP peut faire croître RSS sans limite avec des `remember`
volumineux légitimes ; pas de refus typé, pas de backpressure — l'échec sera
un OOM.

#### Correction minimale

Bornes typées : `MaxValueBytes`/`MaxBatchBytes` au niveau `Engine` (refus
avant écriture, comme `WalBatchTooLarge`) + seuil de flush additionnel en
octets (`memtable_flush_bytes`). Valeurs à mesurer, pas à deviner.

#### Test

`put` d'une valeur > borne → erreur typée ; batch > borne octets → idem.

---

### ENG-RES-002 — Scans produits non bornés : tout l'agent (ou tout le résultat) matérialisé

**Sévérité :** P2
**Catégorie :** Ressources
**Statut :** Dette structurelle (partiellement résorbée par ADR-041, le reste connu)

#### Constat / Preuve

`list_memories`, `agent_stats`, `recent_episodes`, `exact_fact_exists` font
tous `memory.scan_agent(...)` — l'agent entier en RAM
(trait_impl.rs:81,559,660,682) — alors que les chemins de maintenance ont été
migrés vers `scan_agent_page`/`scan_expiring` (ADR-41 §7.2/7.3). `scan_prefix`
/`scan_range` moteur matérialisent tout le résultat (engine.rs:735-781) — les
consommateurs internes (rebuild vectoriel, verify logique, consolidation
vectorielle `consolidate` engine scan complet) aussi.

#### Impact

RSS proportionnel au plus gros agent sur des appels de *lecture* fréquents
(`agent_stats` est sur le chemin `stats()` produit). Latence O(agent).

#### Correction

Migrer les quatre chemins produits vers `scan_agent_page` (la primitive existe
déjà) ; `exact_fact_exists` peut s'arrêter au premier hit d'une page. Le
streaming moteur (itérateur) reste différé légitimement tant qu'aucun
consommateur ne le requiert — mais `agent_stats` le requiert de fait.

---

### ENG-RES-003 — Métriques manquantes pour décider (fsync, orphelins, générations de publication)

**Sévérité :** P3
**Catégorie :** Ressources / Observabilité
**Statut :** Mesure nécessaire

`EngineStats` (store/stats.rs) couvre bien flush/compaction/cache/octets.
Manquent, chacune liée à une décision précise :

- `fsync_count` — décide si le group commit (§4 du draft) vaut son coût :
  sans compteur, le « gain mesuré » exigé par le plan §10 n'a pas de
  numérateur.
- `orphan_bytes` (tmp + SST hors manifest, post-N13) — décide de
  l'agressivité de la GC d'orphelins.
- `manifest_generation` (post-N13) — corrélation logs/incidents.
- `write_stall_…` : aujourd'hui inexistant car le stall *est* le verrou —
  après N13 (flush arrière-plan), le temps d'attente writer devient la
  métrique de backpressure.
- Snapshots actifs / plus ancien snapshot (post-N13) — décide de borner ou
  non la rétention (le (−) espace-leak listé par le draft est indécidable
  sans ce compteur).

Rien d'autre — pas de métriques décoratives.

---

### ENG-COR-003 — Réallocation possible de `vec_id` après guérison de l'allocateur

**Sévérité :** P3
**Catégorie :** Cohérence
**Statut :** Mesure nécessaire (bénin dans l'état actuel, à re-vérifier à chaque nouvelle dérivée)

`heal_next_vec_id` (idx/memory/persistent.rs:143-155) reconstruit l'allocateur
comme max(nœuds vectoriels ∪ vecmap)+1 : si le META est corrompu **et** que
les plus hauts ids ont été purgés physiquement (consolidation vectorielle),
des ids sont réalloués. Aujourd'hui sûr : toutes les structures qui
référencent un `vec_id` (record, vecmap, FTS postings/docterms, nœud) vivent
et meurent dans le *même* batch atomique — un id purgé n'est référencé nulle
part. L'invariant implicite « aucune référence à un vec_id hors du batch qui
le crée/supprime » n'est écrit nulle part ; une future dérivée écrite hors
batch le casserait silencieusement. À documenter comme invariant dans le doc
de module de `idx/memory`.

---

### ENG-CON-004 — Annulation de future : l'écriture continue et commit malgré l'annulation perçue par l'appelant

**Sévérité :** P3
**Catégorie :** Concurrence
**Statut :** Limitation assumée à documenter

`with_inner` fait `spawn_blocking(...).await` (mod.rs:433-440) : si le future
appelant est annulé (timeout REST, client MCP parti), la tâche bloquante
**continue et commit**. L'appelant croit l'opération échouée ; elle est
durable. Inverse aussi : `JoinError` mappé en erreur de stockage alors que
l'écriture a pu réussir (« erreur après commit » classique). Aucun bug de
cohérence interne — ambiguïté de surface uniquement. À documenter dans les
surfaces (REST : une 5xx/timeout ne signifie pas non-appliqué).

---

### ENG-CRY-001 — GC best-effort de l'ancienne génération : matériel ancien-DEK résiduel

**Sévérité :** P3
**Catégorie :** Crypto (intégration, pas de refaire ADR-042)
**Statut :** Limitation assumée, posture à documenter

`gc_old_generation`/`gc_inactive_generations` ignorent toute erreur
(engine.rs:1071-1134). Un unlink refusé laisse d'anciennes SST/crypto.meta
lisibles avec l'ancienne clé — précisément la fenêtre qu'ADR-042 ferme
« logiquement ». Retry à chaque open (bien), mais aucun signal si le résidu
persiste (lié à `orphan_bytes`, ENG-RES-003). Le modèle de menace d'ADR-030 §4
documente le cas *pré-rotation-complète* ; le cas « GC échoue en boucle »
mérite une ligne dans `docs/security/encryption-model.md`. Par ailleurs un
snapshot/worker futur (N13) doit retenir **des objets déjà ouverts**
(`BlockSstFile` + son `CryptoContext` cloné) — jamais un chemin + une clé à
re-dériver : la rotation concurrente changerait la résolution. Le design
`Arc<Version>` du draft a naturellement cette propriété (les `BlockSstFile`
portent leur `crypto: Option<CryptoContext>`, sst_block.rs:426).

---

### ENG-TST-001 — Gaps de couverture : les scénarios qui manquent sont exactement ceux des findings P0/P1

**Sévérité :** P2
**Catégorie :** Tests
**Statut :** Dette structurelle

Matrice réelle vérifiée (fichiers listés un par un dans les workflows — pas de
découverte automatique, un nouveau fichier de test **doit** être câblé à la
main, déjà mordu une fois et corrigé en §8.3) :

| Cible | Exécutée par | OS | Gate |
|---|---|---|---|
| lib + basic/vector_*/graph_parity/malformed_open/engine_stats/failpoints/corruption_smoke/model_based/io_faults/format_lock (+ adr042_contract, ajout non committé dans ci.yml) | `ci.yml` job `native-engine` (liste explicite `--test` un par un, ci.yml:102) | ubuntu + windows | PR |
| crash_consistency (kill-loop, 7 modes ×20 cycles) | job dédié `crash-consistency` | ubuntu + windows | PR |
| crash_consistency ×200 + bench archivé | `nightly.yml` | ubuntu | nightly |
| 24 cibles fuzz (liste = `fuzz/Cargo.toml`, vérifié complète) | `fuzz.yml` | ubuntu | nightly |
| soak (`engine-soak`) | `soak-campaign.yml` | ubuntu + windows + **macOS** | hebdo |

Manquent, corrélés aux findings :

- Réordonnancement de métadonnées FS (rename vs truncate/unlink) — non
  testable in-process ; à défaut, tests de présence du `sync_dir` par
  failpoint (ENG-DUR-003/004).
- Échec de `remove_file` pendant le nettoyage de compaction → résurrection
  (ENG-DUR-002) — testable dès aujourd'hui par failpoint.
- Perte du pointeur de génération après rotation (ENG-DUR-004) — testable
  dès aujourd'hui par suppression directe du fichier.
- verify sous concurrence (ENG-CON-003).
- Panique dans une closure d'écriture → état du store ensuite (ENG-CON-002).
- ENOSPC réel (les failpoints injectent des erreurs *aux frontières
  choisies* ; un vrai disque plein frappe au milieu d'un `write_all` — quota
  FS ou tmpfs plein, faisable sous Linux CI).
- macOS absent du gate PR et du nightly (coût documenté, choix assumé — mais
  les sémantiques fsync/rename APFS sont précisément *différentes* ;
  l'hebdomadaire soak ne rejoue pas le kill-loop sur macOS).

Points forts à reconnaître : le kill-loop est un **vrai** kill de processus
externe (pas une simulation), le model-based test vérifie le contrat (modèle
BTreeMap indépendant), les tests de troncature « à chaque coupe » sont
systématiques, et deux tests documentent honnêtement un gap connu au lieu de
le cacher — pratique rare et précieuse.

---

### ENG-DOC-001 — Contrats mensongers ou surpromesses documentaires

**Sévérité :** P2
**Catégorie :** Documentation
**Statut :** Bug confirmé (contradictions vérifiées)

Par risque induit :

- **Peut induire un faux sentiment de garantie utilisateur** :
  - `README.md:168` : « crash-consistent LSM » sans qualificatif — vrai pour
    crash de processus, non établi pour panne machine (ENG-DUR-003) et faux
    pour suppression externe de SST (ENG-DUR-001, épinglé par test). Une
    ligne « limites connues » suffirait.
  - Doc de `flush` / commentaire sst_block.rs:408-414 : la justification de
    l'absence de fsync répertoire est **techniquement fausse** (voir
    ENG-DUR-003) — c'est le cas le plus dangereux : un mainteneur futur lira
    « not a correctness gap » et construira dessus.
- **Peut faire diverger un agent de code** :
  - `CLAUDE.md` (workspace) : « chantier actif : N12 … non committé » — or
    N12 est clos et committé (status.md:270, commits `fccdca2`, `d6f1f4e`).
    Un agent pourrait « terminer » un chantier déjà clos. À rafraîchir.
  - `context/` marqué « ⚠ non committé » dans CLAUDE.md — les commits R1.x
    (`b4d1fea`…`0e742f8`) sont dans l'historique. Idem.
- **Peut faire lancer les mauvais gates** : ci.yml modifié non committé
  (ajout `--test adr042_contract`) — l'écart working-tree/HEAD sur le gate
  lui-même ; à committer avec le reste.
- Cohérences vérifiées sans contradiction : format.lock ↔ codecs (test
  `format_lock` au gate), licence BUSL-1.1 dans les en-têtes moteur ↔
  Cargo.toml, statut expérimental du format documenté.

---

## 5. Table de priorisation

| ID | Sévérité | Probabilité | Impact | Effort | Bloque N13 | Bloque 0.2.0 | Action |
|---|---:|---:|---:|---:|---|---|---|
| ENG-DUR-001 | P0 | Moyenne (intervention externe, backup, crash) | Perte/régression silencieuse | M (c'est N13 §1) | **Est** N13 | Oui (au moins documenté) | Manifest d'abord, seul, livrable |
| ENG-DUR-002 | P0 | Faible-moyenne (Windows sharing violations réalistes) | Résurrection de données supprimées | S (correction minimale) | Non (corr. min. indépendante) | Oui | Ne plus ignorer `remove_file` + test failpoint |
| ENG-DUR-003 | P1 | Faible (FS-dépendant) | Perte d'un memtable confirmé | S | Oui (le manifest en dépend) | Oui | Helper `sync_dir`, tous les sites de rename |
| ENG-DUR-004 | P1 | Très faible, conséquence maximale | Destruction totale du store | S (garde-fou 3 lignes) | Non | Oui | Pointeur absent + gen-N présent = erreur |
| ENG-COR-001 | P1 | Certaine si implémenté tel quel | Perte de SST flushées | S (amender le texte) | **Oui** | Non | Version *edit*, pas remplacement |
| ENG-CON-001 | P1 | Certaine à l'échelle | p99 + RSS non bornés | L (c'est N13 §2-3) | Est N13 | Non | Après manifest |
| ENG-CON-002 | P2 | Faible | DoS jusqu'au restart | S-M | Non | Souhaitable | Politique de poison à acter |
| ENG-CON-003 | P2 | Moyenne (opérateur) | Faux rapports verify | S | Non | Souhaitable | try_lock + warning |
| ENG-RES-001 | P2 | Moyenne (entrée utilisateur) | OOM sans refus typé | M | Non | Souhaitable | Bornes typées + seuil octets |
| ENG-RES-002 | P2 | Moyenne | RSS/latence O(agent) | M | Non | Non | Migrer 4 chemins vers scan paginé |
| ENG-COR-002 | P2 | Faible | Recall légèrement incohérent | S (doc) | Non | Non | Documenter le contrat |
| ENG-TST-001 | P2 | — | Confiance mal calibrée | M | Partiel (tests préalables N13) | Non | Tests failpoint DUR-002/004 d'abord |
| ENG-DOC-001 | P2 | — | Mainteneur/agent induit en erreur | S | Non | Oui (README) | Corriger commentaire + README + CLAUDE.md |
| ENG-RES-003 | P3 | — | Décisions N13 non mesurables | S | Partiel (fsync_count avant group commit) | Non | 3 compteurs |
| ENG-COR-003 | P3 | Très faible | Réf. croisée future cassée | S (doc) | Non | Non | Écrire l'invariant |
| ENG-CON-004 | P3 | Moyenne | Ambiguïté surface | S (doc) | Non | Non | Documenter |
| ENG-CRY-001 | P3 | Faible | Résidu ancien-DEK | S (doc) | Non | Non | Ligne dans encryption-model.md |

2 × P0, 4 × P1, 7 × P2, 4 × P3 — 17 findings.

---

## 6. Snapshot decision analysis

### Ce que le code exige aujourd'hui, niveau par niveau

**S0 — aucune API de snapshot (état actuel).** Chaque appel produit est
cohérent (une closure = une acquisition). Les compositions (recall fusionné,
hybrides deux-passes) traversent des états. Coût : zéro. Dette : ENG-COR-002
(documentaire), et surtout **l'impossibilité structurelle de sortir la
compaction du verrou** — dès que `compact()` relâche le verrou pendant son
merge, `self.ssts` peut changer sous lui et les fichiers qu'il lit peuvent
être supprimés. S0 est incompatible avec le critère de sortie N13.

**S1 — snapshot structurel des fichiers (`Arc<Version>` sur l'ensemble de
SST).** Les SST sont immuables par construction (jamais modifiées après
rename) — les figer ne coûte qu'un compteur de références. Ce qu'il donne :
compaction hors verrou (merge sur un Version figé), lecteurs jamais bloqués
par la bascule (O(1)), suppression différée sûre (Drop du dernier Arc),
`verify` capable d'auditer un ensemble stable. Ce qu'il ne donne **pas** :
une vue point-in-time — la memtable reste vivante ; un `get` via snapshot
composé « memtable live + SSTs figées » peut voir une écriture postérieure au
snapshot. Coût : remplacer `ssts: Vec<BlockSstFile>` par `Arc<Version>`,
aucune modification de format de données (le manifest est requis par ailleurs).
Complexité introduite : durée de vie des SST liée aux snapshots (l'espace-leak
listé par le draft — borné par une métrique + éventuellement une durée max).

**S2 — snapshot point-in-time du moteur (memtable figée + séquences).**
Exigerait : soit memtable immuable clonée au snapshot (coût mémoire), soit un
numéro de séquence par op + versions multiples par clé dans la memtable, et
des tombstones versionnés dès que la compaction peut courir sous un snapshot.
Cas d'usage réels dans le code actuel : **aucun**. Les trois candidats
examinés :
- `recall_vector → hydrate → touch` : hydratation *déjà* dans la même closure
  que la recherche (inner.rs:36-63) ; `touch` est un write-behind volontaire
  qui n'a pas besoin de la vue de la recherche.
- Export : une closure par agent, déjà cohérent au grain promis.
- `verify --logical` : opère sur les fichiers, pas sur l'API — S1 + manifest
  lui suffisent.
Rejeté maintenant : coût réel (versionnement des clés en memtable, compaction
consciente des séquences) sans consommateur.

**S3 — transactions de lecture multi-opérations.** Nécessiterait S2 + une API
produit (`Memory::snapshot()`) propagée à travers `MemoryStore`. Aucun besoin
exprimé (le Context Engine compose des appels et tolère la dérive). Rejeté.

### Les trois scénarios exigés

1. **Ce qui casse sans séquence** : rien aujourd'hui — le verrou global tient
   lieu d'ordre total. Ce qui casserait *avec compaction concurrente sans S1* :
   `scan_prefix` itérant `self.ssts` pendant que `compact` fait son
   `mem::replace` + `remove_file` → lecture d'un fichier supprimé (erreur io)
   ou d'un état mixte. **S1 suffit** à le fermer, sans aucune séquence.
2. **Ce qui se résout sans MVCC** : la compaction concurrente entière —
   inputs figés par Arc, sortie publiée par edit sous exclusion brève,
   suppression au Drop. Le flush arrière-plan aussi (memtable → immutable
   memtable → SST) *sans* versions par clé, tant qu'un seul writer existe :
   l'immutable memtable est une couche de lecture supplémentaire, pas une
   version.
3. **Ce qui exigerait réellement des versions multiples** : un lecteur qui
   doit voir l'état d'avant une écriture *déjà appliquée à la memtable* —
   c'est-à-dire S2/S3. Aucun chemin produit ne le demande. Le futur
   changefeed (N15) demandera une **séquence durable par batch** — c'est un
   numéro d'ordre d'événements, pas du MVCC : il n'exige ni versions par clé
   ni compaction consciente des snapshots, et peut s'ajouter au manifest plus
   tard sans le refondre (champ additif).

### Recommandation

**S1, pas plus.** La forme du draft (`Version { manifest_generation, ssts:
Arc<[BlockSstFile]> }`) est la bonne, avec trois précisions à graver dans
l'ADR : (a) un `Snapshot` fige **les fichiers, pas la vue** — le nom de l'API
doit le dire (`sst_snapshot()` ou doc explicite), pour ne pas vendre S2 sous
un nom S1 ; (b) publication de compaction = **edit** (ENG-COR-001) ; (c)
métrique « snapshots actifs / plus ancien » dès la première implémentation
(ENG-RES-003). Ce qui reste hors périmètre : séquences par opération,
versions par clé, tombstones versionnés, compaction consciente des snapshots
— aucun n'est nécessaire pour S1, tous le deviendraient pour S2 (à ne
construire que sur besoin produit démontré).

---

## 7. Durable catalog decision analysis

| Option | Ce qu'elle ferme | Ce qu'elle coûte | Verdict |
|---|---|---|---|
| Listage du répertoire (actuel) | Rien | — | 🔴 ENG-DUR-001/002 |
| Manifest snapshot des SST (liste complète réécrite à chaque publication) | DUR-001, DUR-002 (orphelins ignorés), publication logique, base de S1 | 1 tmp+fsync+rename+**sync_dir** par flush/compaction | ✅ **Recommandé** |
| Manifest `VersionEdit` (journal de deltas, style LevelDB) | Idem + historique | Replay + compaction du journal + fichier CURRENT — machinerie entière de LevelDB | 🔴 Prématuré : le nombre de SST est petit (full-merge au-delà de 4-5) ; réécrire une liste de N entiers est trivialement moins cher que la machinerie de replay. À reconsidérer si une stratégie tiered/leveled multiplie les fichiers |
| Manifest SST + WAL | Détection d'un `wal.log` supprimé | Peu | 🟡 Inutile aujourd'hui : un WAL au nom fixe, unique par génération, absent = indistinguable d'un WAL vide… **sauf** si le manifest note un booléen `wal_expected_nonempty` — complexité/valeur défavorable ; un WAL supprimé ne perd que la queue non flushée, et sa détection fiable exigerait de journaliser chaque append. Rejeté tant que le WAL n'est pas segmenté |
| + séquences | Changefeed N15 | Champ additif | Plus tard (N15), le format doit juste rester **extensible** (version de manifest) |
| + prochaine allocation (`next_sst_id`) | Réutilisation d'id après suppression d'orphelins | Un u64 | 🟡 Optionnel : dérivable (`max(live)+1`) *à condition* que l'open supprime les orphelins **avant** de calculer — sinon un orphelin d'id supérieur ré-émergerait. Le stocker explicitement est plus simple à prouver ; décision d'implémentation, pas d'architecture |

**L'invariant, formulé avant le format** (comme exigé) :

> À tout instant après un crash, il existe exactement un manifest lisible par
> génération active, et l'ensemble des SST qu'il liste est intégralement
> présent et lisible sur disque. Toute SST présente hors de cette liste est un
> orphelin librement supprimable. Le WAL de la génération contient exactement
> les écritures confirmées postérieures à la dernière publication du manifest.

Ordre de publication qui le maintient : SST (fsync + rename + **sync_dir**) →
manifest (idem) → troncature WAL. Crash entre 1 et 2 : orphelin + WAL complet
(replay). Entre 2 et 3 : manifest + WAL redondant (replay idempotent — même
propriété que l'actuel `before_wal_truncate` testé). Le contenu logique :
`manifest_generation: u64` + `live_sst_ids: Vec<u64>` + CRC — le brouillon
ADR-043 §1 est correct sur ce point ; n'y ajouter que (a) le `sync_dir`, (b)
la règle « orphelins supprimés avant le calcul de `next_sst_id` » (ou le champ
explicite), (c) une version de format pour l'extensibilité N15. Ne pas figer
le codec binaire ici.

---

## 8. Target state machines

Notation : `[D]` = point de durabilité, `[V]` = point de visibilité,
`↺` = récupération après crash à cet état.

**Write / Batch** (inchangé, S1 n'y touche pas)
```text
encode → seal? → write_all(wal) → sync_all [D] → memtable [V] → Ok
↺ toute coupe avant [D] : record absent ou queue déchirée → tronquée, op absente (jamais partielle)
↺ entre [D] et [V] : impossible à observer (même thread) ; kill → replay au prochain open
```

**Memtable seal (cible N13, phase background-flush)**
```text
active → (seuil octets/entrées) → scellée immuable [V: lecture via active'+scellée+SSTs]
       → nouvelle active vide ; le writer N'ATTEND PAS l'écriture SST
↺ crash : la scellée n'existe que en RAM ; le WAL couvre active+scellée → replay intégral
Invariant : la troncature WAL n'est légale qu'après que TOUTES les memtables
couvertes par ce WAL sont durables en SST — avec une seule scellée à la fois,
identique à aujourd'hui ; avec plusieurs, le WAL doit être segmenté (hors
périmètre tant qu'une seule scellée suffit — à écrire dans l'ADR).
```

**Flush (cible)**
```text
scellée → write_new(tmp) → sync_all [D: contenu] → rename → sync_dir [D: existence]
→ manifest' = manifest ∪ {id} → publish (tmp/fsync/rename/sync_dir) [D+V logique]
→ Version' publié (Arc swap, exclusion brève) [V lecteurs]
→ wal reset (ou segment retiré) → scellée libérée
↺ avant publish manifest : SST orpheline (supprimée à l'open), WAL rejoue
↺ après publish, avant reset : replay idempotent par-dessus la SST
```

**Compaction (cible)**
```text
inputs = Version_snapshot.ssts (Arc, hors verrou) → merge streamé → SST out
[D contenu+existence comme flush]
→ SOUS EXCLUSION BRÈVE : manifest' = (manifest_courant ∖ inputs) ∪ {out}   ← EDIT
  → publish [D+V logique] → Version' = (courant ∖ inputs) ∪ {out} [V]
→ inputs : remove différé au Drop du dernier Arc<Version> les référençant ;
  échec de remove = orphelin (métrique orphan_bytes), ré-essayé à l'open —
  JAMAIS une résurrection : l'open ne lit que le manifest
↺ crash avant publish : out orpheline, anciens ensembles intacts
↺ crash après publish avant remove : inputs = orphelins, supprimés à l'open
```

**Snapshot (S1)**
```text
snapshot() = Arc::clone(current) [V figée : fichiers seulement]
drop(dernier Arc) → SSTs exclusives au Version → remove
Aucun état disque propre au snapshot ; rien à récupérer après crash.
```

**Rotation complète (amendée)**
```text
build gen-N+1 complet (comme aujourd'hui, y c. manifest de gen-N+1)
→ publish pointeur (tmp/fsync/rename/sync_dir racine) [D+V]
→ bascule RAM → GC ancien répertoire (best-effort, ré-essayée à l'open)
↺ pointeur absent + gen-N présent = ERREUR TYPÉE (garde-fou ENG-DUR-004),
  jamais « gen 0 + GC »
```

**Recovery (open, cible)**
```text
lock → store.meta → résoudre génération (avec garde-fou) → crypto
→ lire manifest → vérifier manifest ⊆ disque (manquant = MissingLiveSst typée)
→ supprimer disque ∖ manifest (orphelins) → next_sst_id = max(live)+1 (ou champ)
→ replay WAL → gc générations inactives (inchangé, après garde-fou)
```

**File retirement** : un fichier n'est jamais supprimé que s'il est (a) hors
manifest à l'open, ou (b) retiré du manifest par un edit publié **et** plus
référencé par aucun Version en RAM. La suppression physique n'est jamais un
acte de publication.

---

## 9. ADR map

Le découpage 043/044/045/046 proposé dans la consigne est **partiellement
retenu, resserré** :

**ADR-043 — Catalogue durable + version set + snapshots S1 + compaction
concurrente** (le brouillon actuel, amendé). Garder manifest + Version +
compaction ensemble est justifié : le manifest seul a une valeur livrable
(fermer P0) et le brouillon le dit déjà (« §1 avant §2/§3, valeur livrable
même si la compaction concurrente est repoussée ») — c'est un *phasage de
PR*, pas deux ADR. Amendements requis avant acceptation :
1. Publication de compaction = version **edit** (ENG-COR-001).
2. `sync_dir` après chaque rename de publication, y compris le manifest
   (ENG-DUR-003) — sinon l'ADR bâtit sur le défaut qu'il corrige.
3. Garde-fou générations (ENG-DUR-004) noté comme prérequis.
4. Snapshot explicitement étiqueté S1 (fichiers, pas vue) ; S2 explicitement
   hors périmètre avec le critère de réouverture (« besoin produit d'une vue
   point-in-time démontré »).
5. Orphelins supprimés avant calcul de `next_sst_id` (ou champ explicite).
6. Métriques : snapshots actifs, plus ancien snapshot, orphan_bytes,
   manifest_generation.
Problème fermé : ENG-DUR-001/002, ENG-CON-001 (partie lecture), ENG-COR-001.
Invariant : celui du §7. Hors périmètre : multi-writer, S2, séquences.

**ADR-045 — Memtable scellée + flush/compaction en arrière-plan.** Séparé et
*postérieur* à 043 : il change qui exécute le travail (workers) là où 043
change ce qui est publié. Dépend de 043 (le worker publie des edits). Contenu :
seuil en octets (ENG-RES-001 partiel), une seule memtable scellée à la fois
(pas de WAL segmenté), backpressure explicite (si la scellée n'est pas encore
flushée quand l'active atteint son seuil → le writer attend, métrique
write_stall), politique de panique des workers (lié ENG-CON-002). Hors
périmètre : plusieurs scellées, WAL segmenté.

**ADR-046 — Group commit.** Séparé et conditionnel : ne s'écrit qu'après
`fsync_count` mesuré sous charge réelle (ENG-RES-003) — le plan §10 exige déjà
« gain mesuré ». Sous l'architecture actuelle (un writer logique sous verrou
produit), le group commit exige une file d'attente d'écrivains *devant* le
verrou — c'est une restructuration de `NativeMemoryStore`, pas du moteur seul ;
l'ADR doit le dire. Ne pas l'écrire avant.

**ADR-044 (« catalogue SST+WAL ») : rejeté** comme ADR séparé — le catalogue
est dans 043 ; le suivi du WAL n'y entre pas (§7). Le numéro reste libre pour
le changefeed N15 (qui introduira la séquence durable — et c'est déjà le
numéro que le plan §12 lui réserve).

**Corrections sans ADR** (conformité, pas décision) : sync_dir (DUR-003),
garde-fou générations (DUR-004), remove_file non ignoré (DUR-002 minimal),
politique de poison (CON-002), try_lock verify (CON-003), bornes de taille
(RES-001 — sauf si les valeurs choisies méritent une trace, alors une section
dans 045), docs (DOC-001).

---

## 10. Implementation roadmap

Chaque jalon laisse le moteur ouvrable, sans transition dual-format, avec
rollback = revert du jalon (aucun jalon ne réécrit de données existantes ;
le seul ajout on-disk est un fichier nouveau, ignoré par les builds
antérieurs… qui refuseront de toute façon par `STORE_FORMAT_VERSION` si on la
bump — décision à prendre au jalon 2 : bump = les vieux builds refusent
proprement ; pas de bump = un vieux build ignorerait le manifest et
recréerait le monde du listage — **bump recommandé**, le format est déclaré
expérimental).

```text
J0 — Corrections préalables (indépendantes, 1 PR courte)
  sync_dir sur tous les renames ; garde-fou générations ; remove_file de
  compaction non ignoré (retry + tombstones conservés en dégradé) ;
  correction du commentaire sst_block.rs:408 + README.
  Tests préalables : failpoint remove_file → résurrection (échoue avant),
  suppression generation.meta → destruction (échoue avant).
  Sortie : les deux tests passent ; kill-loop et gate verts.
  Benchmarks : engine_bench avant/après (coût sync_dir sur Linux).

J1 — Tests + instrumentation N13 (PR 2 du protocole §14 du plan)
  fsync_count, orphan_bytes ; tests qui échouent : détection SST manquante
  (retourner les 2 tests existants), flush-pendant-compaction (préparé,
  ignoré tant que non implémenté).

J2 — Manifest (format + publication + open + verify) — ferme les P0
  Manifest:1 dans format.lock ; bump STORE_FORMAT_VERSION ; stamp additif à
  la première ouverture en écriture d'un store pré-N13 (politique
  StoreMeta:1→2 existante) ; ordre SST→manifest→WAL ; open par manifest ;
  MissingLiveSst typée ; verify confronte.
  Sortie : les 2 tests retournés passent ; crash-consistency étendu au
  failpoint before_sst_manifest_publish ; rollback : revert (stores stampés
  restent lisibles par le build du jalon, pas par les antérieurs — assumé).

J3 — Version set immuable (Arc<Version>, suppression différée, snapshot S1)
  Aucun changement de format. Sortie : test « snapshot retient la SST » du
  draft ; métrique snapshots actifs.

J4 — Compaction hors verrou (publication par edit, exclusion brève)
  Sortie : critère « aucun lecteur bloqué » ; test flush-pendant-compaction
  de J1 activé et vert ; p99 documentées vs baseline N7.

J5 — (ADR-045) Memtable scellée + flush worker + backpressure + seuil octets
  Sortie : write_stall instrumenté ; kill-loop inchangé vert (le WAL couvre
  active+scellée).

J6 — (ADR-046, conditionnel) Group commit — seulement si fsync_count sous
  charge réelle le justifie. Sinon : ne pas construire.

Stratégie de compaction (tiered/leveled) : AUCUN changement avant mesures
post-J4 — le full-merge sorti du verrou peut suffire longtemps aux workloads
BaseMyAI (stores de mémoire d'agent : volumes modestes, churn tombstone via
forget/GC — le full-merge les purge intégralement, ce qu'un leveled ne fait
qu'au dernier niveau). Décider sur données : write/space amp du soak après J4.
```

---

## 11. What not to build

- **MVCC complet / clés internes versionnées / tombstones versionnés** —
  aucun consommateur S2/S3 (§6) ; coût de compaction consciente des snapshots
  sans bénéfice démontré.
- **Multi-writer** — le plan le dit déjà ; le verrou produit sérialise, et
  aucune mesure ne montre que la sérialisation des *écritures* (vs le blocage
  par compaction, qui lui est réel) est un goulot.
- **Manifest en journal de VersionEdit + CURRENT (machinerie LevelDB)** —
  liste snapshot suffisante à ≤ ~10 SSTs (§7).
- **WAL segmenté** — nécessaire seulement avec plusieurs memtables scellées ;
  une seule suffit (ADR-045).
- **Leveled/tiered compaction** — pas avant les mesures post-J4 ; les
  workloads mémoire-d'agent favorisent le full-merge (purge totale des
  tombstones = vraie suppression, argument privacy).
- **Lock-free / sharding du RwLock** — la contention mesurée (N5.5 : ~3×
  gain déjà obtenu par le RwLock) ne justifie rien de plus ; le vrai blocage
  est la compaction inline, traité par J4.
- **Borne de durée de vie des snapshots** — instrumenter d'abord (métrique),
  borner seulement si un leak réel apparaît.
- **SQL maison, P2P, sync** — déjà proscrits par le plan §16, confirmé.
- **Cache de blocs sophistiqué (CLOCK/SLRU)** — LRU actuel non mesuré comme
  insuffisant.

---

## 12. Commands and evidence

Exécutées (2026-07-19, machine d'audit Windows 11, rustc 1.95.0) :

```text
git status --short / branch / rev-parse / log -10     → §0
cargo clippy -p basemyai-engine --all-targets --features test-util -- -D warnings
                                                       → OK (exit 0)
cargo test -p basemyai-engine --features test-util --lib --bins
  --test basic --test vector_recall --test vector_persistence
  --test vector_churn --test graph_parity --test malformed_open
  --test engine_stats --test failpoints --test corruption_smoke
  --test model_based --test io_faults --test format_lock
  --test adr042_contract                               → OK (exit 0 ;
  390 tests lib + suites d'intégration ; la ligne « FAILED » visible dans la
  sortie de failpoints est le processus ENFANT du test
  env_configuration_arms_failpoints_in_a_fresh_process, volontairement lancé
  avec une valeur invalide — le test parent qui l'orchestre passe)
```

Non exécutées, et pourquoi :

- `cargo xtask ci` complet — le working tree porte des modifications
  CLI/eval/xtask non committées hors périmètre moteur ; un gate complet
  mesurerait cet état mixte, pas HEAD. Les deux composantes moteur du gate
  (clippy + suite de tests, mêmes flags que ci.yml:77,102) ont été rejouées
  directement, vertes.
- `cargo xtask test-crash-consistency` — kill-loop lent (~20 cycles × 7
  modes), exécuté par un job CI dédié sur les deux OS à chaque PR ; aucune
  raison de croire l'état local différent (sources moteur = HEAD).
- Campagne fuzz / soak 1M — explicitement exclues par la consigne ; nightly/
  hebdo CI les couvrent (matrices vérifiées complètes, §4 ENG-TST-001).
- Reproduction réelle des scénarios ENG-DUR-003/004 (réordonnancement de
  métadonnées) — exige dm-log-writes/CrashMonkey sous Linux ; hors de portée
  de l'environnement d'audit. Statut « risque démontré » (par analyse +
  littérature), pas « bug reproduit ».

---

## 13. Final recommendation

**B — N13 doit être redécoupé mais peut commencer.**

Pas **A** : lancer N13 tel que le brouillon l'écrit implémenterait un
protocole de publication de compaction qui perd des données (ENG-COR-001) sur
une fondation de publication qui n'est pas durable au niveau répertoire
(ENG-DUR-003) — les deux se corrigent en heures, mais *avant*.

Pas **C** : geler N13 en attendant la fermeture des P0 serait circulaire —
le manifest de N13 **est** la fermeture des P0. Les seuls préalables réels
(J0 : sync_dir, garde-fou générations, remove_file) sont trop petits pour
justifier un gel.

Pas **D** : le moteur n'a pas besoin d'une refonte. Le diagnostic « le projet
se tire des balles dans le pied avec des protections autour d'un modèle
incomplet » est **partiellement** confirmé — mais dans un sens précis et
réparable : les protections unitaires (tmp/fsync/rename par fichier, batchs
atomiques, wire-distrust, kill-loop) sont réelles et de qualité ; ce qui
manque est *une seule* pièce systémique — la publication durable de l'état
logique (quel ensemble de fichiers est vivant) — dont l'absence transforme
plusieurs protections locales en garanties accidentelles tenues par le verrou
global et par des hypothèses d'ordre FS non garanties. Le sextuple « pas
d'état logique publié + WAL unique + memtable unique + SST découvertes
physiquement + compaction inline + verrou global » est cohérent en
mono-thread-sous-verrou ; il devient une dette systémique *uniquement* si la
concurrence arrive avant le catalogue. L'ordre J0 → manifest → version set →
concurrence la résorbe incrémentalement, sans réécriture.

---

## Annexe — Forces relevées (à ne pas perdre en corrigeant)

- Discipline « erreur avant écriture » systématique (duplicates, bornes de
  batch, dimension vectorielle) ; « RAM après disque » systématique dans les
  index ; retry propre après échec de flush (id non incrémenté, memtable
  conservée) — vérifié io_faults.
- Deux tests qui *documentent* des trous connus au lieu de les masquer —
  cette honnêteté est l'outil qui a permis à cet audit d'aller vite ; la
  conserver comme pratique.
- Kill-loop avec vrai kill externe + model-based avec modèle indépendant +
  fuzz une-cible-par-décodeur : une base de confiance rare pour un moteur de
  cet âge. Les findings de ce rapport sont presque tous *hors* du domaine que
  ces harnais peuvent voir (métadonnées FS, chemins d'erreur ignorés,
  concurrence future) — c'est la prochaine frontière de test, pas un défaut
  des harnais existants.
