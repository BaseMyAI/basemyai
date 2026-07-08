# N1 — Spike Couche 1 : B-tree CoW vs LSM-tree (comparatif)

Sortie du spike `PLAN-NATIVE-ENGINE.md` Phase 0b / `TODO-NATIVE-ENGINE.md` N1.
Décision actée dans `ADR-025-native-engine-storage-foundation.md`.

## Protocole (identique pour les deux prototypes)

Prototypes jetables, mono-thread, zéro dépendance externe, clés 16 octets /
valeurs 256 octets, Windows/NTFS, `File::sync_all()` comme barrière de
durabilité (confirmée fonctionnelle sans souci particulier sous Windows dans
les deux implémentations).

- **W1** — 100 000 inserts, 100 commits fsyncés (lots de 1 000).
- **W2** — réouverture (recovery incluse), 10 000 lectures point (indices
  pseudo-aléatoires, seed différente de W1).
- **W3** — crash-consistency : 10 cycles de {spawn process `--writer` qui
  insère en boucle et atteste chaque commit fsync-é dans un log séparé →
  sleep aléatoire 200-3000 ms → `taskkill /F /PID` → réouverture +
  vérification intégrale de toutes les clés attestées}. Le store est
  cumulatif entre cycles (pas de reset), donc le volume de données croît à
  chaque cycle.

## Résultats

| Mesure | A — B-tree CoW (shadow paging, façon LMDB) | B — LSM-tree |
|---|---|---|
| W1 throughput | 107 296 inserts/s | **435 894 inserts/s** (4,1×) |
| W1 taille fichier / logique | 370,1 Mo / 25,9 Mo → **×14,3 amplification** | 27,2 Mo / 25,9 Mo → **×1,05** |
| W2 latence lecture | 8,7 µs/lecture | **3,52 µs/lecture** (2,5×) |
| W3 verdict | **10/10 PASS** (1 002 000 clés vérifiées cumulées) | **10/10 PASS** (1 591 000 clés vérifiées cumulées) |
| W3 taille finale du store de crash | 5 480,4 Mo | 819,98 Mio |
| W3 temps de recovery (ouverture) | non instrumenté séparément | 33 ms → 180 ms (croît avec le volume rejoué) |
| LOC prototype | 652 | 613 |

Les deux prototypes passent **10/10** le test de cohérence après crash brutal
(`taskkill /F`, pas de arrêt propre) — sur Windows, `sync_all()` comme
barrière est fiable dans les deux familles de design. Aucune donnée attestée
manquante ou altérée sur 20 cycles cumulés (2 593 000 clés vérifiées au
total).

## Lecture des chiffres

**Le LSM gagne nettement sur les trois axes mesurés**, pas seulement sur un
compromis attendu (« LSM = écriture rapide, B-tree = lecture rapide » ne
s'est pas vérifié ici — le LSM lit aussi plus vite, memtable + SSTs triés en
mémoire pour l'index des clés battent la traversée de pages B-tree sur
disque à ce volume).

**L'amplification d'espace du prototype B-tree (×14,3) n'est pas un verdict
définitif contre le CoW B-tree en général** — c'est un artefact du spike :
*aucune free-list* n'a été implémentée (cf. commentaire du fichier :
« Aucune page n'est jamais réécrite après commit (CoW pur, pas de
free-list) »), donc chaque page modifiée s'accumule en fin de fichier sans
jamais être récupérée. `redb` (référence CoW B-tree en production, cf.
recherche ci-dessous) résout ça avec une free-list de pages ; un B-tree CoW
maison correctement fini n'aurait pas ce ratio. **Mais** implémenter cette
free-list correctement (allocation/libération de pages sans use-after-free
ni fuite, sûre après crash) est exactement le genre de machinerie
supplémentaire qu'ADR-019 citait comme risque — le LSM l'évite structurel-
lement (SSTs immuables + compaction déjà nécessaire de toute façon).

**Le WAL du LSM est un flux de changement quasi gratuit.** Chaque
enregistrement WAL du prototype B est déjà `{batch_id, count, (clé,
valeur)*}` avec checksum — struturellement un log ordonné d'écritures. Pour
un futur sync P2P (VISION §5.6), consommer ce flux (au lieu du store final)
est direct. Le B-tree CoW n'a pas d'équivalent naturel : ses écritures sont
des *pages* (diffs physiques, pas des opérations logiques) — il faudrait un
mécanisme de journalisation logique séparé, donc doublement construit.

## Étude de l'écosystème (`redb`, `fjall`, `sled`) — statut 2026

Vérifié via recherche web (ne pas assumer un statut figé au cutoff
d'entraînement) :

- **`redb`** — B+tree copy-on-write pur Rust, inspiré LMDB, en production
  (~320k téléchargements/mois, 303 dépendants). Version 4.1 en 2026 (gains
  perf assistés par IA sur la stabilité, +1,5× écriture via partitionnement
  de cache dynamique, +15 % lecture concurrente). Format de fichier stable,
  chemin de mise à niveau documenté. **Actif et mûr** — même famille de
  design que le prototype A, mais sans son défaut d'amplification (free-list
  gérée).
- **`fjall`** — moteur LSM pur Rust embarquable. Fjall 3.0 (janvier 2026),
  3.1 avec compaction filters (mars 2026). Signal de mainteneur à noter :
  développement de nouvelles fonctionnalités qui ralentit nettement en
  entrant dans 2026 (posture qui bascule vers maintenance plutôt que feature
  work actif). **Maintenu, mais momentum en baisse.**
- **`sled`** — non revérifié en détail dans ce spike (statut déjà réputé
  incertain avant 2026 côté communauté Rust) ; écarté sans étude approfondie
  vu que `redb` et `fjall` couvrent déjà les deux familles de design avec un
  statut plus clair.

## Conclusion du spike

1. **LSM est la famille de design retenue pour la Couche 1**, sur la base de
   données mesurées (throughput, latence, amplification, aptitude structurelle
   au change-capture) — pas une préférence a priori.
2. **Fondation-maison, pas fork/dépendance sur `fjall`** — décision et
   justification détaillées dans `ADR-025-native-engine-storage-foundation.md`.
3. `redb` et `fjall` restent des **références de design** à consulter pendant
   l'implémentation réelle (Couche 1, N2) — leurs choix de format sur disque,
   stratégies de compaction et gestion de bloom filters sont publics et
   éprouvés en production.
