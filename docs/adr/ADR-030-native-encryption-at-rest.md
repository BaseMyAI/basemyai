# ADR-030 — Chiffrement au repos du moteur natif : AEAD + enveloppe DEK/KEK

**Statut** : ✅ Accepted
**Date** : 2026-07-06
**Relation aux ADR existants** : implémente sur le moteur natif (ADR-024/025)
l'équivalent d'ADR-007 (chiffrement au repos, obligatoire côté `basemyai`) et
la parité fonctionnelle de la rotation de clé M6 (`Store::rotate_key`,
`PRAGMA rekey`). C'est le sous-jalon **N5.4** du découpage acté par ADR-027.
N'amende rien.

## Contexte

Les données du moteur natif vivent dans deux artefacts fichiers : `wal.log`
(enregistrements WAL, y compris les batchs atomiques) et `*.sst` (tables
triées). **Tous** les index logiques (vecteur ADR-026, graphe N4, mémoire
ADR-027, FTS ADR-028) sont des enregistrements KV qui transitent par ces deux
fichiers — chiffrer WAL + SST couvre donc mécaniquement « les blocs d'index ».

Côté libSQL, ADR-007 délègue tout à SQLCipher/SQLite3MultipleCiphers (feature
`crypto`, exige CMake) et la rotation M6 est un `PRAGMA rekey` qui ré-écrit
toutes les pages — avec les acrobaties documentées dans
`Store::rotate_key` (bascule WAL→DELETE→rekey→WAL, instance caduque après
l'appel, réouverture obligatoire).

Le moteur natif possède l'intégralité de ses fichiers et de ses chemins
d'I/O : on peut faire mieux que reproduire ces contraintes.

## Décision

### 1. Primitives : AEAD XChaCha20-Poly1305 + dérivation SHA-256, pur Rust

- **AEAD** : XChaCha20-Poly1305 (crate `chacha20poly1305`, RustCrypto, pur
  Rust, auditée). Nonce de 24 octets **généré aléatoirement** par scellé
  (l'espace de nonce étendu de XChaCha20 rend le tirage aléatoire sûr, sans
  compteur d'état à persister/crash-réconcilier). Le tag Poly1305 authentifie
  chaque artefact : toute altération est une erreur franche, jamais une
  lecture silencieusement fausse.
- **Dérivation de la KEK** (clé d'enveloppe) : `SHA-256(domaine || salt(16)
  || clé_utilisateur)`, salt aléatoire par store, domaine
  `basemyai-engine/kek/v1`. **Pas d'étirement de clé** (Argon2/PBKDF2) : la
  clé fournie est supposée à haute entropie, même posture qu'ADR-007 où la
  clé passe telle quelle à SQLCipher. Si un jour une passphrase humaine
  devient l'entrée nominale, l'étirement sera un ADR de suivi (le `salt` et
  le domaine versionné dans `crypto.meta` laissent la place).
- **Aucune feature Cargo** : contrairement au `crypto` libSQL (gaté parce que
  CMake est un coût d'installation), les deps sont pures Rust et légères —
  le chiffrement natif compile inconditionnellement. `sha2` est déjà dans le
  workspace (vérification SHA-256 du provisioning) ; seule `chacha20poly1305`
  s'ajoute.

### 2. Enveloppe DEK/KEK : la clé utilisateur n'encrypte jamais la donnée

Modèle standard (LUKS, chiffrement de systèmes de fichiers, KMS cloud) :

- À la création du store chiffré, une **DEK** (data encryption key, 32
  octets) est tirée aléatoirement. C'est **elle** qui chiffre WAL et SST.
- La clé utilisateur ne sert qu'à dériver la **KEK**, qui scelle la DEK dans
  un petit fichier `crypto.meta` (`CryptoMeta:1` dans `format.lock` : magic,
  version, salt, nonce, DEK scellée, crc32 ; AAD = magic‖version‖salt pour
  lier l'en-tête au scellé).
- À l'ouverture : `crypto.meta` présent ⇒ store chiffré — clé obligatoire
  (erreur franche sinon), descellement de la DEK = **vérification de clé**
  (échec AEAD ⇒ `WrongEncryptionKey`, jamais des lectures qui échouent plus
  loin de façon inexplicable). `crypto.meta` absent + fichiers existants +
  clé fournie ⇒ erreur franche : on ne chiffre pas a posteriori un store en
  clair (même posture que `rotate_key` ADR-007).

### 3. Granularité de chiffrement

- **WAL** : chaque enregistrement (y compris un batch entier — déjà un seul
  enregistrement externe, ADR-025) est scellé individuellement dans une
  enveloppe `WalEnvelope:1` (magic, version, nonce, ct_len, ciphertext).
  La tolérance au *torn tail* est préservée à l'identique : enveloppe
  incomplète ⇒ arrêt silencieux du replay (crash mid-append attendu) ;
  enveloppe complète dont l'AEAD échoue ⇒ `CorruptWal` (la clé a déjà été
  vérifiée via `crypto.meta`, ce ne peut être qu'une corruption). L'atomicité
  des batchs tombe du même cadrage qu'en clair : un batch = une enveloppe.
- **SST** : le fichier entier est scellé en une enveloppe `SstEnvelope:1`
  (magic, version, nonce, ciphertext jusqu'à EOF). Adapté au design actuel
  (ADR-025 : le SST est lu intégralement en mémoire, pas de lecture par bloc)
  — si un futur ADR introduit un index de blocs, le chiffrement par bloc sera
  un nouveau format versionné.
- Le pipeline crash-safe (écrire tmp, fsync, rename, **puis** tronquer le
  WAL) est inchangé : le chiffrement enveloppe les octets, il ne touche pas
  à l'ordre des opérations durable.

### 4. Rotation de clé : re-scellement O(1), commit atomique un-fichier

`Engine::rotate_key(new_key)` : nouveau salt, nouvelle KEK dérivée, la **même
DEK** re-scellée, `crypto.meta` remplacé par écriture tmp + fsync + rename.
Conséquences, comparées au `PRAGMA rekey` libSQL :

- **O(1)** au lieu d'une ré-écriture de toutes les pages.
- **Crash-safe par construction** : le commit est le rename atomique d'un
  seul petit fichier — après un crash, `crypto.meta` est soit l'ancien
  (l'ancienne clé ouvre), soit le nouveau (la nouvelle clé ouvre), jamais un
  état mixte « la moitié des SST sur l'ancienne clé ». C'est précisément le
  danger qu'une ré-encryption in-place des fichiers de données aurait créé,
  et la raison de choisir l'enveloppe.
- **L'instance reste valide après rotation** (la DEK ne change pas) — pas de
  « Store caduc, rouvrez tout » comme libSQL.

**Écart assumé, documenté honnêtement** : la DEK ne change pas. Un attaquant
qui possède l'ancienne clé **et** une copie de l'ancien `crypto.meta` peut
desceller la DEK et lire les fichiers de données *actuels* — là où le rekey
SQLCipher rend le fichier courant illisible même avec ancienne clé + ancienne
copie. Le modèle de menace d'ADR-007 (fichier au repos lu par quiconque
accède au disque, ancienne clé compromise *sans* copie antérieure du store)
est intégralement couvert : l'ancienne clé n'ouvre plus le store. La
ré-encryption complète des données (nouvelle DEK + ré-écriture WAL/SST sous
un journal de rotation crash-safe) est un chantier de suivi explicite si ce
modèle de menace renforcé devient exigé — jamais une promesse implicite de
cette décision.

### 5. Surface consommateur

- `Engine::open_encrypted(path, key)` (+ variante `_with_options`),
  `Engine::rotate_key`, `Engine::is_encrypted`. `Engine::open` inchangé
  (clair).
- `EngineCapabilities::native(encrypted: bool)` — même signature que
  `libsql(encrypted)` : le flag reflète l'instance ouverte, plus un mensonge
  structurel `false`.
- `basemyai::storage::NativeMemoryStore::open_encrypted(path, key)` +
  `rotate_key` async (parité de surface avec `Memory::rotate_key`). La
  politique « chiffrement obligatoire sur fichier » d'ADR-007 s'appliquera au
  moment où `Memory` saura s'assembler sur le backend natif (N5.6) — pas
  ici : `NativeMemoryStore` est une brique gatée `engine-native`, jamais
  défaut.

### 6. Ce qui est vérifié

- Codecs : roundtrips, torn-tail, rejets structurels (mêmes disciplines de
  défiance du wire que N2/N3 : longueurs bornées avant toute allocation).
- Moteur : roundtrip chiffré put/get/flush/compaction/reopen ; mauvaise
  clé / clé absente / clé sur store en clair ⇒ erreurs franches typées ;
  altération d'un SST ou d'un enregistrement WAL ⇒ erreur AEAD franche ;
  rotation ⇒ ancienne clé rejetée, nouvelle clé ouvre, données intactes,
  instance utilisable sans réouverture.
- **Crash harness** : le kill-loop réel (mode `batch` — le plus riche :
  enveloppes WAL de batchs, flushs SST, compaction, `crypto.meta` relu à
  chaque cycle) tourne aussi en variante chiffrée.
- `format.lock` : trois nouveaux formats (`CryptoMeta:1`, `WalEnvelope:1`,
  `SstEnvelope:1`), tout drift casse la CI.

## Alternatives rejetées

- **Chiffrer directement avec la clé utilisateur (pas de DEK)** : force une
  rotation par ré-écriture de tous les fichiers, dont la crash-safety exige
  un journal de rotation multi-fichiers — exactement la complexité que
  l'enveloppe fait disparaître ; et chaque rotation coûterait O(données).
- **AES-256-GCM** : équivalent en sécurité, mais nonce de 12 octets — le
  tirage aléatoire y est plus contraint (bornes de collision), et sans
  accélération matérielle garantie sur toutes les cibles, le logiciel pur
  ChaCha20 est plus uniforme. XChaCha20 supprime la question.
- **Étirement de clé (Argon2id)** : hors modèle d'entrée actuel (clé haute
  entropie fournie par la couche application, posture ADR-007) ; ajouterait
  une dep lourde et des paramètres de coût à gérer. ADR de suivi si besoin.
- **Chiffrement par champ/valeur KV** : casserait la recherche vectorielle
  côté moteur et laisserait les clés (donc `agent_id`, termes FTS, ids) en
  clair sur le disque — inacceptable, c'est la métadonnée la plus sensible.
- **Zeroize des clés en mémoire** : hors périmètre, comme côté core
  (`EncryptionKey` est une `String` non zeroizée) — la menace ADR-007 est le
  disque au repos, pas la RAM du process. Item de suivi commun si le modèle
  s'étend.

## Conséquences

- `EngineCapabilities::native(encrypted)` peut enfin rapporter `encrypted:
  true` honnêtement — dernière capacité `false` du backend natif levée.
- Trois formats de plus sous le régime `format.lock`.
- La rotation native est *meilleure* que la parité (O(1), crash-safe, pas de
  réouverture) au prix de l'écart DEK documenté en §4 — un consommateur qui
  exige la sémantique « ré-encryption totale » ne doit pas la supposer.
- N5.5 (barre M6) doit étendre le harnais crash mode `memory` au store
  chiffré et mesurer le surcoût AEAD dans le bench KNN chemin `MemoryStore`.
