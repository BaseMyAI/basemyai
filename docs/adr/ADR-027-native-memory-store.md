# ADR-027 — `MemoryStore` sur le moteur natif : mapping, atomicité et découpage N5

**Statut** : ✅ Accepted
**Date** : 2026-07-05
**Relation aux ADR existants** : ouvre la Phase 4 (parité complète) du chantier
acté par ADR-024. S'appuie sur la fondation LSM d'ADR-025 (N2), l'index
vectoriel LM-DiskANN d'ADR-026 (N3) et le graphe natif (N4). Implémente sur
`Native` le contrat `MemoryStore` défini par ADR-020, en préservant les
sémantiques ADR-005 (RAG temporel), ADR-006 (isolation par agent) et ADR-012
(oversampling ×8 quand un filtre est présent). N'amende rien.

## Contexte

N0→N4 sont clos : le moteur natif (`crates/basemyai-engine`) a un store KV
durable crash-testé, un index vectoriel persistant (recall@10 = 1.0, parité
bench M6 tenue avec marge) et un graphe persistant (portage littéral de la CTE
récursive). Le jalon N5 (« parité complète ») est le point où tout se
rassemble : brancher le trait `MemoryStore` (`crates/basemyai/src/storage/mod.rs`)
sur ces primitives, ce qui débloque enfin le **diff multi-backend** du runner
déclaratif (`crates/basemyai/tests/memory_tests/`), posé au N2 précisément
pour ce moment.

Trois écarts structurels entre libSQL et le moteur natif forcent des décisions :

1. **libSQL est async multi-connexions ; `Engine` est sync mono-écrivain.**
2. **libSQL adresse les souvenirs par colonnes SQL (`id` TEXT, `agent_id`) ;
   l'index vectoriel natif adresse des `u64`.** Il faut un mapping.
3. **libSQL rend l'insert `memory` + miroir FTS atomique par transaction ;
   côté natif, l'insert vectoriel construit déjà son propre `apply_batch`
   interne** (nœud + voisins re-prunés + méta, ADR-026 §3). Sans couture, un
   `remember` natif serait deux écritures séparées avec une fenêtre de crash
   entre les deux.

## Décision

### 1. Découpage N5 (sous-jalons, dans l'ordre)

- **N5.1 — `NativeMemoryStore` hors FTS/crypto** : impl `MemoryStore` complète
  sauf `keyword_ranking_ids`, `backend_suite!(native)` vert. C'est le présent ADR.
- **N5.2 — FTS/BM25 natif** : index inversé + scoring BM25, parité
  `recall_hybrid`. `keyword_ranking_ids` retourne d'ici là une **erreur
  franche** (« FTS natif non implémenté, N5.2 ») — jamais un faux vide qui
  ferait passer un RRF dégradé pour un résultat correct.
- **N5.3 — 100 % `storage_contract.rs` + `contracts.rs` verts sur Native**
  (portage des scénarios restants dans le runner déclaratif).
- **N5.4 — Chiffrement au repos natif + rotation de clé** (équivalent
  ADR-007) — chantier crypto séparé, ne bloque pas N5.1-N5.3.
- **N5.5 — Barre hardening M6** : modèle de concurrence (le mono-écrivain
  sérialisé de N5.1 est assumé jusqu'ici), bench KNN via le chemin
  `MemoryStore` complet, stress long, harnais crash étendu au mode `memory`.
- **N5.6 — ADR de bascule du défaut libSQL→Native** : décision **séparée et
  humaine**, chiffres à l'appui, jamais prise en passant.

### 2. Frontière moteur/consommateur : `idx/memory` dans `basemyai-engine`

Le moteur gagne un troisième index logique, `idx/memory/`, sur le modèle exact
de `idx/vector` (N3) et `idx/graph` (N4) :

- **Formats versionnés dans `format.lock`** : `MemoryRecord:1` (bloc souvenir :
  tag de couche opaque, contenu, `valid_from`/`valid_until`, source,
  importance, `last_access`, `vec_id`), `MemoryVecMap:1` (résolution inverse
  `u64` → `(agent, id)`), `MemoryIndexMeta:1` (compteur d'allocation
  `next_vec_id`). Même discipline de codec que `GraphEntity` (magic, version,
  longueurs bornées contre la taille réelle du buffer avant toute allocation —
  leçon fuzzing N2/N3 —, crc32 traînant).
- **Layout de clés** (`key::memory_index`, préfixe réservé `idx/memory/`) :
  `idx/memory/rec/<agent_len u32 BE><agent><id>` (préfixe-longueur sur
  `agent`, même justification anti-collision que N4),
  `idx/memory/vecmap/<vec_id u64 BE>`, `idx/memory/meta`. L'isolation par
  agent est **structurelle** (préfixe de scan), jamais un filtre applicatif.
- **`PersistentMemoryIndex`** possède toute la composition crash-critique
  (put/get/forget/scan/résolution/purge mémoire + touch `last_access`) — pour
  que le harnais crash-consistency puisse un jour la tester **côté moteur**,
  comme les modes `vector` et `graph` existants (c'est l'item N5.5).

La **politique** reste côté `basemyai` (`NativeMemoryStore`) : fenêtres de
validité, filtre de couche, oversampling, hydratation, RRF. Mécanisme au
moteur, sens au consommateur — même règle qu'ADR-001, un niveau plus bas.

### 3. Atomicité : les écritures du consommateur montent dans le batch de l'index

Plutôt que de tolérer des orphelins entre l'insert vectoriel et le bloc
souvenir, l'API de l'index vectoriel gagne deux variantes :
`PersistentVectorIndex::insert_with(engine, id, vector, extra)` et
`delete_with(engine, id, extra)`, où `extra` est un `Batch` du consommateur
**fusionné dans le même `apply_batch`** que les blocs de l'index
(`Batch::extend_from`). Un `remember` natif = **un seul enregistrement WAL** :
compteur + vecmap + bloc souvenir + nœud vectoriel + voisins re-prunés + méta,
présent en bloc ou absent en bloc après un crash — la même garantie que la
transaction libSQL qu'il remplace. `forget` = idem (tombstone + suppression
bloc + vecmap). `delete_with` applique `extra` même si le nœud vectoriel est
absent ou déjà tombstoné : les enregistrements compagnons du consommateur ne
doivent pas survivre à un tombstone no-op.

### 4. Mapping des ids : compteur monotone persistant, jamais de réutilisation

Chaque souvenir reçoit un `vec_id: u64` alloué par un compteur **monotone**
persisté (`MemoryIndexMeta`), incrémenté dans le même batch atomique que
l'insert (décision 3). Alternatives rejetées :

- *Hash de `(agent, id)`* : collision improbable mais alors **déterministe et
  permanente** (un `remember` qui ne peut jamais réussir) ;
- *max(ids existants) + 1 recalculé à l'ouverture* : réutilisation possible
  d'un id après purge physique, fenêtre de `DuplicateVectorId` fantôme.

Si le méta-compteur est absent ou corrompu, l'ouverture **guérit depuis la
donnée** (max des clés nœud ∪ vecmap + 1) — possible sans danger uniquement
parce que la décision 3 garantit que compteur et nœuds avancent dans le même
batch. Un id sauté (batch jamais commité) est bénin.

### 5. Pont sync↔async : mono-écrivain sérialisé assumé (jusqu'à N5.5)

`NativeMemoryStore` enveloppe `Engine` + index dans un
`Arc<std::sync::Mutex<Inner>>` ; chaque méthode du trait s'exécute via
`tokio::task::spawn_blocking`, le verrou pris **à l'intérieur** de la closure
bloquante — jamais tenu à travers un `.await` (lint `await_holding_lock`).
C'est le modèle de concurrence le plus simple qui soit correct ; le pool
lecteur façon ADR-021 est explicitement le chantier N5.5, pas une promesse
implicite de N5.1.

### 6. Sémantiques de parité — et les écarts assumés

Parité comportementale avec `LibsqlMemoryStore`, requête par requête :
oversampling ×8 puis post-filtre agent/temporel/couche (ADR-012) ; `hydrate`
**sans** filtre de validité (l'original n'en a pas) ; `graph_upsert_edge` qui
préserve `valid_from` existant et ne met à jour que `weight` (le
`ON CONFLICT ... DO UPDATE SET weight` original) ; `exact_fact_exists` sans
filtre de validité ; scores = distance cosinus (l'index natif expose
`search_scored`, distances déjà calculées par le beam search). Écarts assumés,
documentés dans le code :

- **`put_memory_batch`** : atomique **par item** (un batch WAL par souvenir),
  pas tout-ou-rien comme la transaction libSQL — composer N plans d'insert
  Vamana dans un seul batch exigerait de threader l'état du planificateur
  entre les inserts ; différé, avec trace, à un item de suivi.
- **`purge_agent`** : par item + batch final graphe, **idempotent et
  reprennable** (chaque souvenir intégralement purgé ou intégralement
  présent), pas globalement atomique. Un crash au milieu se répare en relançant.
- **`put_memory` sur id existant** : erreur franche (l'original lève une
  violation de contrainte UNIQUE) — jamais d'écrasement silencieux, qui
  laisserait un nœud vectoriel vivant non référencé polluer les recherches.

## Conséquences

- Le diff multi-backend promis au N2 devient réel : `backend_suite!` rejoue
  les mêmes scénarios contre `Libsql` et `Native`, zéro divergence tolérée.
- `EngineCapabilities::native()` peut enfin rapporter `vectors: true` et
  `recursive_queries: true` honnêtement (N3/N4 les fournissent, N5.1 les
  câble) ; `full_text`/`encrypted` restent `false` jusqu'à N5.2/N5.4.
- Trois nouveaux formats dans `format.lock` — tout drift de wire casse la CI.
- Le mono-écrivain sérialisé plafonne la concurrence de lecture jusqu'à N5.5 ;
  c'est mesuré (pas caché) au moment du bench N5.5.
- La bascule du défaut reste un non-sujet tant que N5.6 n'est pas instruit.
