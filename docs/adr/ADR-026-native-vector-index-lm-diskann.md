# ADR-026 — Index vectoriel natif : famille DiskANN (LM-DiskANN sur KV), pas HNSW

**Statut** : ✅ Accepted
**Date** : 2026-07-04
**Relation aux ADR existants** : ouvre la Couche 2 du chantier acté par ADR-024
(§Décision 2 : « index vectoriel (HNSW/DiskANN pur Rust) » — le choix entre les
deux y était laissé ouvert). S'appuie sur la fondation LSM d'ADR-025 (l'index
est une structure logique par-dessus le store KV, jamais un second moteur de
durabilité). N'amende rien.

## Contexte

Le jalon N3 (`docs/TODO-NATIVE-ENGINE.md`) exige de trancher **HNSW vs
DiskANN** avant toute implémentation de `basemyai-engine/src/idx/vector/`.
Critère affiché : profil mémoire vs disque pour une mémoire d'agent locale.

Ce que l'on sait déjà, sans nouvelle mesure :

- **Profil de charge réel** : vecteurs 384d `f32` (`all-MiniLM-L6-v2`,
  `EMBEDDING_DIM = 384`, `crates/basemyai/src/memory/schema.rs`), soit
  1 536 octets bruts par vecteur — ~15 Mio à 10k souvenirs, ~147 Mio à 100k
  (arithmétique, pas une mesure). Volumes réalistes 10k–100k par agent ; le
  bench M6 documente 1M comme hors du cas d'usage. Insertions incrémentales
  continues (`remember`) **et suppressions réelles** : `forget` physique,
  `invalidate` soft, `purge_agent`, GC des expirés (ADR-005), oubli adaptatif —
  toutes présentes dans le contrat `MemoryStore`
  (`crates/basemyai/src/storage/mod.rs`) que le moteur natif devra implémenter.
- **La cible mesurée à battre** (`docs/benchmarks/m6-knn-results-2026-07-01.md`) :
  sur libSQL, requête KNN k=10 ≈ 48–49 ms de moyenne à 10k comme à 100k
  (sous-linéaire, vrai ANN), mais **build d'index ~78–79 ms/ligne**,
  quasi-linéaire — et la maintenance *incrémentale* du même index est
  documentée comme catastrophique (100k jamais terminé en 3h+, amplification
  disque ×65). Le coût de build/maintenance est LE point faible mesuré du
  backend actuel.
- **L'analyse comparative est déjà faite**
  (`docs/research/surrealdb-gap-analysis.md` §6, code SurrealDB
  `core/src/idx/trees/{hnsw,diskann}/` vérifié) : l'index natif libSQL derrière
  `vector_top_k` est de la famille **DiskANN (LM-DiskANN)** ; SurrealDB
  maintient les deux familles en production et leurs deux implémentations
  restent des références de lecture.
- **La fondation Couche 1 existe** (ADR-025, jalon N2) : `Engine`
  WAL+memtable+SST avec `apply_batch` **atomique** (un batch = un
  enregistrement WAL, présent en bloc ou absent — comportement réellement
  observé sous kill), `format.lock` anti-drift, harnais crash-consistency et
  fuzzing en place. Débit brut KV mesuré au spike N1 : 435 894 inserts/s — le
  chemin d'écriture KV n'est pas le goulot.
- **Littérature établie** (qualitativement, pas de chiffre inventé) : HNSW est
  un graphe multi-couches résident en RAM avec point d'entrée global, conçu
  pour l'insertion mais **pas pour la suppression** — les implémentations de
  référence (hnswlib) ne font que du *mark-delete* (tombstones filtrés à la
  recherche), la connectivité du graphe se dégrade sous churn
  insert/delete et la récupération réelle passe par un rebuild. La famille
  DiskANN (graphe Vamana plat) a précisément traité ce point :
  FreshDiskANN documente inserts + deletes en flux avec passes de
  consolidation ; **LM-DiskANN** (le variant retenu par libSQL) range chaque
  nœud dans un bloc autonome (vecteur + liste de voisins, éventuellement
  compressés), ce qui rend l'index paginable depuis le disque avec une
  empreinte RAM faible et les mises à jour **locales** (toucher un nœud =
  réécrire son bloc), sans structure globale en RAM obligatoire.

## Décision

1. **La famille retenue est DiskANN, variante LM-DiskANN** : graphe de
   proximité **plat** (un seul niveau, façon Vamana), où **un nœud = un
   enregistrement KV autonome** (vecteur + liste de voisins), stocké dans le
   store LSM de la Couche 1. Pas de HNSW.
2. **Persistance par-dessus le KV, pas de fichier sidecar.** L'index vit dans
   un keyspace dédié du store (`key/` : préfixe réservé à `idx/vector/`),
   comme structure logique (PLAN §2 Couche 2). Il réutilise la durabilité de
   la Couche 1 au lieu d'en construire une seconde. Les nouveaux types
   persistés (bloc nœud, métadonnées d'index : point d'entrée, paramètres,
   dimension, epoch) sont versionnés dans `format/` et **enregistrés dans
   `format.lock`** au même titre que `WalRecord`/`SstFile`.
3. **Crash consistency — l'invariant est double** :
   - Les mises à jour d'index voyagent dans le **même `apply_batch`** que
     l'écriture de la donnée qu'elles indexent : l'atomicité WAL déjà prouvée
     par le harnais couvre l'index gratuitement (donnée + nœuds de graphe
     touchés = un batch, présent en bloc ou absent).
   - **La donnée reste l'unique source de vérité** : l'index est déclaré
     reconstructible depuis les enregistrements mémoire. Un chemin `rebuild`
     existe dès la V1 de l'index, déclenché par mismatch d'epoch/métadonnées
     (l'échappatoire si un bug d'index est découvert — on ne perd jamais de
     souvenirs à cause de l'index).
4. **Suppressions — de premier ordre, pas un correctif** : `forget`/GC
   suppriment le nœud (delete KV du bloc) et marquent l'id dans un ensemble de
   tombstones d'index ; les voisins sont réparés **paresseusement** (à la
   visite, un voisin tombstoné est court-circuité vers ses propres voisins,
   façon FreshDiskANN) ; une passe de **consolidation** périodique réécrit les
   listes de voisins qui référencent trop de tombstones — branchée sur le
   `MaintenanceWorker` existant (mécanique injectée, ADR-008), pendant naturel
   de la compaction LSM déjà nécessaire. `invalidate` (soft) ne touche **pas**
   l'index : la validité temporelle reste un filtre au recall (ADR-005), comme
   aujourd'hui.
5. **Écriture : cache RAM + flush par batch.** La construction et la mise à
   jour du graphe se font sur un cache de blocs en RAM (read-through sur
   `Engine::get`), les blocs modifiés étant écrits par `apply_batch`. C'est
   l'attaque directe du point faible mesuré de libSQL : les ~78–79 ms/ligne
   viennent du maintien du graphe *à travers* la couche SQL/pages B-tree ;
   notre chemin supprime cet étage (le KV brut encaisse 435k inserts/s, spike
   N1). **Aucun chiffre de gain n'est promis ici** — les seuils sont posés
   avant mesure, point 6.
6. **Barre de sortie N3, fixée avant toute mesure** (façon protocole N1) :
   mêmes scénarios que le bench M6 (10k/100k, k=10, cosine, 384d), chiffres
   archivés sous `docs/benchmarks/` :
   - **Requête** : latence moyenne ≤ la parité libSQL (≈ 48–49 ms mesurés) —
     c'est un plafond, pas l'ambition.
   - **Build** : coût par ligne **strictement inférieur** aux ~78–79 ms/ligne
     de libSQL, insertion *incrémentale* comprise (pas seulement bulk-load) —
     c'est le critère qui a motivé tout le jalon.
   - **Qualité** : recall@10 ≥ 0,9 contre le brute-force exact sur les mêmes
     données, y compris **après** un churn insert/delete (le scénario où HNSW
     décroche) — mesuré, pas supposé.
   Si l'un des trois seuils n'est pas atteint, l'implémentation ne se coche
   pas (discipline TODO : critère de sortie vérifié, pas « le code existe »).

### Pourquoi pas HNSW — les trois raisons décisives

1. **Les suppressions sont réelles chez nous.** `forget`, `purge_agent`, GC
   des expirés et oubli adaptatif font partie du contrat `MemoryStore` — pas
   un cas rare. HNSW n'a pas de réponse de référence au delete autre que
   tombstone + rebuild ; la famille DiskANN a une réponse documentée
   (FreshDiskANN) qui s'aligne naturellement sur la mécanique
   tombstone/compaction que le store LSM possède déjà.
2. **Profil mémoire local-first.** Le produit tourne à côté d'un LLM local qui
   consomme la RAM (ADR-013/016). LM-DiskANN pagine depuis le disque avec une
   empreinte RAM faible ; HNSW exige le graphe résident. À 100k × 384d les
   ~147 Mio + voisinage resteraient *tenables* en RAM — l'argument n'est pas
   « impossible », il est « le budget RAM appartient au LLM, pas à l'index »,
   et il devient structurant si les volumes ou la dimension montent (V2
   multi-modèles).
3. **Persistance et parité.** Un nœud-un-bloc se mappe **exactement** sur
   un nœud-une-entrée-KV : mises à jour locales, couvertes par `apply_batch`,
   versionnées par `format.lock`. HNSW (multi-couches, point d'entrée global)
   se persiste incrémentalement beaucoup moins naturellement — l'alternative
   classique « HNSW en RAM + rebuild au boot » imposerait des minutes de
   reconstruction à chaque ouverture d'un `.bmai` de 100k souvenirs,
   inacceptable pour une mémoire d'agent ouverte à la session. Enfin, la
   parité M6 se juge contre un LM-DiskANN (libSQL) : même famille = comparaison
   à armes égales, et l'implémentation DiskANN de SurrealDB reste disponible
   comme référence de lecture (gap-analysis §6).

## Conséquences

✅ N3 peut démarrer sans décision bloquante restante ; le layout
`idx/vector/` (PLAN §3.1) se remplit avec un design précis : bloc nœud
versionné, cache RAM, recherche greedy + robust-prune, tombstones +
consolidation.
✅ L'index hérite gratuitement de la discipline Couche 1 : atomicité
`apply_batch` prouvée par le harnais, `format.lock`, fuzzing (le décodage du
bloc nœud devient une cible de fuzz au même titre que `sst_decode`).
✅ La suppression physique devient un citoyen de première classe de l'index —
ce que le backend libSQL ne nous laisse pas contrôler finement aujourd'hui.
✅ Les seuils de sortie sont posés avant la mesure — pas de chiffre inventé,
pas de « vibe bench ».
⚠️ Vamana/robust-prune + réparation paresseuse des deletes est plus subtil
qu'un HNSW en RAM ; le risque reste « modéré » (PLAN §2) parce que
l'algorithme est bien documenté et que deux implémentations de référence
lisibles existent (libSQL, SurrealDB `diskann/`), mais la passe de
consolidation devra être testée sous churn, pas seulement en insertion pure.
⚠️ Le recall après churn est le critère le plus susceptible d'échouer en
premier — c'est voulu : mieux vaut le voir échouer sur un seuil posé d'avance
que le découvrir en production.
⚠️ La compression des vecteurs voisins (le « LM » complet de LM-DiskANN) n'est
**pas** exigée pour N3 : à 10k–100k × 384d les blocs restent petits ; elle
reste une optimisation V2, cohérente avec le gap déjà noté « types
compressés → V2, ADR requis » (gap-analysis §6).

## Alternatives rejetées

**HNSW en RAM (reconstruit au boot ou sérialisé en bloc).** Le plus simple à
écrire et le plus rapide à construire *en RAM* — mais suppressions par
tombstone sans réparation de référence, graphe qui se dégrade sous le churn
réel d'une mémoire d'agent, RAM prise au détriment du LLM local, et
persistance incrémentale non naturelle (rebuild de plusieurs minutes au boot à
100k, ou sérialisation monolithique hors du modèle `apply_batch`). Chacun de
ces points est contournable isolément ; leur somme reconstruit de fait un
DiskANN — autant le prendre directement.

**HNSW persistant façon SurrealDB (compaction async du pending, cache
vecteurs).** Existe et fonctionne chez eux, preuve que c'est faisable — mais
c'est la voie la plus complexe des deux chez SurrealDB précisément parce que
HNSW n'est pas né disque-natif, et elle ne résout pas la dégradation sous
delete. Choisir la famille née disque-natif coûte moins que domestiquer
l'autre.

**Brute-force exact (pas d'index) pour 10k–100k.** Honnêtement envisageable à
10k (le scan de 15 Mio est trivial) et utile comme *oracle de recall* dans les
tests — mais à 100k il ne tiendrait la parité de latence qu'en y consacrant de
la RAM et du SIMD, et il plafonne par construction : aucune marge pour V2
(multi-modèles, dimensions supérieures, volumes qui montent). Conservé
uniquement comme référence de mesure du recall, jamais comme chemin de
production.

**Nouveau spike prototypes HNSW vs DiskANN avant de trancher.** Rejeté : le
critère du spike N1 (« un signal net et cohérent rend la prolongation de
l'incertitude coûteuse sans gain d'information ») s'applique ici en amont —
les suppressions réelles, le profil RAM local-first, le mapping bloc↔KV et la
parité à juger contre un LM-DiskANN pointent tous dans la même direction.
L'incertitude résiduelle (le recall sous churn, le coût de build réel) n'est
pas résoluble par un spike jetable de plus : elle est précisément ce que les
seuils de sortie N3 (§Décision 6) mesureront sur l'implémentation réelle.
