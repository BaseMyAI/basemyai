# ADR-044 — Format WAL v2 anti-rejeu (CRYPTO-1)

**Statut** : 🟡 Proposed — conçu et spécifié ci-dessous, **implémentation non
commencée**. Voir « Pourquoi ce correctif n'est pas encore implémenté » en
fin de document.
**Date** : 2026-07-23
**Relation aux ADR existants** : implémente le correctif du finding CRYPTO-1
de l'audit de sécurité adversarial BaseMyAI (2026-07-22). Amende le
comportement de scellement du WAL décrit par ADR-030 §3 (« chaque
enregistrement… est scellé individuellement dans une enveloppe
`WalEnvelope:1` ») sans remettre en cause le reste d'ADR-030 (enveloppe
DEK/KEK, primitives AEAD, rotation O(1)) ni ADR-025 (fondation LSM, ordre
WAL→SST→troncature). N'amende pas ADR-039 (SST par blocs), qui a déjà son
propre anti-rejeu correctement implémenté et testé (`encrypted_sst_block_aad`,
`store/sst_block.rs`) — ce document généralise le même principe au WAL, qui
en était dépourvu.

## Contexte

Vérifié dans le code réel (`crates/basemyai-engine/src/format/crypto.rs`,
`store/wal.rs`) :

- `wal_envelope_aad()` retourne une **constante fixe de 6 octets**
  (`magic‖version`), identique pour **toute** enveloppe WAL jamais scellée
  dans un store, quels que soient sa position, son numéro de séquence, ou le
  store dans lequel elle vit.
- Le format `WalRecord` en clair (`format/wal.rs`, `WAL_RECORD_VERSION = 2`)
  n'a lui non plus aucun champ de position/séquence — seulement
  `magic‖version‖op‖key_len‖val_len‖key‖value‖crc32`.
- Contraste : `encrypted_sst_block_aad()` (même fichier) lie chaque bloc SST
  scellé à `sst_id‖section_type‖section_no`, et deux tests
  (`encrypted_block_moved_between_two_ssts_fails_authentication`,
  `encrypted_blocks_swapped_within_the_same_sst_fail_authentication`)
  prouvent qu'un déplacement ou une permutation de bloc échoue
  l'authentification. Aucun test équivalent n'existe pour le WAL, et le code
  confirme pourquoi : rien n'empêcherait qu'il passe.

**Scénario concret** (menace déjà dans le périmètre déclaré de
`SECURITY.md` : « un répertoire `.bmai` malveillant ou corrompu ») : un
attaquant disposant d'un accès en écriture au système de fichiers d'un store
chiffré — **sans la clé** — peut copier les octets exacts d'une ancienne
enveloppe `Put(clé=X, valeur périmée)` vers une position ultérieure du même
`wal.log`, après un `Delete(clé=X)` légitime. Le nonce voyage avec
l'enveloppe (généré aléatoirement à chaque scellement, jamais dérivé de la
position) et l'AAD ne change jamais : l'enveloppe dupliquée se déchiffre et
s'authentifie avec succès au prochain replay, exactement comme si elle avait
été écrite à cet endroit. De même, permuter deux enveloppes adjacentes de
même longueur inverse l'ordre effectif d'écriture pour les clés concernées.
Aucune erreur typée n'est levée dans les deux cas — le replay accepte
silencieusement une séquence falsifiée.

C'est la même classe de « ressuscitation d'une valeur périmée » que
DUR-LSM-01 (ADR-043, amendement du 2026-07-22), mais côté attaquant plutôt
que côté course de compaction ordinaire — et le WAL est la couche qui
contient les écritures **les plus récentes**, pas encore compactées.

## Décision

### 1. Portée : lier chaque enveloppe WAL à sa position exacte, sans état persisté supplémentaire

Trois candidats ont été évalués pour « qu'est-ce qui identifie la position
d'un enregistrement WAL de façon unique et vérifiable » :

- **Un compteur de séquence global, jamais remis à zéro, persisté
  explicitement.** Rejeté pour cette V2 : correct en théorie, mais exige un
  nouveau petit fichier durable (`wal_sequence.meta` ou équivalent),
  publié avec sa propre discipline crash-safe (tmp+fsync+rename) à **chaque**
  écriture WAL — un coût et une surface de correctness supplémentaires pour
  une propriété que l'option retenue obtient sans état neuf.
- **`manifest_generation` comme identifiant d'épisode.** Rejeté : évalué en
  détail pendant la conception — `manifest_generation` (déjà durable,
  publié avant chaque troncature de WAL) semblait au premier abord un bon
  candidat gratuit, mais il est **aussi** incrémenté par une compaction
  ordinaire, qui ne touche jamais le WAL. Deux enregistrements WAL écrits
  dans le **même** segment (entre deux troncatures) peuvent donc légitimement
  chevaucher des valeurs différentes de `manifest_generation` si une
  compaction s'intercale entre eux — le rendant impropre à une comparaison
  d'égalité stricte par enregistrement sans réintroduire l'ambiguïté qu'on
  cherche à éliminer.
- **`wal_epoch` dédié + offset physique comme séquence intra-épisode**
  (retenu). `wal_epoch` est un compteur **indépendant**, incrémenté
  uniquement par une troncature de WAL réussie (`Wal::reset`), publié
  durablement **avant** cette troncature (même ordre que
  manifest→troncature, ADR-025). L'offset d'écriture dans le fichier courant
  sert de séquence à l'intérieur d'un épisode — gratuit : c'est exactement
  ce que la boucle de replay track déjà en avançant `offset` au fil du
  décodage.

### 2. Format `WalEpoch:1` — nouveau petit fichier durable

```text
magic:      u32
version:    u16
wal_epoch:  u64   // incrémenté à chaque Wal::reset() réussi
crc32:      u32   over every byte above
```

Même idiome que `manifest.meta`/`crypto.meta`/`generation.meta` : tmp+fsync+
rename, un fichier par génération (vit dans le même répertoire que
`wal.log`). Publié par `Wal::reset()` **avant** la troncature elle-même —
un crash entre les deux laisse soit l'ancien `wal_epoch` (le WAL non
tronqué correspond toujours à l'épisode qu'il annonce), soit le nouveau
(le WAL déjà vide correspond au nouvel épisode) — jamais un état où le WAL
courant prétend un épisode qu'aucun fichier durable ne confirme.

### 3. Format `WalRecord:3` — bump du format en clair pour porter l'offset en position

```text
magic:       u32   = WAL_MAGIC
version:     u16   = 3
op:          u8
record_offset: u64  // offset absolu (octets) où *ce* scellé commence dans
                     // le wal.log courant — la séquence intra-épisode
key_len:     u32
val_len:     u32
key:         [u8; key_len]
value:       [u8; val_len]
crc32:       u32   over every byte above (magic..value)
```

`record_offset` est porté par le format **plaintext** aussi (pas seulement
dans l'AAD chiffrée) pour deux raisons : (1) il permet une validation
structurelle légère même sur un store non chiffré — pas une frontière de
sécurité au sens du modèle de menace (un store en clair n'a jamais prétendu
d'intégrité contre un attaquant disque), mais une hygiène de fiabilité utile
(détecte une corruption/permutation accidentelle) ; (2) le replay peut
valider `record_offset == offset` (l'offset physique réel où ce scellé a été
trouvé) **avant** de faire confiance au contenu, sans dépendre de l'AEAD pour
cette vérification structurelle basique.

### 4. AAD `WalEnvelope:2` — lie l'enveloppe chiffrée à store + épisode + position

```rust
fn wal_envelope_aad_v2(store_id: Uuid, wal_epoch: u64, record_offset: u64) -> [u8; 6 + 16 + 8 + 8] {
    // magic(4) ‖ version(2) ‖ store_id(16) ‖ wal_epoch(8) ‖ record_offset(8)
}
```

`store_id` (déjà porté par `store.meta`, ADR-042 §3.3) empêche un
attaquant de copier une enveloppe WAL valide **d'un autre store chiffré
avec la même clé** (scénario réaliste : deux stores de test partageant une
clé de développement) vers celui-ci. `wal_epoch` empêche de faire passer une
enveloppe d'un épisode WAL antérieur (déjà tronqué) comme appartenant à
l'épisode courant. `record_offset` empêche une permutation à l'intérieur du
même épisode.

### 5. Comportement au replay (`Wal::replay`, `scan_readonly`)

- L'épisode courant est lu depuis `WalEpoch:1` **avant** le replay — durci
  comme `store.meta`/`crypto.meta` : absence sur un store créé par ce build
  ⇒ corruption typée (`CorruptWalEpoch`) ; absence sur un store pré-V2 ⇒
  géré par la politique de migration (§7).
- Pour chaque enregistrement décodé : `record_offset` (plaintext) doit
  égaler l'offset physique réel où le décodage l'a trouvé — sinon
  `CorruptWal` (« déclare une position différente de celle où il apparaît »),
  **jamais** toléré comme torn tail.
- Pour le cas chiffré : l'AAD reconstruite à partir de
  `(store_id, wal_epoch_courant, offset_physique)` doit authentifier
  l'enveloppe — un échec AEAD ici est **indiscernable, à dessein**, d'un
  échec AEAD « classique » (mauvaise clé/corruption) au niveau de l'erreur
  retournée (`CorruptWal`) : un attaquant sondant les messages d'erreur ne
  doit pas pouvoir distinguer « tag invalide » de « position falsifiée ».
- Un enregistrement **complet et structurellement valide mais dont
  `record_offset`/l'AAD ne correspond pas à sa position réelle** est une
  corruption franche, jamais un torn tail — la distinction torn-tail
  existante (dernier enregistrement incomplet) reste inchangée : elle se
  décide **avant** cette validation, sur la seule base de la longueur
  disponible.
- Une seule anomalie de position dans un fichier suffit à rejeter le replay
  entier plutôt que de continuer en ignorant l'enregistrement fautif — un
  attaquant capable de falsifier un enregistrement est traité comme capable
  d'en falsifier d'autres ; accepter partiellement rouvrirait la même classe
  de trou.

### 6. Atomicité de batch préservée

Un batch reste un seul enregistrement externe (`WalOp::Batch`, `format/wal.rs`
« Batch records ») — `record_offset` s'applique à l'enregistrement externe
dans son ensemble, exactement comme le `crc32`/l'enveloppe AEAD actuels.
Aucun changement à la garantie tout-ou-rien existante.

## 7. Compatibilité et migration — décision explicite, aucun repli silencieux

Le workspace est **natif-only, pré-1.0** (`version.workspace = 0.2.0`,
non publiée — `docs/status.md`), avec un précédent déjà posé par ADR-033
(cutover dur libSQL→natif, sans chemin de migration). Décision retenue,
dans le même esprit :

- `WAL_RECORD_VERSION` passe de 2 à 3. `Wal::open_for_append`/`replay`
  **refusent typé** (`EngineError::UnsupportedFormatVersion`, déjà le
  comportement existant pour une version inconnue) un WAL v2 rencontré sans
  `WalEpoch:1` correspondant — pas de relecture silencieuse en mode
  dégradé, pas de réécriture automatique à la volée.
- Une migration explicite reste possible en suivant : ouvrir l'ancien store
  en lecture (ancien binaire ou un mode de compatibilité dédié, non
  implémenté par ce document), `export`/`import` via le format JSONL déjà
  existant (`memory::porting`), ou un outil `basemyai migrate-wal` dédié —
  **hors périmètre de cet ADR**, à trancher séparément si un besoin réel de
  migrer un store V1 existant se confirme. Tant qu'aucun store natif n'est
  publié en production, ce besoin n'est pas démontré.
- `format.lock` gagne deux nouvelles entrées (`WalRecord:3`, `WalEnvelope:2`)
  et une nouvelle (`WalEpoch:1`) — toute dérive casse la CI, même discipline
  que chaque format existant.

## 8. Tests et fuzzing requis avant implémentation acceptée

Repris de la remédiation de l'audit (Phase 6) — liste, pas encore exécutée :

- Échanger deux enregistrements de même longueur → rejet typé.
- Dupliquer un ancien `Put` après un `Delete` → rejet typé (le scénario
  CRYPTO-1 exact).
- Supprimer un enregistrement au milieu du fichier → rejet typé (position
  suivante ne correspond plus).
- Copier un enregistrement depuis un autre store (`store_id` différent) →
  rejet typé.
- Copier un enregistrement depuis une autre génération/épisode
  (`wal_epoch` différent) → rejet typé.
- Modifier `record_offset` en clair sans pouvoir recalculer un tag AEAD
  valide (cas chiffré) → échec AEAD, jamais une lecture silencieuse.
- Rejouer deux fois le même WAL (double open) → idempotent, comme
  aujourd'hui.
- Tronquer chaque octet du dernier enregistrement → torn tail toléré,
  inchangé.
- Crash injecté pendant l'écriture de `WalEpoch:1`, avant/après la
  troncature qu'il précède.
- Nouvelles fuzz targets : `wal_record_v3_decode`, `wal_epoch_decode`,
  `wal_envelope_v2_decode` — mêmes conventions que les 24 cibles
  existantes.

## Alternatives rejetées

- **Ajouter seulement l'offset courant à l'AAD sans formaliser un format
  versionné** : rejeté explicitement — c'est le correctif « rapide » que la
  remédiation de l'audit a demandé d'éviter. Sans `wal_epoch` et sans bump
  de version structurel, l'attaquant garde la possibilité de rejouer un
  enregistrement d'un épisode WAL antérieur (déjà tronqué) partageant le
  même intervalle d'offsets qu'un enregistrement courant.
- **Compteur global persisté séparément** : voir §1 — écarté pour la
  complexité et la surface de correctness supplémentaires sans bénéfice net
  sur l'option retenue.

## Pourquoi ce correctif n'est pas encore implémenté

Décision délibérée de séquencement, pas un oubli. Le WAL est le chemin de
récupération après crash le plus critique du moteur — une erreur dans son
format ou sa logique de replay a un rayon d'explosion largement supérieur à
n'importe quel autre correctif de cette remédiation (perte de données
silencieuse à l'échelle du store entier, pas d'un seul enregistrement). Cet
ADR livre la conception complète, revue et spécifiée, pour qu'une
implémentation dédiée — avec son propre cycle complet de tests, fuzzing
(§8), et `cargo xtask test-crash-consistency` étendu à `WalEpoch:1` —
puisse être menée sans la pression de temps qui a produit DUR-LSM-01 en
premier lieu. Voir le rapport de remédiation de l'audit (2026-07-23) pour le
détail de cet arbitrage.

## Critères de sortie (avant de passer ce statut à Accepted)

- [ ] `WalEpoch:1`, `WalRecord:3`, `WalEnvelope:2` implémentés et dans
  `format.lock`.
- [ ] Les 10 scénarios du §8 passent, avec au moins 3 nouvelles fuzz
  targets tournant en continu (`.github/workflows/fuzz.yml`).
- [ ] `cargo xtask test-crash-consistency` étend son harnais (`crash_writer`)
  avec des points de défaillance sur la publication de `WalEpoch:1`.
- [ ] Refus typé confirmé pour un store WAL v2 (pas de fallback silencieux),
  avec un test de reproduction.
- [ ] Politique de migration éventuelle tranchée séparément si un besoin
  réel se présente avant la clôture — sinon documentée comme non nécessaire
  (aucun store natif publié en production à ce jour).
