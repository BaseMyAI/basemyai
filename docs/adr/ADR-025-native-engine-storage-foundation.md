# ADR-025 — Fondation Couche 1 du moteur natif : LSM-tree maison

**Statut** : ✅ Accepted
**Date** : 2026-07-04
**Relation aux ADR existants** : ferme le spike ouvert par ADR-024 (§« Alternatives
rejetées » y laissait explicitement non tranchée la question « forker un moteur
KV Rust existant comme fondation définitive »). N'amende aucune décision
d'ADR-024, la complète.

## Contexte

`PLAN-NATIVE-ENGINE.md` (Phase 0b) et `TODO-NATIVE-ENGINE.md` (jalon N1)
prescrivaient un spike avant d'écrire la moindre ligne de la Couche 1
(stockage durable) : comparer deux prototypes jetables (B-tree copy-on-write
façon LMDB, LSM-tree façon RocksDB) sur le même protocole, et étudier le
statut 2026 des moteurs KV purs Rust existants (`redb`, `fjall`, `sled`) pour
trancher fondation-maison vs dépendance consciente.

Résultats complets : `docs/benchmarks/n1-storage-engine-spike-2026-07-04.md`.
Résumé :

- **LSM bat le B-tree CoW sur les trois axes mesurés** : 4,1× le débit
  d'écriture (435 894 vs 107 296 inserts/s), 2,5× la latence de lecture
  (3,52 µs vs 8,7 µs), et une amplification d'espace de ×1,05 contre ×14,3
  (ce dernier chiffre partiellement un artefact de spike — absence de
  free-list dans le prototype A — mais implémenter une free-list sûre après
  crash est elle-même de la machinerie supplémentaire que le LSM évite par
  construction, via ses SSTs immuables et sa compaction déjà nécessaire).
- **Les deux prototypes passent 10/10 le test de cohérence après crash**
  brutal (`taskkill /F` en boucle, 20 cycles cumulés, 2 593 000 clés
  vérifiées, zéro perte/altération) — `sync_all()` comme barrière de
  durabilité est fiable sous Windows dans les deux familles.
- **Le WAL du LSM est structurellement un flux de changement** (enregis-
  trements ordonnés `{batch_id, count, (clé, valeur)*}` avec checksum) —
  directement exploitable pour le sync P2P futur (VISION §5.6). Le B-tree
  CoW n'a pas d'équivalent naturel : ses écritures sont des diffs de pages
  physiques, pas des opérations logiques.
- **`redb`** (B+tree CoW) est mûr et activement maintenu en 2026 (v4.1,
  ~320k téléchargements/mois, 303 dépendants, format de fichier stable).
  **`fjall`** (LSM) est maintenu mais son mainteneur signale un net
  ralentissement du développement de nouvelles fonctionnalités en entrant
  dans 2026 (bascule vers une posture de maintenance).

## Décision

1. **La famille de design retenue pour la Couche 1 est le LSM-tree**, pas le
   B-tree copy-on-write. Choix fondé sur les chiffres du spike, pas une
   préférence a priori — le protocole était strictement identique pour les
   deux prototypes.
2. **La fondation est maison** (implémentation propre dans
   `crates/basemyai-engine`), **pas un fork ni une dépendance consciente sur
   `fjall`**, alors même que le critère d'ADR-024 le permettait explicitement
   (« propriété des couches 2-4 garantie dans les deux cas »). Raisons :
   - La Couche 1 est explicitement désignée par `PLAN-NATIVE-ENGINE.md` comme
     *« la fondation, le risque le plus élevé, le seul qui peut tuer le
     projet en silence »* — et aussi la couche la plus différenciante pour
     les capacités futures (change-capture natif pour le sync P2P). C'est
     précisément la couche où la motivation de fond d'ADR-024 (« posséder la
     technologie de bout en bout », pari long terme explicitement assumé par
     le propriétaire du projet) pèse le plus lourd — en dépendre externement
     ici annulerait l'essentiel du bénéfice recherché par tout le chantier.
   - Dépendre de `fjall` reproduirait, à une échelle différente, exactement
     le risque qu'ADR-024 quitte : la roadmap et la maintenance de la couche
     la plus critique du produit resteraient entre les mains d'un tiers. Le
     signal de ralentissement du développement `fjall` en 2026 rend ce risque
     concret, pas hypothétique.
   - Le spike a démontré, à l'échelle d'un prototype, que le mécanisme
     central (WAL + memtable + SST + compaction + recovery + crash-
     consistency) est déjà compris et fonctionnel (652/613 lignes, 10/10 PASS
     crash test) — la marche vers une implémentation de production est un
     travail de durcissement (fuzzing, `format.lock`, concurrence,
     compaction plus fine, bloom filters), pas une découverte d'architecture.
3. **`redb` et `fjall` restent des références de design actives**, à consulter
   pendant l'implémentation réelle (N2) pour leurs choix de format sur disque,
   stratégies de compaction et structures d'index — au même titre que
   SurrealDB l'est déjà pour l'organisation du repo. Aucune de leurs
   dépendances Cargo n'entre dans `basemyai-engine`.
4. Le prototype B (LSM) devient la référence architecturale de départ pour
   N2 — pas le code à réutiliser tel quel (spike : `unwrap()` partout,
   mono-thread, pas de bloom filter, compaction naïve à seuil fixe), mais le
   design (WAL puis memtable, flush ordonné SST-fsync-rename-avant-truncate-
   WAL, compaction par merge) à durcir.

## Conséquences

✅ Décision de fondation prise sur données mesurées, pas sur intuition —
cohérent avec la discipline anti-« vibe code » demandée pour ce chantier.
✅ Ferme la dernière alternative laissée ouverte par ADR-024 ; N2 peut
démarrer sans decision bloquante restante.
✅ Le change-capture (WAL déjà un flux logique ordonné) dérisque en amont le
futur chantier sync P2P (N6), conformément à la thèse d'ADR-024 selon
laquelle un moteur maison rend le sync plus naturel, pas plus dur.
⚠️ Aucun bénéfice du travail déjà fait par `redb`/`fjall` sur la dureté
(fuzzing accumulé, bugs déjà trouvés et corrigés en production, années
d'usage réel) n'est hérité — toute la discipline de test définie par ADR-024
§4 (harnais crash-consistency en CI dès le premier commit, fuzzing,
`format.lock`) doit être payée intégralement par ce projet, sans raccourci.
⚠️ Le prototype de spike n'est pas production-ready : compaction naïve
(merge total au-delà de 4 SSTs, dernier écrivain gagne, aucun bloom filter,
aucune limite de taille de SST, mono-thread) — N2 doit concevoir une
stratégie de compaction par niveaux/tiered avant tout chiffre de parité M6.

## Alternatives rejetées

**B-tree copy-on-write (façon `redb`/LMDB) comme fondation.** Mesurablement
plus lent en écriture et lecture sur ce protocole, et sans avantage structurel
pour le change-capture. Resterait un choix défendable si la charge cible était
dominée par des lectures aléatoires massives avec peu d'écritures — ce n'est
pas le profil d'une mémoire d'agent (écritures fréquentes : `remember` à
chaque interaction, lectures en rafale au `recall`).

**Forker/dépendre de `fjall` (ou son sous-crate `lsm-tree`) comme Couche 1.**
Aurait dérisqué des mois de durcissement (le code est déjà testé en
production). Rejeté car la Couche 1 est précisément la couche où la
motivation de fond d'ADR-024 — l'appropriation complète de la pile — a le
plus de valeur, et parce que le ralentissement de maintenance signalé par le
mainteneur `fjall` reproduirait le risque de dépendance-tierce qu'ADR-024
cherche à quitter.

**Ne pas trancher maintenant, garder les deux prototypes en compétition
jusqu'à N2.** Rejeté : le spike a produit un signal net et cohérent sur les
trois axes mesurés (pas un résultat mitigé qui justifierait de prolonger
l'incertitude) ; prolonger le spike aurait été un coût sans gain d'information
supplémentaire attendu.
