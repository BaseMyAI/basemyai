# ADR-021 — Pool de connexions lecteur libSQL + writer unique sérialisé, sous WAL

**Status**: Accepted
**Date**: 2026-06-21
**Context**: suivi d'ADR-011 (backend libSQL V1) ; n'amende ni ne supersède
ADR-011 ni ADR-019 (`.bmai` libSQL-compatible). Cible le hardening M6 (« bench
KNN, stress test, pool, key rotation ») listé ouvert dans `docs/status.md`.

## Contexte

`basemyai_core::Store` (`crates/basemyai-core/src/storage/store.rs`) garde
**une seule** `Connection` libSQL clonée (`conn: Connection`, le clone partage
le même handle natif `sqlite3*`). Toutes les méthodes — lecture (`vector_knn`,
`vector_knn_reranked`) comme écriture (`migrate`, `vector_upsert`,
`begin_write`) — passent par `self.connect()`, qui rend ce clone.

En mode SQLite SERIALIZED (celui que libSQL configure,
`SQLITE_CONFIG_SERIALIZED`), les opérations concurrentes sur **une même
connexion** contendent sur le mutex interne de cette connexion : leur exécution
est sérialisée, même sur un runtime tokio multi-thread. C'est le **vrai**
goulot d'étranglement du sidecar (MCP/REST) en concurrence — une rafale de KNN
parallèles tombe en file derrière ce mutex — et **non** le choix du moteur de
stockage. Le chemin chaud à débloquer est la lecture (KNN).

## Décision

1. **Garder la `Database` vivante dans `Store`** (en plus du handle actuel) et
   ouvrir un **pool de N connexions lecteur indépendantes** pour les requêtes
   de lecture. Chaque `db.connect()` produit un **handle natif neuf** (pas un
   clone), donc avec son propre mutex de connexion : N lectures progressent
   réellement en parallèle.
2. **Un writer unique sérialisé** est conservé : SQLite n'autorise qu'**un seul
   écrivain** à la fois. Le `write_lock: Mutex<()>` existant reste en place ;
   toutes les écritures (DDL, upsert, transactions `begin_write`) passent par
   cette connexion writer unique sous verrou.
3. **Sélection des lecteurs en round-robin lock-free** (`AtomicUsize`,
   `fetch_add` modulo N). Pas de checkout/checkin : les connexions SERIALIZED
   sont sûres à partager entre futures, donc inutile de les sortir/rendre du
   pool — on en pointe une et on l'utilise.

## WAL est un prérequis, pas un détail

Le modèle « N lecteurs + 1 writer **concurremment** » n'existe dans
SQLite/libSQL **que** sous `PRAGMA journal_mode=WAL`. Dans le journal rollback
par défaut, un writer prend un **verrou exclusif** qui bloque **tous** les
lecteurs pour la durée de l'écriture : un pool de lecteurs n'apporterait alors
rien (ils attendraient le writer comme avant). WAL est donc activé à
l'ouverture, avec `busy_timeout` (pour absorber les contentions transitoires
writer/checkpoint) et `synchronous=NORMAL` (le couple recommandé avec WAL :
durabilité au checkpoint, pas à chaque commit). **WAL ne s'applique qu'aux
stores sur fichier.**

## `:memory:` dégénère en pool de taille 1

Sur une base libSQL **en mémoire**, chaque `db.connect()` rend une base
**séparée et vide** : il n'y a pas de fichier partagé derrière. C'est
précisément pourquoi le code actuel partage un handle cloné pour rester
cohérent en `:memory:`. Conséquence : **le pool lecteur ne s'applique qu'aux
stores sur fichier**. Les stores en mémoire gardent la connexion partagée
unique (pool dégénéré de taille 1, sans WAL). **Non négociable** — sinon les
tests en mémoire (et toute la suite d'intégration) cassent.

## Routage lecture/écriture explicite

Le foot-gun précédent — `connect()` servait indifféremment aux lectures **et**
à des écritures ad hoc — est remplacé par un routage explicite :

- **Écritures** (DDL, `vector_upsert`, transactions) → la connexion **writer**
  unique, sous `write_lock`. Inchangé sémantiquement, toujours sérialisé.
- **Lectures** → `reader()` (pooled, round-robin).

`connect()` est **conservé comme alias de compatibilité vers le writer** : les
appelants existants qui s'en servent pour écrire restent corrects et
sérialisés. On ne route vers le pool que les chemins de lecture explicitement
migrés.

## Race d'ouverture native sur Windows

Ouvrir N connexions multiplie les appels `sqlite3_open_v2` — l'opération
signalée racy sur Windows (`STATUS_ACCESS_VIOLATION`, voir
`native_open_lock()` dans `store.rs` et `RUST_TEST_THREADS=1` dans
`.cargo/config.toml`). Le **warm-up du pool ouvre les N handles lecteur
séquentiellement, sous `native_open_lock()`** — jamais en parallèle. Les
ouvertures restent rares (au démarrage du store), donc le coût séquentiel est
négligeable face au risque de la race.

## `spawn_blocking` délibérément reporté (hors de cette ADR)

Le pool de lecteurs indépendants **délivre déjà le parallélisme** recherché :
chaque future est *pollée* sur un worker tokio distinct, et l'appel FFI libSQL
s'exécute inline en parallèle sur ces workers. Un offload `spawn_blocking`
ajouterait de l'**isolation de latence** (empêcher une rafale de KNN d'affamer
les workers async qui servent le réacteur HTTP), mais l'API locale de libSQL
est **async** : l'envelopper dans `spawn_blocking` forcerait un
`Handle::current().block_on()` à l'intérieur — un anti-pattern peu lisible, non
justifié pour des requêtes point/KNN sub-millisecondes.

**Décision : livrer le pool, *benchmarker*, et n'ajouter l'offload que si le
profiling montre une famine des workers async.** Référence : SurrealDB sépare
un handle RocksDB **unique** partagé (concurrent en interne via MVCC) d'un pool
bloquant `affinitypool` ; notre libSQL n'a **pas** cette concurrence interne,
d'où le besoin du pool de lecteurs — pas (encore) du pool bloquant.

## Conséquences

Positives :

- Le sidecar (MCP/REST) sert N lectures KNN réellement en parallèle ; le mutex
  de connexion unique cesse d'être le goulot sous concurrence.
- L'écriture reste correcte et atomique : un seul writer, `begin_write`
  inchangé, sérialisation garantie par `write_lock`.
- Le routage lecture/écriture explicite supprime le foot-gun `connect()`.

Compromis :

- `Store` gagne en complexité (pool, round-robin, WAL, deux chemins selon
  fichier vs `:memory:`). Le pragma WAL et la divergence `:memory:` sont des
  invariants à maintenir et à tester.
- L'isolation de latence (`spawn_blocking`) n'est **pas** fournie : si un futur
  profiling révèle une famine des workers async sous charge HTTP, un ADR de
  suivi l'introduira.

## Alternatives rejetées

**Changer de moteur de stockage (ex. RocksDB, concurrent en interne via
MVCC).** Rejeté : on perdrait le vecteur in-DB de libSQL
(`vector_top_k`/`F32_BLOB`, sans extension ni DB externe) et le chiffrement au
repos intégré (feature `crypto`), et cela **contredit ADR-011** (décision
libSQL) ainsi que le chemin futur Turso DB (pur Rust). Le goulot constaté n'est
pas le moteur mais le **handle de connexion unique** ; on corrige la cause
réelle.

**Garder un handle unique et offloader chaque requête en `spawn_blocking`.**
Rejeté ici : sans pool, toutes les requêtes resteraient sérialisées sur le
mutex de la connexion unique — `spawn_blocking` déplacerait l'attente sur un
autre thread sans supprimer la sérialisation. Le pool s'attaque à la cause ;
l'offload (latence) est un sujet orthogonal, reporté.

**Activer WAL aussi en `:memory:`.** Rejeté : sans fichier partagé, chaque
`db.connect()` en mémoire ouvre une base distincte ; WAL n'y a pas de sens et
le pool y dégénère de toute façon à taille 1.

## Suivi possible

- Bench KNN concurrent (M6) pour dimensionner N et valider le gain mesuré.
- Si le profiling montre une famine des workers async sous charge HTTP réelle :
  ADR de suivi pour l'offload `spawn_blocking` (isolation de latence).
- `busy_timeout` / taille de pool exposés en configuration si la charge le
  justifie.
