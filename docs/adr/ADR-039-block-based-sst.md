# ADR-039 — Format SST par blocs : AEAD par bloc, index, bloom filters, block cache

**Statut** : ✅ Accepted
**Date** : 2026-07-10
**Relation aux ADR existants** : fait évoluer le format SST posé par ADR-025
(« Phase A : lecture intégrale en mémoire, pas d'index de blocs ni de bloom
filter » — simplification assumée là-bas, levée ici) et la granularité de
chiffrement SST d'ADR-030 §3 (enveloppe fichier entier, dont le texte
prévoyait déjà : « si un futur ADR introduit un index de blocs, le
chiffrement par bloc sera un nouveau format versionné » — c'est cet ADR).
C'est le jalon **N8** du programme production-hardening
(`docs/PLAN-NATIVE-ENGINE.md` §5). N'amende ni ADR-025 ni ADR-030 sur le
reste (WAL, enveloppe DEK/KEK, pipeline tmp/fsync/rename inchangés).

## Contexte

Le format SST actuel (`SstFile:1`) est un unique segment : à l'ouverture, le
fichier **entier** est lu, déchiffré (une seule `SstEnvelope`) et décodé en
`Vec<(Key, Option<Value>)>` résident en mémoire. C'était le bon choix de
Phase A (correctness d'abord) ; la baseline N7.5
(`docs/benchmarks/n7-engine-baseline-2026-07-10.md`) chiffre maintenant ce
que ce modèle coûte :

- **Ouverture = chargement intégral** : `open_bytes_read == sst_bytes`
  (vérifié par compteur), 35,5 ms pour 12,7 Mo à 100k — projection ~3 s et
  ~1 Gio de RSS pour un store de 1 Gio. Le RSS du process croît linéairement
  avec la taille du store, indéfiniment.
- **Chiffré : +24 % à l'ouverture** (unseal du fichier entier avant le
  moindre octet utile).
- Le point lookup est rapide (0,9 µs) *uniquement parce que* tout est déjà
  en RAM — conséquence du problème, pas une qualité du format.

Un moteur de mémoire d'agent local doit ouvrir vite et tenir un RSS borné
sur des stores qui grossissent pendant des années. Le format doit changer
**avant** que des stores utilisateurs réels existent — la politique
« format expérimental » (`docs/format/bmai-v1.md` §Format stability) rend la
coupure encore gratuite.

## Décision

### 1. Nouveau layout SST

```text
SstHeader        magic, version, sst_id, block_size, entry_count, dim(réservé)
Data block 0..N  entrées triées, bornées par block_size (cible, pas exact)
Block index      par bloc : first_key, last_key, offset, len, entry_count
Bloom filter     sur toutes les clés du fichier
SstFooter        offsets/longueurs index+bloom, compteurs, crc32, magic final
```

- Chaque **data block** porte : nombre d'entrées, entrées `(key_len, key,
  val_len|tombstone, val)`, crc32. Toutes les longueurs sont bornées contre
  la taille réelle du buffer **avant** toute allocation (discipline N2/N3,
  leçon fuzzing).
- Le **footer** est à taille fixe en fin de fichier : l'ouverture lit
  footer → index → bloom, soit O(métadonnées), jamais O(données).
- Pas de compression de préfixes ni de restart points en v1 du format bloc :
  d'abord la structure, la compression sera mesurée ensuite si l'ampli
  disque le justifie (le format la permet sans re-design : c'est interne au
  bloc, versionné par `SstDataBlock`).
- **`first_key`/`last_key` vivent dans l'index, pas dupliqués dans le bloc**
  — le bloc reste autonome pour le rebuild (ses entrées portent leurs clés),
  l'index reste suffisant pour router sans lire les blocs.

### 2. Taille de bloc : fixée par le spike N8.1, pas par intuition

Le paramètre `block_size` est un champ du header (pas une constante de
compilation). Le spike N8.1 compare **16, 32 et 64 KiB** sur les workloads
canoniques (`engine_bench`, mêmes seeds que la baseline N7.5) et mesure :
point lookup froid, scan séquentiel, coût d'index, amplification disque,
coût du chiffrement, I/O par lecture, RSS. La valeur gagnante devient le
défaut d'`EngineOptions` ; les deux autres restent lisibles (le reader lit
`block_size` du header — un seul code path).

### 3. Chiffrement par bloc (mode chiffré)

Chaque section (data block, index, bloom, header étendu) est scellée
individuellement :

```text
EncryptedSstBlock  nonce XChaCha20 (24 o, aléatoire), ct_len, ciphertext+tag
```

- **AAD par section** : `domaine_versionné ‖ sst_id ‖ section_type ‖
  section_no`. Un bloc déplacé **entre deux SST** (sst_id différent dans
  l'AAD) ou **au sein du même SST** (section_no différent) échoue son tag
  Poly1305 même si le bloc est individuellement intact — l'exigence
  anti-permutation du plan §5.4 tombe de l'AAD, sans structure
  supplémentaire.
- L'index et le bloom sont authentifiés comme les data blocks (mêmes
  enveloppes, section_type distinct). Le footer, lu en premier, porte son
  propre scellé ; en clair il garde un crc32.
- La DEK, `crypto.meta`, la vérification de clé à l'ouverture et l'enveloppe
  WAL sont **inchangés** (ADR-030 §2/§3-WAL/§4).

### 4. Chemin de lecture

Point lookup (`EngineStats` instrumenté à chaque étape) :

1. bloom filter — absent ⇒ terminé (zéro I/O) ;
2. index de blocs — recherche binaire sur `last_key` ;
3. block cache — hit ⇒ pas d'I/O (`block_cache_hits`) ;
4. miss ⇒ lecture du **seul** bloc visé (pread offset/len), déchiffrement/
   vérification, insertion au cache (`block_cache_misses`) ;
5. recherche binaire dans le bloc.

**Invariant instrumenté** : `point_lookup_full_sst_read == 0` — un compteur
dédié incrémenté si un lookup déclenche la lecture de plus d'un data block ;
un test l'épingle à zéro sur les workloads canoniques.

Le scan préfixé lit la suite contiguë de blocs couvrant l'intervalle (via
l'index), en streaming — la compaction utilise le même chemin : elle cesse
d'exiger tout le store en RAM. **La stratégie de compaction (full-merge
naïve, amplification ×80 mesurée) ne change pas ici** — c'est une décision
séparée (plan §16 : ne pas mélanger format et stratégie), instruite après N8
avec les chiffres du nouveau format.

### 5. Block cache

- Clé `(sst_id, block_no)` → bloc **décodé** (déchiffré en mode chiffré).
- Capacité **en octets**, configurable (`EngineOptions`), défaut fixé par le
  spike (ordre de grandeur : 32 Mio).
- Politique **LRU simple** en v1 (mesurée au spike ; CLOCK/SLRU seulement si
  LRU montre un défaut mesuré — pas de sophistication spéculative).
- Aucun verrou tenu pendant une I/O : lookup cache sous verrou court, I/O
  hors verrou, insertion sous verrou court (le double-fetch concurrent d'un
  même bloc est bénin et rare).
- Invalidation par `sst_id` à la suppression d'une SST (compaction).
- **Modèle de menace documenté** : le cache contient du clair en RAM. Même
  posture qu'ADR-030 (« la menace est le disque au repos, pas la RAM du
  process ») — consigné dans `SECURITY.md` avec cette décision.

### 6. Bloom filters

- **Un filtre par SST** en v1 (par bloc seulement si les mesures du spike le
  justifient — plan §5.7).
- Double hashing `h1 + i·h2` sur deux graines d'une fonction versionnée
  (implémentation maison zéro-dep, figée par `SstBloomFilter:1`) ;
  bits-par-clé configurable, défaut 10 (~1 % de faux positifs).
- Reconstruit depuis les blocs, jamais source de vérité, incapable de faux
  négatif (propriété testée : chaque clé insérée répond présente).

### 7. Rejet des anciens stores : marqueur de version + coupure nette

- Nouveau fichier **`store.meta`** (`StoreMeta:1`) : magic, version de
  format du store (**2**), crc32. Écrit à la création ; les stores existants
  n'en ont pas.
- À l'ouverture : `store.meta` absent **avec** des artefacts présents, ou
  version ≠ attendue ⇒ erreur typée `UnsupportedStoreFormat { expected,
  found, path }` dont le message suit le gabarit du plan §5.3 (« recreate
  the store with the current version »). Store vierge ⇒ création normale.
- Politique de remplacement (plan §5.3, politique format expérimental) : le
  writer ne produit **que** le nouveau format, le reader ne lit **que** lui,
  les codecs `SstFile:1`/`SstEnvelope:1` sont **supprimés** (pas de lecture
  duale, pas de migration), les stores de dev sont recréés. `format.lock` :
  retrait des deux specs, ajout de `SstHeader:2`, `SstDataBlock:1`,
  `SstBlockIndex:1`, `SstBloomFilter:1`, `SstFooter:1`,
  `EncryptedSstBlock:1`, `StoreMeta:1`.
- Le failpoint `before_manifest_publish` (réservé depuis N7.4) prend son
  site sur la publication de `store.meta`. Le **manifest des SST vivantes**
  (le gap « SST supprimée = perte silencieuse » épinglé par
  `tests/corruption_smoke.rs`) reste le périmètre d'ADR-040/N9 — `store.meta`
  n'identifie que le format, il ne liste pas les fichiers.

### 8. Critères de sortie (mesurés contre la baseline N7.5)

1. Ouverture d'un store de **1 Gio** : RSS additionnel ≤ métadonnées
   (index+bloom+footer) + capacité du cache — jamais proportionnel aux data
   blocks ; `open_bytes_read` ≤ 5 % de `sst_bytes`.
2. `point_lookup_full_sst_read == 0` sur tous les workloads canoniques.
3. Corruption d'un bloc (bit-flip, troncature, permutation intra- et
   inter-SST) ⇒ erreur typée — `corruption_smoke` étendu au niveau bloc.
4. Ancien code SST supprimé du crate ; ancien store ⇒
   `UnsupportedStoreFormat` (testé).
5. Régressions vs baseline 100k bornées : `kv-fill` mean ≤ +10 % ;
   `kv-point-read` **chaud** (cache) mean ≤ 10 µs ; froid p95 ≤ 500 µs ;
   `memory-recall` mean ≤ +15 % (les lectures ANN passent par le cache).
6. `cargo xtask engine-crash` vert en clair et chiffré (le harnais couvre le
   nouveau chemin flush/compaction), failpoints SST re-vérifiés.
7. Fuzz targets des nouveaux codecs (`sst_header_decode`,
   `sst_block_decode`, `sst_footer_decode`, bloom, `store_meta_decode`)
   posées ; exécutées sous WSL (contrainte libFuzzer Windows documentée).
8. Baseline N8 archivée (`engine-bench` 10k/100k/1M, clair+chiffré) à côté
   de N7.5, avec le tableau avant/après.

## Alternatives rejetées

- **Statu quo (fichier entier)** : tué par les chiffres de la baseline —
  RSS et temps d'ouverture linéaires en taille de store, sans plafond.
- **mmap + déchiffrement à la volée** : sous Windows (cible primaire
  dev/CI), la combinaison mmap + fichiers chiffrés par enveloppe n'apporte
  rien (il faut de toute façon déchiffrer vers un buffer privé) et le
  comportement mmap sous kill/rename est la classe de bugs la plus opaque
  des moteurs qui l'ont choisi. Lectures pread explicites, testables par
  failpoints.
- **Adopter un format existant (RocksDB BlockBasedTable, parquet, fjall)** :
  re-délèguerait la propriété du layout disque que ADR-024/025 ont
  précisément décidé de posséder ; et aucun ne porte l'AEAD par bloc lié par
  AAD au `sst_id` qu'exige le modèle ADR-030.
- **Garder l'enveloppe fichier-entier avec un index en clair à côté** :
  l'index en clair fuit les clés (donc `agent_id`, ids, bornes de
  l'espace) — inacceptable, ADR-030 a déjà rejeté le KV-par-champ pour la
  même raison ; et le fichier resterait indéchiffrable partiellement.
- **Lecture duale ancien/nouveau format pendant une transition** : interdit
  par la politique format-expérimental (aucune donnée utilisateur publique
  n'existe) — la dette de compatibilité commencerait avant le premier
  utilisateur.
- **Compaction leveled/tiered dans le même chantier** : mélangerait format
  et stratégie (plan §16) ; l'amplification ×80 a sa propre décision à
  instruire sur les chiffres post-N8.

## Conséquences

- Le RSS du moteur devient borné (métadonnées + cache) au lieu de linéaire
  en taille de store — la condition d'existence des stores multi-Gio.
- L'ouverture devient O(métadonnées) ; le surcoût crypto d'ouverture (+24 %
  mesuré) disparaît dans le même mouvement (déchiffrement lazy par bloc).
- Le point lookup froid coûte désormais une I/O + un déchiffrement de bloc —
  le bloom et le cache existent précisément pour que le cas commun n'en paie
  aucun. Les seuils du §8.5 en font un engagement mesurable, pas un espoir.
- `EngineStats` gagne ses compteurs réels `block_cache_hits/misses` (champs
  déjà publiés à zéro depuis N7.1 — schéma JSON stable).
- Sept specs de format changent dans `format.lock` en un seul bump cohérent ;
  tout store de dev antérieur doit être recréé (politique assumée).
- Le découpage d'implémentation suit le plan §17 (N8.1 → N8.10) : spike,
  codecs, writer, reader, suppression de l'ancien format, bloom, cache,
  AEAD par bloc, rejet des anciens stores, crash+fuzz+bench.
