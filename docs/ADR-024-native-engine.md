# ADR-024 — Moteur natif BaseMyAI (remplace le chemin Turso DB)

**Statut** : ✅ Accepted
**Date** : 2026-07-02
**Relation aux ADR existants** : amende ADR-011 (le « chemin de migration vers
Turso DB » est abandonné ; tout le reste — libSQL V1, vecteur natif, async —
tient) et ADR-019 (le « native `.bmai` backend explicitly out of scope for
V1 » reste vrai *pour V1* ; cette décision ouvre le chantier comme pari
long terme parallèle, pas comme remplacement V1). Ni l'un ni l'autre ne sont
supersedés dans leurs autres décisions.

## Contexte

ADR-011 (juin 2026) a choisi libSQL comme backend V1 et noté « chemin de
migration vers Turso DB (pur Rust, zéro C) quand il passera production » comme
sortie future du fork C. ADR-019 (18 juin 2026) a rejeté « implement a native
`.bmai` backend now » avec une raison précise : crash recovery, compaction,
index vectoriel, chiffrement et machinerie de migration seraient à construire
avant que le produit mémoire soit prouvé.

Depuis, deux choses ont changé :

1. **Le produit est prouvé plus vite que prévu.** Phase 1 + Phase 2
   implémentées et testées, surfaces MCP/REST/bindings/CLI livrées, crates.io
   et PyPI publiés (0.1.0, 2026-06-22), hardening M6 largement fait (pool
   lecteur, key rotation, bench KNN, stress Candle). La condition « before the
   memory product is proven » d'ADR-019 est en voie d'être remplie.
2. **Décision stratégique du propriétaire du projet** (2026-07-02) : posséder
   la technologie de bout en bout — stockage, index vectoriel, graphe, et à
   terme un langage de requête — plutôt que de dépendre du moteur d'un tiers
   (libSQL aujourd'hui, Turso demain). Motivation explicite : pari long terme
   multi-années ; le temps n'est pas la contrainte, la maîtrise complète de la
   pile est l'objectif. Adopter Turso reviendrait à échanger une dépendance
   (fork C maintenu par un tiers) contre une autre (réécriture Rust maintenue
   par le même tiers) sans gagner la propriété de la couche la plus
   différenciante à long terme.

## Décision

1. **Le chemin « migration Turso DB » d'ADR-011 est abandonné.** BaseMyAI ne
   migrera pas vers le moteur de Turso ; il construira le sien.
2. **Un moteur de stockage natif BaseMyAI est mis en chantier**, comme pari
   long terme, dans un nouveau crate interne `crates/basemyai-engine`,
   organisé en couches à risque décroissant : (1) store durable
   (pages/WAL/transactions/crash recovery), (2) index vectoriel
   (HNSW/DiskANN pur Rust), (3) graphe natif, (4) langage de requête —
   cette dernière conditionnée à une décision produit séparée (surface
   agent vs outil interne).
3. **libSQL reste le backend par défaut** pendant toute la durée du chantier
   (« strangler fig »). Le moteur natif vit derrière `EngineKind::Native` et
   une feature Cargo `engine-native`, jamais activée par défaut. La bascule du
   défaut exigera un nouvel ADR, appuyé sur des chiffres de parité.
4. **La barre d'entrée est définie avant la première ligne du store** :
   harnais de cohérence après crash (kill -9 sous charge) en CI dès le premier
   commit du moteur ; fuzzing des surfaces de format (clés, WAL, pages) ;
   fichier `format.lock` (équivalent du `revision.lock` de SurrealDB) figeant
   la version de sérialisation de chaque type persisté, validé en CI ; runner
   de tests déclaratifs rejouant les mêmes scénarios mémoire sur libSQL et
   natif. Le moteur natif est jugé sur les suites de contrat existantes
   (`storage_contract.rs`, `contracts.rs`) plus la barre de hardening M6 que
   libSQL a passée (pool, bench KNN, stress, key rotation).
5. **Le format public `.bmai` ne change pas d'identité** (ADR-019) : le moteur
   interne est un détail d'implémentation derrière le contrat `StorageEngine`
   et les métadonnées de conteneur. `format_version`/`storage_engine` dans
   `bmai_meta` porteront la distinction.
6. **Les invariants d'écosystème tiennent intégralement** : `basemyai-core`
   reste agnostique métier (le moteur n'introduit aucun `agent_id`/`Symbol`
   dans le core), mono-fichier chiffré, zéro réseau par défaut, mécanisme au
   core / sens au consommateur.

Plan détaillé : `docs/PLAN-NATIVE-ENGINE.md`. Backlog :
`docs/TODO-NATIVE-ENGINE.md`.

## Conséquences

✅ Propriété complète de la pile de stockage à terme : plus de fork C, plus de
dépendance à la roadmap d'un tiers pour vecteur/chiffrement/WASM/sync.
✅ Le change-capture peut être une primitive de premier ordre du WAL dès la
conception — le sync P2P (VISION §5.6) devient un consommateur naturel au lieu
d'un bricolage par-dessus un moteur emprunté.
✅ Le produit ne prend aucun risque : libSQL reste le défaut, le chantier est
additif et jugé sur des suites de tests qui existent déjà.
✅ Les cinq risques d'ADR-019 (crash recovery, compaction, index, chiffrement,
migration) deviennent des jalons séquencés avec critères de sortie mesurables,
au lieu d'être des raisons de ne jamais commencer.
⚠️ Coût considérable et assumé : un moteur maison n'hérite d'aucune décennie de
durcissement SQLite ; la discipline de test (crash harness, fuzzing,
`format.lock`) doit être payée en premier, pas en dernier.
⚠️ Risque de dispersion : le chantier ne doit jamais geler le travail produit
sur libSQL. Les phases du plan sont conçues pour être interruptibles.
⚠️ Le chiffrement au repos (obligatoire dans `basemyai`, ADR-007) devra être
réimplémenté nativement — chantier cryptographique sérieux, pas un détail.

## Alternatives rejetées

**Adopter Turso DB (le chemin ADR-011).** Échange une dépendance tierce contre
une autre ; ne donne pas la propriété de la pile ; l'API et la couverture
fonctionnelle (FTS5, CTE récursives, chiffrement) du moteur Turso restaient à
vérifier par spike de toute façon. Le bénéfice « pur Rust » est réel mais
s'obtient aussi — avec la propriété en plus — par le moteur natif.

**Rester sur libSQL indéfiniment.** Option par défaut la plus sûre à court
terme, et elle reste la réalité du produit pendant tout le chantier. Rejetée
comme *fin de partie* uniquement : fork C, pas de propriété, dépendance à la
roadmap d'un tiers pour toute capacité future (WASM, réplication P2P,
quantization des vecteurs).

**Réécriture big-bang (arrêter le produit, tout refaire).** La façon la plus
sûre d'échouer — exactement ce qu'ADR-019 a refusé. Le strangler fig avec
parité prouvée par tests est non négociable.

**Forker un moteur KV Rust existant (`redb`/`fjall`) comme fondation
définitive.** Ni adopté ni rejeté ici : l'étude de ces moteurs comme référence
de design (voire comme dépendance consciente de la seule Couche 1) fait partie
du spike Phase 0b. La décision fondation-maison vs fondation-forkée sera actée
à l'issue du spike.
