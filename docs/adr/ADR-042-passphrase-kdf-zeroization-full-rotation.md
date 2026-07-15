# ADR-042 — Passphrase KDF, zeroization des secrets et rotation complète de la DEK

**Statut** : 🟡 Proposed (décision soumise à revue — aucune implémentation ne
doit atterrir avant acceptation ; PR1 du découpage §14 du plan)
**Date** : 2026-07-15
**Amendement 2026-07-15 (même jour)** : §1-§5 amendés après trois passes
dédiées — (a) recherche de prior art (SQLCipher/SEE `rekey`, CockroachDB/TiKV
sur RocksDB, MariaDB re-encryption online, LUKS2 `reencrypt` + CVE-2021-4122,
ZFS `change-key`, Android Verified Boot pour le rollback), (b) analyse de
faisabilité sur le code réel (file:line), (c) revue sécurité adversariale du
présent document. Les deux questions laissées ouvertes par la première
version (compaction avant `--full` ; prérequis N13) sont **tranchées**
ci-dessous, et les correctifs sécurité obligatoires intégrés.
**Relation aux ADR existants** : prolonge ADR-030 (enveloppe DEK/KEK,
`crypto.meta` v1, `rotate_key` re-scellement O(1)) — **n'amende pas** ADR-030,
qui reste exact pour tout ce qu'il couvre (mode clé brute, rotation
re-scellement). ADR-042 **ajoute** un second mode d'entrée (passphrase) et une
**seconde** opération de rotation (`--full`), distincte de celle d'ADR-030 qui
reste disponible telle quelle. Complète ADR-034 (résolution centralisée de la
passphrase — le §6 d'ADR-034 renvoie explicitement le keyring OS à une
« section V2 », c'est cette section). Ne touche ni ADR-025 (fondation LSM), ni
ADR-039 (SST par blocs) au-delà de la génération de répertoire décrite ici.
C'est le jalon **N12** du programme production-hardening
(`docs/PLAN-NATIVE-ENGINE.md` §9).

## Contexte

ADR-030 a posé l'enveloppe DEK/KEK et documenté deux écarts assumés comme
« item de suivi » plutôt que comme lacune silencieuse :

- §1 : *« Pas d'étirement de clé (Argon2/PBKDF2) : la clé fournie est
  supposée à haute entropie… Si un jour une passphrase humaine devient
  l'entrée nominale, l'étirement sera un ADR de suivi. »*
- §4 : *« La ré-encryption complète des données (nouvelle DEK…) est un
  chantier de suivi explicite si ce modèle de menace renforcé devient
  exigé. »*
- Alternatives rejetées : *« Zeroize des clés en mémoire : hors périmètre…
  Item de suivi commun si le modèle s'étend. »*

ADR-034 a normalisé la résolution de la passphrase à travers les surfaces
(CLI/bindings/REST/MCP) mais a explicitement mis le keyring OS hors scope V1
(§6), le renvoyant à une future « section V2 ». N11 (hardening) vient de se
clore (`docs/status.md`, 2026-07-15) : les trois items de suivi ci-dessus
deviennent le contenu de N12. Cet ADR est la **décision**, pas encore le code
— PR1 du découpage §14 du plan (ADR, critères de sortie, formats envisagés,
alternatives rejetées, politique de remplacement du format ; PR2 posera
les tests/instrumentation qui échouent avant l'implémentation ; PR3+
implémentera).

Aujourd'hui, concrètement (vérifié dans le code, pas supposé) :

- `crates/basemyai-engine/src/crypto.rs::derive_kek` : `SHA-256(KEK_DOMAIN ||
  salt || user_key)`, où `user_key: &[u8]` est un emprunt — zéro étirement.
- `CryptoMeta` (`format/crypto.rs`, `CryptoMeta:1` verrouillé dans
  `format.lock`) ne porte aucun champ de mode : un seul mode existe.
- `basemyai_core::EncryptionKey` (`crates/basemyai-core/src/storage/key.rs`)
  enveloppe une `String` brute, `#[derive(Clone)]`, sans `Zeroize`. Elle
  traverse CLI → résolution ADR-034 → `NativeMemoryStore::rotate_key`/`open` →
  `crypto.rs::create_meta`/`write_meta`/`load_meta` (`&[u8]` partout) sans
  qu'aucun maillon ne zeroize à la destruction.
- `crypto.rs::derive_kek` retourne déjà `Zeroizing<[u8; 32]>` — confirmé.
- `crypto/material.rs::Dek` est déjà `#[derive(Clone, Zeroize, ZeroizeOnDrop)]`
  — confirmé.
- `Engine::rotate_key`/`crypto.rs::write_meta` **re-scelle la même DEK** sous
  une nouvelle KEK — O(1), jamais de ré-écriture WAL/SST (ADR-030 §4, `git
  grep rotate_key` dans `crates/basemyai/src` confirme une seule opération de
  ce type exposée : `Memory::rotate_key` → `NativeMemoryStore::rotate_key` →
  `Engine::rotate_key`).
- `docs/security/encryption-model.md` n'existe pas dans le dépôt à ce jour ;
  le cadrage de référence est ADR-007 (menace : disque au repos) + ADR-030 +
  ADR-034 (`docs/security/key-resolution.md`, qui existe et documente déjà le
  renvoi keyring OS → V2, cohérent avec le point 4 ci-dessous).

## Décision

### 1. Deux modes d'entrée de clé

`CryptoMeta` gagne un discriminant de mode. **Le mode clé brute reste
strictement inchangé** (compat totale des stores existants) ; le mode
passphrase est **additif**.

- **`KdfMode::RawKey` (= 0)** : chemin actuel, bit à bit identique —
  `derive_kek` inchangée, aucune re-lecture requise pour les stores
  existants.
- **`KdfMode::Argon2id` (= 1)**, nouveau : la passphrase humaine passe
  d'abord par Argon2id, dont la sortie (32 octets, haute entropie) joue le
  rôle que `user_key` joue aujourd'hui dans `derive_kek` — **jamais**
  Argon2id directement comme KEK, toujours composé avec le même
  domain-separation SHA-256 existant. Concrètement :

  ```text
  stretched = Argon2id(passphrase, kdf_salt, m, t, p)         // 32 octets
  KEK       = SHA-256(KEK_DOMAIN_PASSPHRASE || salt || stretched)
  ```

  `KEK_DOMAIN_PASSPHRASE` (`b"basemyai-engine/kek/passphrase/v1"`) est un
  domaine **distinct** de `KEK_DOMAIN` (mode brut) — même discipline que le
  commentaire actuel de `KEK_DOMAIN` (« domain-separation… future derivation
  change is a new label »), pour qu'un même salt reconstruit sous le mauvais
  mode ne puisse jamais accidentellement retomber sur la même KEK.

  **Salt séparé pour Argon2id** (`kdf_salt`, 16 octets, indépendant du `salt`
  du wrap SHA-256) plutôt que réutilisation du même salt pour les deux
  étapes : hygiène par défaut, coût quasi nul (16 octets), évite tout débat
  sur l'« extension of use » d'un salt entre deux primitives différentes.

- **Pourquoi Argon2id et jamais PBKDF2/scrypt pour ce mode** (§ Alternatives
  ci-dessous développe) : Argon2id est le lauréat de la Password Hashing
  Competition et la recommandation actuelle de l'OWASP Cheat Sheet et de la
  RFC 9106 pour du hachage de mot de passe/passphrase — PBKDF2 n'a pas de
  coût mémoire (parallélisable à bas coût sur GPU/ASIC), scrypt a un coût
  mémoire mais une conception plus ancienne et moins de guidance de
  paramétrage récente. Aucune ambiguïté à trancher ici : ADR-030 §1 avait
  déjà nommé Argon2id comme le candidat naturel du suivi.

- **Paramètres Argon2id proposés — et leur justification** : OWASP (2024,
  toujours la référence en usage début-2026 à ma connaissance) donne un
  plancher pour un serveur d'authentification à fort débit :
  `m = 19 MiB, t = 2, p = 1`, taillé pour absorber un grand nombre de logins
  concurrents par seconde. **Ce n'est pas notre contexte** : un conteneur
  `.bmai` se déverrouille une fois par session, en local, mono-utilisateur —
  aucune pression de débit. RFC 9106 §4 documente un second profil,
  « low-memory » en fait plus généreux en mémoire que le plancher OWASP :
  `m = 64 MiB (65536 KiB), t = 3, p = 4`. Je propose ce second profil RFC 9106
  comme **défaut** :

  - Marge de sécurité nettement au-dessus du plancher OWASP, à un coût
    temps/mémoire qui reste imperceptible pour un déverrouillage ponctuel
    (cible indicative : quelques centaines de ms sur un poste de travail
    ordinaire — **à mesurer en PR2/PR3**, pas une promesse de cette PR de
    décision).
  - `p = 4` reste raisonnable même sur un poste bas de gamme (contrairement à
    un profil `p` élevé pensé pour un serveur multi-cœurs dédié).
  - Cohérent avec la posture « hardware-aware » déjà actée pour le
    provisioning (ADR-010) : les paramètres sont **persistés par store** dans
    `crypto.meta` (jamais un défaut global implicite), donc un store créé sur
    une machine généreuse reste ouvrable sur une machine plus modeste — c'est
    la lecture qui rejoue les paramètres enregistrés, jamais un « recalcul »
    au moment de l'ouverture. Un mode `--low-memory` explicite (paramètres
    OWASP plancher `19 MiB/t2/p1`) reste une option CLI documentée pour du
    matériel contraint — **jamais un downgrade silencieux**.
  - **Non tranché ici, à trancher en PR2 avec mesure réelle** : la valeur
    exacte de `m`/`t`/`p` par défaut est une proposition motivée, pas un
    nombre gravé — le format prévoit leur persistance justement pour ne pas
    devoir choisir une seule fois pour toujours.

- **Impact format / `format.lock`** : `CryptoMeta` passe en version 2.
  Champs ajoutés en fin de structure (après `wrapped_dek`, avant `crc32`) :

  ```text
  kdf_mode:      u8         0 = RawKey, 1 = Argon2id
  # présents seulement si kdf_mode == Argon2id :
  kdf_salt:      [u8; 16]
  argon2_m_kib:  u32
  argon2_t_cost: u32
  argon2_p:      u32
  ```

  **Liaison AAD du wrap DEK (obligatoire — issue de la revue adversariale)** :
  l'AAD de `CryptoMeta:2` lie `magic ‖ version ‖ salt ‖ kdf_mode ‖ (kdf_salt ‖
  m ‖ t ‖ p si Argon2id) ‖ generation_id (§3.5)`. Deux pièges épinglés,
  vérifiés dans le code actuel :

  - `CryptoMeta::wrap_aad()` (`format/crypto.rs:210-216`) lie aujourd'hui la
    **constante compile-time** `CRYPTO_META_VERSION`. Bumper naïvement cette
    constante à 2 en gardant `wrap_aad()` tel quel ferait échouer l'ouverture
    de **tous** les stores v1 existants (`WrongEncryptionKey` sur stores
    sains — l'AAD recalculée au load ne correspondrait plus à celle qui a
    scellé le wrap). L'AAD doit lier la **version décodée du fichier**,
    jamais la constante : le wrap d'un fichier v1 continue de se vérifier
    contre `1` pour toujours.
  - Les paramètres Argon2id (`m`/`t`/`p`) et `kdf_mode` doivent être
    **explicitement** authentifiés par l'AAD. Ils sont incidemment
    auto-authentifiants aujourd'hui (ils alimentent la chaîne de dérivation,
    donc les altérer change la KEK et fait échouer l'unwrap), mais cette
    propriété est fragile — elle ne tient que tant que chaque futur champ
    reste sur le chemin de dérivation. La liaison AAD rend la défense
    anti-tampering des paramètres de coût explicite et structurelle
    (la classe d'attaque « métadonnées de rotation non authentifiées » est
    exactement celle de CVE-2021-4122 sur LUKS2, voir §3.5).

  **Re-dérivation des paramètres aux rotations** : toute rotation (rewrap par
  défaut comme `--full`) écrit un `crypto.meta` neuf (nouveau salt, nouvelle
  KEK) — c'est le point d'upgrade gratuit des paramètres KDF. Une rotation
  re-dérive donc avec les **paramètres par défaut courants**, sauf
  `--low-memory` explicitement répété : un store créé faible ne le reste pas
  à vie par inertie silencieuse.

  **Changement de mode ≠ événement de sécurité en soi** : passer de `RawKey`
  à `Argon2id` via la rotation par défaut (rewrap, même DEK) ne révoque
  **pas** l'ancienne clé — un attaquant détenant l'ancienne clé brute et une
  copie pré-upgrade de `crypto.meta` déballe toujours la même DEK et lit
  toutes les données actuelles (c'est l'écart ADR-030 §4, inchangé par un
  simple changement de mode). Si l'ancienne credential est considérée
  exposée, le changement de mode DOIT être combiné à `--full`. Le CLI
  affiche cet avertissement chaque fois qu'une rotation change `kdf_mode`
  sans `--full`.

  **Politique de remplacement du format (même discipline que §5.3/N8 pour le
  SST par blocs, et que le §7.2 d'ADR-041 pour l'absence de nouvelle entrée
  quand une clé se suffit à elle-même)** :

  - `CryptoMeta:1` (verrouillé aujourd'hui) reste un décodeur **conservé
    tel quel** — un fichier `version == 1` doit rester lisible indéfiniment
    (c'est le mode `RawKey` d'un store déjà existant), jamais réinterprété
    comme un v2 tronqué. `decode_crypto_meta` distingue par `version` avant
    tout autre champ, exactement comme aujourd'hui pour un futur mismatch
    (`UnsupportedFormatVersion` reste le mécanisme pour toute version
    ultérieure inconnue, `2` y compris tant que ce build ne la comprend pas).
  - `CryptoMeta:2` est une **nouvelle** entrée `format.lock` (nouveau nom
    versionné, pas une réécriture de l'entrée `CryptoMeta:1` existante) —
    même discipline que `EncryptedSstBlock:1` qui a **remplacé**
    `SstEnvelope:1` sans réinterpréter les anciens fichiers (`format/crypto.rs`
    module doc, ADR-039 §3). Un store v1 n'est **jamais migré en place** :
    il continue de s'ouvrir en `RawKey` jusqu'à une rotation explicite qui
    choisit le nouveau mode.
  - **Aucune ambiguïté de detection** : `version == 1` ⇒ `RawKey` implicite,
    aucun octet `kdf_mode` à lire (layout inchangé, longueur inchangée) ;
    `version == 2` ⇒ lire `kdf_mode` puis conditionnellement les 4 champs
    Argon2id. Un fichier v2 en mode `RawKey` (créé par un utilisateur qui
    choisit une clé brute même sur un binaire qui connaît le mode passphrase)
    n'écrit **pas** les champs Argon2id — `kdf_mode` seul suffit, pas de
    padding inutile.
  - Les anciens codecs (`CryptoMeta:1`) ne sont **pas supprimés** par cette
    PR — voir critères de sortie : ils ne le seront que si aucun store v1 ne
    subsiste dans le parc, jugement explicitement reporté à l'implémentation,
    jamais fait par défaut.

### 2. Zeroization — audit des fuites actuelles et wrappers proposés

Confirmé par lecture directe (pas supposé) :

| Secret | État actuel | Verdict |
|---|---|---|
| `basemyai_core::EncryptionKey` (passphrase/clé brute, CLI→bindings→resolve) | `String` nue, `Clone` dérivé, aucun `Zeroize` | **Fuite** — trace en mémoire au-delà du besoin, clones libres |
| `user_key: &[u8]` dans `create_meta`/`write_meta`/`load_meta` | emprunt d'`EncryptionKey::expose()` | Dépend entièrement du type source ci-dessus — l'emprunt lui-même ne peut pas zeroize ce qu'il ne possède pas |
| KEK (`derive_kek`) | `Zeroizing<[u8; 32]>` | **Déjà correct** — confirmé en lisant `crypto.rs:49-55` |
| DEK (`crypto::material::Dek`) | `#[derive(Clone, Zeroize, ZeroizeOnDrop)]` | **Déjà correct** — confirmé `material.rs:28-30` |
| Buffer de sortie Argon2id (nouveau, N12) | n'existe pas encore | À wrapper dès la sortie de la crate `argon2`, jamais un `Vec<u8>`/`[u8; 32]` nu |
| Mémoire interne de travail d'Argon2id (les blocs m_cost) | n'existe pas encore | **Vérifié (amendement)** : `argon2` n'est pas encore dans l'arbre (`Cargo.lock`) ; `cargo add argon2` apporte 0.5.3, qui expose bien une feature Cargo `zeroize` (optionnelle — `alloc`/`password-hash`/`rand` par défaut), compatible avec le `zeroize` 1.9.0 déjà verrouillé au workspace. **À activer explicitement** dans le `Cargo.toml`. |
| État interne du cipher `XChaCha20Poly1305` après usage | déjà couvert | **Vérifié (amendement)** : à la version épinglée 0.10.1, `zeroize` est une dépendance **non optionnelle** de `chacha20poly1305`, et sa dépendance `chacha20` est épinglée avec `features = ["zeroize"]` inconditionnellement — la zeroization du matériel de clé du cipher est toujours active, il n'y a **rien à activer**. La ligne « non confirmée » de la première version de cet ADR est résolue. |
| `Memory::open` — `key.expose().to_string()` (`crates/basemyai/src/memory/mod.rs:136`) | `String` nue re-matérialisée hors de tout wrapper | **Fuite réelle** — wrapper `EncryptionKey` ne protège rien si le premier consommateur en re-crée une copie non protégée |
| CLI — clone déplacé vers `open_encrypted` (`crates/basemyai-cli/src/context.rs:56`) | clone d'`EncryptionKey` | Couvert **si** `EncryptionKey` devient zeroizable (chaque clone zeroizé à son propre drop) |
| REST — `db_key.expose().to_string()` (`crates/basemyai-rest/src/main.rs:50`) | copie `String` nue conservée **pendant toute la vie du serveur** | **Fuite la plus grave** — durée de vie maximale, process longue durée exposé réseau |
| Intermédiaires de résolution (`std::env::var`, `fs::read_to_string`, `trim().to_string()` dans `key.rs`) | buffers droppés nus | Best-effort seulement : la variable d'environnement `BASEMYAI_DB_KEY` reste de toute façon une copie plaintext dans l'environnement du process pour toute sa durée de vie — documenter, ne pas prétendre corriger |

Wrappers proposés (types, pas des commentaires « penser à zeroize ») :

- **`EncryptionKey`** : remplacer le champ interne `String` par
  `zeroize::Zeroizing<String>` (ou une struct dédiée
  `SecretString(Zeroizing<String>)` si un `Drop` custom devient nécessaire
  pour un log d'audit) — zeroize garanti au dernier drop, y compris pour
  chaque clone. Le `Clone` dérivé peut rester (chaque clone est
  indépendamment zeroizé à son propre drop, ce n'est pas un usage sans fin —
  mais **auditer les sites d'appel** pour réduire le nombre de clones vivants
  simultanément reste un objectif qualitatif, pas un critère de sortie
  bloquant).
- **Argon2id output** : `Zeroizing<[u8; 32]>` immédiatement à la sortie de
  `argon2::Argon2::hash_password_into` (ou équivalent bas niveau) — même
  patron que `derive_kek` actuel, pas un nouveau concept.
- **`create_meta`/`write_meta`/`load_meta`** : signature inchangée en
  surface (`&[u8]`) est correcte **si** l'appelant garantit que le buffer
  source est déjà zeroizable à son drop — donc le vrai correctif est en amont
  (`EncryptionKey`), pas dans ces fonctions qui empruntent déjà sans copier.
- **Feature `zeroize` des crates tierces** : résolu par l'amendement (voir
  tableau) — `argon2` 0.5.3 : feature `zeroize` à activer explicitement ;
  `chacha20poly1305` 0.10.1 : déjà inconditionnelle, rien à faire. Documenter
  dans le module doc de `crypto.rs` (comme le commentaire `KEK_DOMAIN`).
- **Critère de sortie grep-able (même esprit que le grep d'agnosticité
  ADR-001)** : aucune re-matérialisation `String` du secret hors
  d'`EncryptionKey` — `grep -rn 'expose()' crates bindings | grep to_string`
  doit retourner **zéro**. Les trois sites listés dans le tableau
  (`memory/mod.rs:136`, `basemyai-rest/src/main.rs:50`, et tout site
  équivalent introduit entre-temps) doivent être refactorés pour consommer
  `&EncryptionKey`/`&[u8]` emprunté, jamais une copie possédée non
  zeroizable. Sans ce critère, wrapper `EncryptionKey` livre une protection
  qui ne protège rien.

**Périmètre RAM — explicite (la première version ne redessinait pas la
frontière qu'ADR-030 avait exclue)** : l'objectif de la zeroization est de
**réduire la durée de vie des copies du secret en RAM**, rien de plus. Hors
périmètre, explicitement : `mlock`/`VirtualLock` (pas de verrouillage de
pages), fichier d'échange/hibernation (une page contenant le secret peut
être écrite sur disque par l'OS), core dumps, attaques cold-boot. Un lecteur
ne doit jamais lire « zeroization livrée » comme « la mémoire est
protégée ». État sérialisation vérifié sain aujourd'hui : `EncryptionKey` ne
dérive pas `Serialize`, les `Debug` de `EncryptionKey`/`CryptoContext`/
`Dek`/`Nonce`/`Salt` sont tous masqués — critère de sortie : ça le reste.

### 3. Rotation complète de la DEK (`basemyai key rotate agent.bmai --full`)

**Aujourd'hui** (`crypto.rs::write_meta`, confirmé par lecture + le test
`rewrap_preserves_dek_and_switches_keys`) : la rotation ADR-030 re-scelle la
**même** DEK sous une nouvelle KEK, O(1), un seul fichier `crypto.meta`
remplacé par tmp+fsync+rename. **Elle reste l'opération par défaut** de
`basemyai key rotate` (pas de régression de perf pour le cas courant :
changement de passphrase sans changer le modèle de menace).

`--full` est une **opération distincte** : nouvelle DEK aléatoire, donc tout
WAL et tout SST doivent être re-scellés — ADR-030 §4 l'avait déjà anticipé
comme le « chantier de suivi » si le modèle de menace se durcit (ancienne
clé + ancienne copie de `crypto.meta` ne doit plus pouvoir lire les données
**actuelles**, pas seulement échouer sur le fichier de clé).

**Design crash-safety, réutilisant des idiomes déjà en place — aucune
nouvelle primitive de bas niveau inventée :**

1. **Génération de répertoire + pointeur, pas un journal multi-fichiers.**
   Le précédent direct existe déjà : `store.meta`
   (`format/store_meta.rs`, ADR-039 §7) est *exactement* un « marqueur de
   génération » — un petit fichier qui déclare quelle génération de layout
   ce répertoire contient, écrit tmp+fsync+rename, et dont l'absence/version
   inattendue est le signal qu'un lecteur ne doit pas continuer à l'aveugle
   (`EngineError::UnsupportedStoreFormat`). La rotation complète réutilise le
   **même patron**, à un niveau au-dessus : un fichier `generation.meta` (nom
   à confirmer en PR3) à la racine du store, contenant l'identifiant de la
   génération SST/WAL actuellement active (`current: u64`), publié par
   tmp+fsync+rename — le même idiome que `crypto.rs::write_meta` utilise
   déjà pour `crypto.meta` lui-même (`OpenOptions::create+write+truncate`,
   `sync_all`, `fs::rename`).
2. **Étapes de la rotation complète — passe fusionnée (TRANCHÉ par
   l'amendement, la question « forcer une compaction d'abord ? » est
   dissoute)**. La rotation `--full` **est** une compaction full-merge qui
   écrit sa sortie sous une autre DEK dans un autre répertoire — une seule
   passe de lecture, jamais deux. Fondement dans le code réel (analyse de
   faisabilité, file:line) :

   - `Engine::compact()` est déjà un full-merge naïf — tous les SST →
     `BTreeMap` → un seul SST de sortie (`store/engine.rs:651-684`), et
     `compact_now()` = `flush()` + `compact()` inconditionnel
     (`engine.rs:638-644`). Après `flush()`, le WAL est tronqué
     (`Wal::reset()`, `engine.rs:616`) : **il n'existe plus aucun record WAL
     à re-sceller**. Le chemin « re-scellement WAL record par record »
     envisagé par la première version n'a pas lieu d'exister — la nouvelle
     génération naît avec un WAL vide (`Wal::open_for_append` crée le
     fichier au premier open, `wal.rs:39-48`). Aucun système étudié
     (CockroachDB, TiKV, MariaDB, LUKS2, SQLCipher) ne re-scelle un
     journal : tous laissent les segments mourir et écrivent les neufs sous
     la nouvelle clé.
   - La couture existe déjà : `BlockSstFile::write_new(dir, id, entries,
     block_size, crypto)` prend le répertoire cible **et** le
     `CryptoContext` en paramètres (`store/sst_block.rs:309-315`), sans
     jamais toucher l'état de l'`Engine` ; le site de compaction actuel
     passe `&self.dir` + `self.crypto` (`engine.rs:663`) — la rotation passe
     `&gen_dir` + `&new_ctx`. Les lectures sous l'ancienne clé ne demandent
     rien : chaque `BlockSstFile` clone son `CryptoContext` à la
     construction (`sst_block.rs:210-214`).
   - Le `crypto.meta` de la nouvelle génération est une primitive existante
     appelée telle quelle : `crypto::create_meta(gen_dir, user_key)` génère
     une DEK fraîche et l'écrit tmp+fsync+rename (`crypto.rs:120-178`).
   - **Bilan sécurité de la passe fusionnée : zéro nouveau code
     cryptographique** — chaque seal/unseal/génération de DEK est une
     fonction existante et testée, appelée avec d'autres arguments. La
     variante deux-passes aurait exigé un re-scelleur par fichier (décoder
     chaque section sous l'ancien contexte, re-sceller sous le nouveau avec
     AAD remappée) : un chemin crypto **neuf** avec ses propres tests, pour
     le seul privilège de lire et écrire tout le store deux fois.
   - Bonus sécurité du merge : les records masqués et les tombstones ne sont
     **pas** transportés dans la nouvelle génération (le merge les purge) —
     un re-scellement fichier-par-fichier les aurait préservés.
   - Coût honnête : le full-merge matérialise toutes les entrées vivantes en
     RAM (`BTreeMap`, `engine.rs:654`) — même profil que chaque compaction
     actuelle (exercé par le soak 1M de N11.4), mais désormais déclenchable
     par l'opérateur sur des stores potentiellement plus gros. Pas un risque
     nouveau ; à documenter côté CLI avec le coût disque.

   **Invariants d'hygiène de clé de la séquence (revue adversariale — à
   épingler par test, pas seulement par convention)** :

   - La nouvelle DEK n'est **jamais** wrappée sous l'ancienne KEK — son seul
     wrap est dans le `crypto.meta` de `gen-<n+1>/`, sous la KEK dérivée de
     la credential (nouvelle ou courante) fournie à `--full`.
   - Chaque génération contient **son propre** `crypto.meta` (le layout perd
     le `crypto.meta` à la racine : `crypto.meta`, `wal.log` et les SST
     vivent **dans** `gen-<n>/` ; `store.meta` reste à la racine — il
     versionne le layout, y compris le schéma de génération lui-même).
     `generation.meta` ne contient **aucun** matériel de clé.
   - Une rotation interrompue puis relancée génère une DEK **fraîche** —
     jamais de réutilisation de la DEK d'un `gen-<n+1>/` orphelin trouvé sur
     place (l'orphelin est supprimé et la séquence repart de zéro).

   **Publication et suppression (aligné sur la posture Windows-first
   documentée du moteur — la première version introduisait un idiome
   « fsync du répertoire parent » que le codebase rejette explicitement,
   `sst_block.rs:408-414`)** :

   - `fsync` de chaque **fichier** de la nouvelle génération avant toute
     publication (discipline existante du writer SST et de `write_meta`).
   - **Publication atomique** : `generation.meta` (tmp+fsync+rename) bascule
     `current` de `n` vers `n+1` — l'unique point de non-retour. Avant cette
     rename, un crash laisse `gen-<n+1>/` orphelin, ignoré puis nettoyé à la
     réouverture ; après, `gen-<n+1>/` est la seule génération qu'un lecteur
     honore.
   - **Suppression de `gen-<n>/` : best-effort, jamais bloquante** — même
     posture que la suppression des vieux SST par la compaction
     (`let _ = fs::remove_file(...)`, « a space leak, not a correctness
     issue », `engine.rs:670-682`). Impératif Windows : le handle WAL de
     l'ancienne génération (le seul handle longue durée du moteur,
     `wal.rs:21-26` — les `BlockSstFile` ne tiennent aucun handle,
     `sst_block.rs:189-215`) doit être fermé (remplacé par le WAL de la
     nouvelle génération dans l'état vivant de l'`Engine`) **avant** la
     tentative de suppression, sinon `remove_dir_all` échoue en
     `ERROR_SHARING_VIOLATION`. Un `gen-<n>/` résiduel (suppression échouée
     ou crash) est GC au prochain open — un résidu qui s'attarde étend la
     fenêtre d'exposition de l'ancienne clé sur disque sans aucun bénéfice,
     donc le GC est systématique, pas opportuniste.

   **Verrou d'exclusivité (nouveau constat de l'analyse — le problème
   préexiste à N12)** : le moteur n'a aujourd'hui **aucune** exclusivité
   inter-process — pas de fichier de verrou, pas de crate de locking dans
   l'arbre, `Engine::open_inner` ne vérifie rien. Un `basemyai rotate` CLI
   contre un store qu'un serveur REST tient ouvert **réussit silencieusement
   aujourd'hui** : deux `Engine` indépendants, deux memtables, appends et
   troncatures concurrents du même `wal.log`, `next_sst_id` en collision —
   un hasard de corruption réel et non détecté, pour toute commande
   d'écriture CLI, pas seulement la rotation. N12 introduit donc un **verrou
   advisory obligatoire** (fichier de verrou pris à `Engine::open`, relâché
   au drop) pour tout open en écriture — pas seulement `--full` — et
   `--full` refuse de démarrer si le verrou est tenu. Les surfaces
   longue-durée (REST/MCP) doivent en plus refuser proprement les requêtes
   pendant une rotation locale (erreur typée/503), plutôt qu'un comportement
   indéfini.
3. **Propriété d'invariant recherchée (reformulée par l'amendement — la
   version initiale, « exactement une des deux générations est ouvrable »,
   est fausse dans la fenêtre post-publication/pré-suppression où les deux
   générations complètes coexistent, chacune sous sa clé, et serait donc
   intestable au fail-point « pendant la suppression »)** : à tout instant
   d'une interruption, **la génération désignée par `generation.meta` est
   intégralement ouvrable** — jamais un pointeur vers une génération
   partiellement écrite, parce que la rename qui change `current` est le
   tout dernier acte, après tous les fsync de contenu. **Toute autre
   génération présente sur disque (orpheline pré-publication, résiduelle
   post-publication) est ignorée par les lecteurs et garbage-collectée au
   prochain open.** `verify`/`repair` résolvent le pointeur d'abord et ne
   descendent jamais dans une génération non courante, sauf pour la
   supprimer — pas de diagnostic ambigu « deux stores dans un répertoire ».
   C'est la même garantie que `crypto.meta`/`store.meta` offrent déjà à
   leur échelle, étendue à « tout le contenu du store ».
4. **N13 (ADR-043, version sets/snapshots) est-il un prérequis ?** — **Non,
   tranché et maintenant étayé par le prior art (amendement)**. La rotation
   `--full` est un événement rare, piloté par l'opérateur, hors du chemin
   critique d'écriture normale — elle exige un accès exclusif au store
   (appliqué par le verrou advisory du §3.2, plus une convention). Ce que la
   recherche établit :

   - **L'exclusivité est le point de départ de tout le monde** : SQLCipher/
     SEE `rekey` (connexion unique), LUKS1 `cryptsetup-reencrypt` (volume
     désactivé), MySQL/Percona (rebuild de tablespace verrouillé).
   - **Les systèmes online ont payé une machinerie énorme** : CockroachDB/
     TiKV re-chiffrent *lazily* par le churn de compaction — il leur a fallu
     un registre par-fichier (`COCKROACHDB_REGISTRY`), le support de
     plusieurs clés simultanément vivantes, et ils ne savent **toujours
     pas** forcer la complétion (issue ouverte cockroach#74804, « may take
     several days ») ; MariaDB a des threads de fond dédiés + versions de
     clé par page + throttling IOPS + catalogue de progression ; LUKS2 a le
     hotzone journalisé, des modes de résilience, une réparation
     automatique — **et sa pire vulnérabilité (CVE-2021-4122) vit
     précisément dans ce chemin de recovery online**.
   - **Le pattern retenu ici a un précédent béni** : la réponse officielle
     de ZFS à la ré-encryption complète est `zfs send | zfs recv` vers un
     nouveau dataset scellé puis bascule — littéralement « génération
     sibling + pointeur ». Notre machine d'états de crash a exactement trois
     états, tous résolus par une rename atomique — contre le hotzone LUKS2
     ou les versions par page MariaDB.

   Pour un moteur embarqué mono-writer sur des stores de l'ordre du Go, la
   passe fusionnée s'exécute en secondes/minutes d'accès exclusif —
   l'online-ness n'achète rien ici. Si le besoin évolue, le chemin
   d'évolution connu-bon est celui de CockroachDB (nouveaux SST sous
   nouvelle DEK via la compaction normale + tag de clé par fichier + GC de
   l'ancienne clé quand le dernier vieux fichier meurt) — qui recoupe alors
   le version-set d'ADR-043. **Clause de révision élargie (amendement)** :
   ce jugement doit être révisé avant PR3 non seulement si l'exigence
   devient « rotation sans interrompre les lecteurs », mais aussi si
   `basemyai-rest` doit un jour offrir une rotation sans indisponibilité
   perceptible pour ses clients — dans l'intervalle, REST/MCP refusent
   proprement (erreur typée/503) pendant une rotation, cf. §3.2.

5. **Rollback et limites du modèle de menace (nouveau — revue adversariale +
   prior art)**. `generation.meta` est un petit fichier en clair : sans
   contre-mesure, un attaquant avec accès disque en écriture qui a conservé
   `gen-<n>/` (avec son `crypto.meta`) peut re-pointer `current` vers
   l'ancienne génération. Si la passphrase n'a pas changé pendant `--full`
   (autorisé : `--full` = nouvelle DEK, credential identique), la
   réouverture est **totalement silencieuse** — pour une mémoire d'agent IA,
   c'est une vraie attaque (resservir des souvenirs injectés-puis-rotés),
   pas juste de la donnée périmée. La leçon de CVE-2021-4122 (LUKS2) est
   exactement celle-là : des métadonnées de rotation non authentifiées sur
   lesquelles le chemin d'ouverture **agit** sont une surface d'attaque.
   Décision en deux volets, aux ambitions honnêtement distinctes :

   - **Durcissement bon marché, fait dans N12** : l'identifiant de
     génération est lié dans l'AAD du wrap DEK de chaque `crypto.meta` (§1)
     — un pointeur bascule vers une génération dont le `crypto.meta` déclare
     un autre id échoue **bruyamment** au lieu de réussir en silence ou de
     dérouter verify/repair. Impossible à retrofitter sans nouveau bump de
     version : c'est maintenant, au design du format v2, que ça se décide.
   - **Hors périmètre, déclaré tel** : le rollback du **répertoire entier
     cohérent** (pointeur + génération + crypto.meta restaurés ensemble
     depuis un backup) est indétectable de l'intérieur du store — c'est
     l'équivalent d'une restauration de backup. Le prior art est unanime :
     la protection anti-rollback exige un compteur monotone dans un stockage
     que l'attaquant ne peut pas rembobiner (RPMB/TPM chez Android Verified
     Boot, compteurs sécurisés chez TF-M) — indisponible à un moteur
     userspace pur, et AVB lui-même documente que sans stockage
     inviolable, la protection est contournable. Ancre externe future
     naturelle : le keyring OS (§4) peut mémoriser `(passphrase, id de
     génération attendu)` et rendre le rollback détectable **sur cette
     machine** — une phrase de durcissement V2, pas une promesse N12.

### 4. Keyring OS (DPAPI / Keychain / Secret Service) — scope surfaces uniquement

**Le moteur (`basemyai-engine`) ne doit jamais connaître un type de keyring
OS** — même principe d'agnosticité que `basemyai-core` vis-à-vis du métier
(CLAUDE.md racine : « mécanisme au core, sens au consommateur »). L'engine
ne voit toujours qu'un `&[u8]`/`Zeroizing<String>` déjà résolu ; d'où vient
ce secret ne le regarde pas.

- **Frontière** : un trait côté surface, p. ex.

  ```rust
  trait KeyringBackend {
      fn store(&self, service: &str, account: &str, secret: &SecretString) -> Result<(), KeyringError>;
      fn retrieve(&self, service: &str, account: &str) -> Result<SecretString, KeyringError>;
      fn delete(&self, service: &str, account: &str) -> Result<(), KeyringError>;
  }
  ```

  Implémenté par une crate d'intégration OS (`keyring` crate ou équivalent,
  qui abstrait déjà DPAPI/Keychain/Secret Service) **uniquement référencée
  depuis `basemyai-cli`** (et plus tard bindings/REST si le besoin apparaît),
  jamais depuis `basemyai-engine` ni `basemyai` (façade mémoire).
- **Premier consommateur : le CLI**, cohérent avec ADR-034 §6 qui renvoyait
  déjà explicitement ce sujet à une « section V2 » et avec le fait que
  `basemyai-cli` existe déjà et porte déjà `config key generate|path|check` —
  le point d'extension naturel est un nouveau sous-mode
  `config key use-keyring` qui appelle `KeyringBackend::store` après
  résolution ADR-034, et une option de résolution supplémentaire (rang à
  définir en PR3, probablement entre l'argument explicite et les variables
  d'environnement) qui appelle `KeyringBackend::retrieve` avant de tomber sur
  les sources actuelles.
- **Trois points épinglés par la revue adversariale, à trancher avant PR3
  (amendement)** :
  - *Schéma de nommage* : `service = "basemyai"`, `account` = **UUID stable
    du store enregistré dans `store.meta`** — jamais le chemin (déplacer le
    store orphelinerait l'entrée) ni une constante (deux stores
    entreraient en collision et une passphrase en écraserait une autre en
    silence).
  - *Déplacement honnête du modèle de menace* : un keyring OS déverrouillé
    (Secret Service, DPAPI user-scope, Keychain) livre le secret à **tout
    process s'exécutant sous le même utilisateur** — adopter le keyring
    fait passer de « la passphrase est dans la tête de l'utilisateur » à
    « tout code du même utilisateur peut la lire ». Pour un produit
    local-first, c'est un affaiblissement réel à documenter en une phrase
    honnête au moment de l'opt-in, jamais un défaut silencieux.
  - *Interaction avec la rotation* : après une rotation qui change la
    passphrase, une entrée keyring périmée est une **copie persistée de
    l'ancienne passphrase** — le flux de rotation doit mettre à jour ou
    supprimer l'entrée, et un échec de `delete()` doit être remonté à
    l'opérateur, jamais avalé.
- **REST n'implémente pas `KeyringBackend`** — normatif, pas « si le besoin
  apparaît » : un serveur qui persiste la credential de déverrouillage dans
  le keyring de son compte de service cumule les deux pires propriétés
  (longue durée de vie + périmètre same-user élargi). La clé de REST reste
  fournie par son environnement de déploiement (ADR-034).
- **Pas de design d'implémentation complet ici** — seulement la frontière :
  aucun test, aucune dépendance ajoutée, dans cette PR de décision. (Fait
  vérifié : la crate `keyring` n'est pas dans l'arbre de dépendances
  aujourd'hui.)

### 5. Critères de sortie (adaptés du plan §9, rendus testables)

- [ ] Une clé/passphrase **retirée** de rotation (ancien mode ou ancienne
  valeur) échoue `open` avec `WrongEncryptionKey` — jamais un succès
  partiel, jamais un panic.
- [ ] L'ancienne clé **combinée à une copie de l'ancien `crypto.meta`**
  (mode `--full` seulement — c'est exactement l'écart qu'ADR-030 §4
  documentait comme non couvert) ne peut plus déchiffrer **aucun** octet de
  la nouvelle génération : ni WAL ni SST — test explicite qui copie
  l'ancien `crypto.meta` à côté de la nouvelle génération et vérifie
  l'échec AEAD sur chaque artefact, pas seulement sur `crypto.meta`.
- [ ] Un fichier `crypto.meta` `version == 1` (mode `RawKey` historique) se
  décode et s'ouvre **sans aucune modification de comportement** — test de
  non-régression explicite sur un fixture v1 gelé.
- [ ] Un fichier `crypto.meta` `version == 2` en mode `Argon2id` refuse de
  s'ouvrir avec la clé brute correspondante interprétée comme mode `RawKey`
  (les deux modes ne se substituent jamais silencieusement l'un à l'autre).
- [ ] Crash injecté (fail-point, même mécanisme que
  `crate::fail_point!("after_crypto_meta_write")` déjà présent) à **chacune**
  des étapes de la rotation `--full` listées en §3.2 : après génération de la
  nouvelle DEK, en cours d'écriture de chaque SST re-scellé, après le fsync
  de contenu mais avant la rename de `generation.meta`, juste après cette
  rename, et pendant la suppression de l'ancienne génération — dans tous les
  cas, `verify --logical` (ADR-040, mode le plus profond) reste vert et **la
  génération désignée par `generation.meta` est intégralement ouvrable ;
  toute autre génération présente est ignorée puis GC au prochain open**
  (invariant §3.3 — la formulation initiale « exactement une des deux »
  était intestable dans la fenêtre post-publication/pré-suppression).
- [ ] Altérer un seul octet des champs KDF v2 (`kdf_mode`, `kdf_salt`,
  `argon2_m_kib`/`t_cost`/`p` — CRC recalculé pour passer le décodage
  structurel) fait échouer l'unwrap (AAD, §1) — le test anti
  parameter-tampering explicite.
- [ ] Un `generation.meta` re-pointé vers une génération dont le
  `crypto.meta` déclare un autre id de génération échoue **bruyamment** à
  l'ouverture (AAD du wrap, §3.5) — jamais une réouverture silencieuse de
  l'ancienne génération par simple bascule du pointeur.
- [ ] `grep -rn 'expose()' crates bindings | grep to_string` retourne zéro
  (§2) — les sites `memory/mod.rs:136`, `basemyai-rest/src/main.rs:50` et
  équivalents refactorés en emprunt ; `EncryptionKey` zeroizable ; les
  `Debug` masqués le restent et `Serialize` reste absent.
- [ ] Le verrou advisory (§3.2) est en place pour tout open en écriture :
  test « second open en écriture refusé pendant qu'un premier est vivant »,
  et `--full` refuse de démarrer si le verrou est tenu.
- [ ] La documentation opérateur du `--full` (CLI + docs) énonce les trois
  limites d'honnêteté : coût disque temporaire ×2, rémanence SSD (l'ancien
  ciphertext n'est pas effacé du support de façon garantie — la promesse est
  « l'ancienne clé ne lit plus le store courant », jamais « l'ancien
  ciphertext est irrécupérable du médium »), et non-protection rétroactive
  des backups/copies antérieurs à la rotation.
- [ ] Le nouveau format crypto (`CryptoMeta:2`, tout champ Argon2id) est
  documenté dans le module doc de `format/crypto.rs` avec le même niveau de
  détail que la documentation actuelle de `CryptoMeta:1`, et verrouillé dans
  `format.lock` (`cargo xtask format-lock` vert).
- [ ] Les anciens codecs crypto ne sont supprimés **que si** aucun store v1
  ne doit plus être ouvert par ce build — décision explicite prise en PR3/PR
  finale, jamais par défaut dans cette PR de décision (le plan §14 exige
  cette question posée avant l'implémentation, pas après).
- [ ] `cargo xtask ci` vert, `cargo xtask test-crash-consistency` étendu pour
  couvrir le mode `--full` (même exigence que le kill-loop réel déjà
  existant pour le mode `batch` chiffré, ADR-030 §6).

## Alternatives rejetées

- **PBKDF2 ou scrypt au lieu d'Argon2id pour le mode passphrase** : PBKDF2
  n'a aucun coût mémoire — parallélisable à très bas coût sur GPU/ASIC, ce
  qui est précisément le scénario qu'un étirement de clé doit rendre
  coûteux pour un attaquant qui a volé `crypto.meta`. scrypt a un coût
  mémoire mais une conception plus ancienne, moins de guidance de
  paramétrage récente (RFC 9106/OWASP convergent sur Argon2id comme choix
  actuel) et une implémentation Rust pure moins largement auditée dans
  l'écosystème RustCrypto que `argon2`. Rejeté pour les deux.
- **Ré-encryption in-place des fichiers de données existants** (au lieu
  d'une génération séparée) : exactement le danger qu'ADR-030 §4 a déjà
  écarté pour justifier le design DEK/KEK — une ré-écriture in-place
  interrompue laisse un état mixte (« la moitié des SST sur l'ancienne
  DEK ») qui n'a **aucune** façon propre de se distinguer d'une corruption.
  Le prior art le confirme empiriquement (amendement) : SQLCipher
  `PRAGMA rekey` (in-place, page par page) a des rapports de terrain
  récurrents de bases rendues illisibles (sqlcipher#98, incidents de
  coupure de courant), son vendeur ne documente **aucune** garantie de
  crash-safety pour rekey et oriente lui-même vers `sqlcipher_export()` —
  c'est-à-dire la copie vers un nouveau fichier, le pattern génération. Le
  répertoire de génération transforme cette question en un problème déjà
  résolu ailleurs dans le moteur (tmp+fsync+rename d'un marqueur), au prix
  d'un doublement temporaire de l'espace disque pendant la rotation —
  jugé acceptable pour une opération rare et explicite, documentée comme
  telle dans le CLI (`--full` doit avertir de ce coût).
- **Deux passes (compacter sous l'ancienne clé, puis re-sceller
  fichier par fichier)** — rejeté par l'amendement (§3.2) : double la
  lecture/écriture intégrale du store, laisse à mi-chemin un état
  intégralement compacté mais encore entièrement sous l'ancienne clé (un
  passif pur), transporterait les tombstones dans la nouvelle génération,
  et surtout exigerait un chemin crypto **neuf** (re-scelleur par section
  avec AAD remappée) là où la passe fusionnée n'appelle que des fonctions
  crypto existantes et testées. Un état mixte par fichier exigerait en
  outre un suivi de clé par fichier — CockroachDB a eu besoin d'un fichier
  registre entier (`COCKROACHDB_REGISTRY`) pour exactement ça.
- **Re-scellement du WAL enregistrement par enregistrement** — rejeté par
  l'amendement : sans objet après `flush()` (WAL vide par construction,
  `engine.rs:616`), aucun prior art ne le fait (tous les systèmes étudiés
  laissent les segments de journal mourir et écrivent les neufs sous la
  nouvelle clé), et chaque état de recovery « quel segment est sous quelle
  clé » supprimé est une classe de bugs supprimée.
- **Attendre la machinerie complète de N13 (version sets/snapshots,
  ADR-043) avant de faire quoi que ce soit sur la rotation complète** :
  rejeté en §3.4 ci-dessus — la rotation `--full` exclusive (pas de writer
  concurrent) est un problème strictement plus simple que le multi-writer
  d'ADR-043, et le plan lui-même proscrit d'anticiper une machinerie
  « pour afficher une feature » (§10). Signalé comme jugement révisable si
  l'exigence réelle s'avère être « rotation sans interrompre les
  lecteurs ».
- **Réutiliser le même salt pour Argon2id et pour le wrap SHA-256 KEK** :
  rejeté au profit de deux salts indépendants (§1) — le coût (16 octets) est
  négligeable face à l'incertitude qu'éviter toute question de réutilisation
  de salt entre deux primitives distinctes fait disparaître.
- **Un unique buffer/type `Secret` générique pour tous les secrets du
  workspace** (au lieu de wrappers spécifiques `EncryptionKey`/Argon2id
  output) : envisagé puis écarté pour cette PR — chaque secret a une durée
  de vie et un point de création différents (résolution CLI vs sortie de
  fonction de dérivation), et le patron `Zeroizing<T>` déjà en usage dans
  `crypto.rs` couvre le besoin sans introduire une nouvelle abstraction
  transverse dont la portée dépasserait N12.

## Conséquences

- (+) Compat totale : aucun store v1/mode `RawKey` existant n'est affecté ;
  `format.lock` gagne une entrée, n'en modifie aucune.
- (+) La rotation `--full` referme l'écart documenté honnêtement par ADR-030
  §4 depuis le premier jour — sans avoir attendu ni anticipé N13.
- (+) Le keyring OS reste hors du moteur — l'invariant d'agnosticité tient à
  la même échelle que celui de `basemyai-core`.
- (−) Une rotation `--full` double temporairement l'espace disque du store
  (deux générations coexistent le temps de l'opération) — assumé, coût
  documenté côté CLI, pas un défaut caché.
- (−) Les paramètres Argon2id par défaut proposés ici (§1) ne sont **pas**
  mesurés sur du matériel réel — PR2/PR3 doivent produire cette mesure avant
  de figer le défaut en dur ; cette PR documente le raisonnement, pas un
  nombre validé empiriquement.
- (−) **Rémanence (honnêteté, amendement)** : supprimer `gen-<n>/` n'efface
  pas l'ancien ciphertext du support physique — wear-leveling SSD, absence
  de garantie TRIM, journalisation du filesystem (sources unanimes :
  FAQ cryptsetup « you cannot reliably erase parts of SSDs », man page ZFS
  `change-key` « accessible via forensic analysis for an indeterminate
  length of time »). La promesse du produit est exactement : « après
  `--full`, l'ancienne credential + l'ancien `crypto.meta` ne peuvent plus
  lire le store **courant** » — jamais « l'ancien contenu est irrécupérable
  du médium ». Un TRIM/overwrite best-effort de l'ancienne génération peut
  être offert, étiqueté best-effort (le modèle ZFS : `zpool trim --secure`
  « si le matériel le supporte »).
- (−) **Non-protection rétroactive (amendement)** : toute copie/backup de
  {ancienne génération + ancien `crypto.meta`} reste déchiffrable avec
  l'ancienne credential pour toujours — la rotation est une protection
  **vers l'avant** ; roter après une fuite suspectée ne protège que ce que
  l'attaquant n'a pas déjà copié. Documenté côté CLI et docs opérateur.
- (−) Le full-merge de la rotation matérialise toutes les entrées vivantes
  en RAM (même profil O(données vivantes) que la compaction actuelle,
  exercé à 1M par le soak N11.4) — pas un risque nouveau, mais désormais
  déclenchable explicitement par l'opérateur sur de gros stores ; mentionné
  dans la doc CLI avec le coût disque.

## Points signalés pour revue humaine avant implémentation

**Résolus par l'amendement du 2026-07-15** (recherche prior art + analyse de
faisabilité file:line + revue adversariale) :

- ~~Feature `zeroize` des crates tierces~~ — vérifié : `argon2` 0.5.3 l'a
  (optionnelle, à activer), `chacha20poly1305` 0.10.1 l'a inconditionnelle
  (§2).
- ~~WAL non compacté au moment d'un `--full`~~ — dissous : la rotation est
  une passe fusionnée flush+full-merge, la nouvelle génération naît avec un
  WAL vide par construction (§3.2). Le re-scellement de WAL n'existe pas.
- ~~N13 tiré en avant ou non~~ — tranché « non » et étayé par le prior art
  (§3.4), clause de révision élargie au cas « REST sans downtime ».

**Restent ouverts pour revue humaine** :

1. **Paramètres Argon2id par défaut** (§1) : proposition motivée
   (RFC 9106 second profil, `64 MiB/t3/p4`) mais non mesurée sur du matériel
   réel bas de gamme — à valider ou ajuster en PR2 avec un vrai benchmark,
   pas à prendre pour acquis.
2. **Périmètre exact du verrou advisory** (§3.2) : l'analyse a montré que
   l'absence d'exclusivité inter-process est un hasard de corruption
   préexistant (un `rotate` CLI contre un store tenu par REST double-écrit
   silencieusement le même WAL **dès aujourd'hui**) — le verrou est donc
   justifié pour tout open en écriture, pas seulement `--full`. À
   confirmer : verrou général dès N12 (recommandé — il ferme un vrai bug)
   ou minimal `--full`-seulement avec le verrou général en suivi.
3. **UUID de store dans `store.meta`** (§4) : le schéma de nommage keyring
   requiert un identifiant stable par store — l'ajouter à `store.meta`
   (bump de version) ou le dériver autrement ; à trancher en PR3 avec
   l'implémentation keyring, non bloquant pour le cœur N12.
4. **`docs/security/encryption-model.md`** référencé dans la consigne de la
   première version n'existe pas dans le dépôt — cadrage fait sur ADR-007 +
   ADR-030 + `docs/security/key-resolution.md` (qui, lui, existe). Si un
   document de modèle de menace séparé est attendu, il reste à créer — les
   paragraphes rémanence/rollback/périmètre-RAM de cet ADR en seraient le
   noyau naturel.

## Prior art consulté (amendement)

SQLCipher `PRAGMA rekey` + incidents de terrain (sqlcipher#98) et la
recommandation vendeur `sqlcipher_export()` ; SQLite SEE `sqlite3_rekey` ;
CockroachDB encryption-at-rest RFC (rotation par churn de compaction,
registre par-fichier non authentifié, complétion non forçable —
cockroach#74804, #79066) ; TiKV/TiDB encryption-at-rest ; MySQL/Percona
`ROTATE INNODB MASTER KEY` (wrap-only) vs MariaDB re-encryption online
(threads de fond, versions de clé par page, throttling IOPS) ; LUKS2
`cryptsetup reencrypt` (hotzone, modes de résilience) et **CVE-2021-4122**
(métadonnées de rotation non authentifiées → décryption partielle
silencieuse — la leçon directe de la liaison AAD §1 et du §3.5) ; ZFS
`zfs change-key` (wrap-only assumé, ré-encryption complète = send/recv vers
un nouveau dataset — le précédent direct du pattern génération) ; FAQ
cryptsetup (rémanence SSD) ; Android Verified Boot 2.0 / TF-M (protection
anti-rollback = compteur monotone + stockage inviolable, hors de portée
d'un moteur userspace pur — d'où le partage §3.5 entre durcissement bon
marché et hors-périmètre déclaré).
