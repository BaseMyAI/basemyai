# ADR-028 — Index full-text natif : BM25 sur index inversé maison

**Statut** : ✅ Accepted
**Date** : 2026-07-06
**Relation aux ADR existants** : clôt le sous-jalon N5.2 ouvert par ADR-027 §1.
S'appuie sur la fondation KV d'ADR-025 (N2), suit le layout `idx/<domaine>/`
d'ADR-026 (N3, vecteur) et ADR-027 (N5.1, mémoire). Cible la parité
comportementale avec ADR-014 (BM25 via FTS5, libSQL) pour le sous-ensemble de
requêtes que `Memory::recall_hybrid` produit réellement. N'amende rien.

## Contexte

`NativeMemoryStore::keyword_ranking_ids` (ADR-027 §6) retourne aujourd'hui une
erreur franche : le second signal de `recall_hybrid` (BM25) n'existe pas côté
natif. `EngineCapabilities::native().full_text` est honnêtement `false`.

Le côté libSQL n'expose pas toute la syntaxe FTS5 à l'appelant : la façade
`Memory` construit elle-même le `match_expr` via `fts_match_expr()`
(`crates/basemyai/src/memory/mod.rs`) — tokenisation sur non-alphanumérique,
minuscule, troncature à 32 tokens, chaque token cité littéralement et joint
par ` OR `. La fonction produit *exactement* `"token1" OR "token2" OR ...`,
jamais de `NEAR`, de filtre de colonne, de `AND`/`NOT`, ni de wildcard `*`.
Reconstruire tout FTS5 serait donc du travail jeté : personne n'appelle jamais
la partie non utilisée.

Deux écarts structurels avec FTS5 :

1. **FTS5 tokenise les deux côtés (contenu ET requête) avec le même
   tokenizer** (`porter unicode61 remove_diacritics 2`, ADR-014) —
   racinisation anglaise + pliage des accents, appliqués silencieusement par
   SQLite. `fts_match_expr()` ne fait que minuscule + citation ; c'est le
   tokenizer FTS5 qui fait le reste des deux côtés au moment du `MATCH`.
2. **libSQL rend `INSERT INTO memory` + `INSERT INTO memory_fts` atomiques
   par transaction.** Côté natif, l'insert vectoriel + mémoire compose déjà
   son propre `apply_batch` (ADR-027 §3) ; sans couture, l'entrée FTS serait
   une troisième écriture avec une fenêtre de crash.

## Décision

### 1. Périmètre : le sous-ensemble réellement produit, pas FTS5

L'analyseur de requête natif accepte **exactly** la forme produite par
`fts_match_expr()` : une séquence de tokens cités entre guillemets doubles,
joints par ` OR ` littéral, rien d'autre. Toute autre forme (opérateurs
FTS5, colonnes, `NEAR`, wildcard) est une **erreur franche** de parsing —
jamais une tentative d'interprétation partielle. Ce choix n'est pas une
simplification hasardeuse : c'est la frontière exacte du contrat observé
(mécanisme au moteur, sens à l'appelant — même règle qu'ADR-001, un niveau
plus bas). Si `fts_match_expr()` change de forme un jour, ce parseur cassera
bruyamment plutôt que de silencieusement ignorer une partie de la requête.

### 2. Tokenizer : casefold + pliage d'accents, **stemming Porter différé**

Le tokenizer natif (`idx::fts::tokenizer`) réplique `fts_match_expr()` +
la partie *practicable sans nouvelle dépendance* d'`unicode61
remove_diacritics 2` : découpe sur non-alphanumérique (`char::is_alphanumeric`,
identique à `fts_match_expr`), minuscule Unicode (`char::to_lowercase`), puis
**pliage d'accents par table figée** (Latin-1 Supplement + Latin Extended-A
courants : `é/è/ê/ë→e`, `à/â→a`, `ç→c`, `ù/û→u`, `î/ï→i`, `ô→o`, `ñ→n`, …) —
zéro dépendance nouvelle (workspace Cargo.toml n'a today aucun crate de
normalisation Unicode ; une table figée est ~60 lignes et couvre le cas
français/latin courant, cohérent avec `remove_diacritics 2` qui ne couvre lui
non plus que les diacritiques, pas une translittération complète).

**Racinisation Porter (anglais) explicitement hors périmètre de N5.2** —
gap de parité assumé et documenté, pas silencieux :

- Le Porter stemmer classique (Porter, 1980) est un algorithme à 5 étapes
  avec de nombreuses listes de suffixes cas par cas — une réimplémentation
  correcte est un chantier significatif en soi, disproportionné par rapport
  à l'item de TODO scopé (« index inversé + scoring BM25 »).
- `fts_match_expr()` lui-même ne racinise pas côté appelant — le stemming
  n'a jamais été une garantie que le code appelant construit activement,
  seulement un bénéfice que FTS5 ajoutait de manière transparente.
- Conséquence mesurable et acceptée : une requête `"chats"` ne remonte pas un
  souvenir ne contenant que `"chat"` sur le backend natif (alors que libSQL
  le ferait via le stemming FTS5). RRF (`recall_hybrid`) amortit
  partiellement l'écart via le signal vectoriel. Suivi : item de TODO
  séparé, non bloquant pour N5.2 (voir `docs/TODO-NATIVE-ENGINE.md`).

Alternative rejetée : tirer un crate de stemming (`rust-stemmers` ou
équivalent) — violerait le zéro-dépendance-nouvelle déjà tenu pour
Candle/LM-DiskANN/LSM maison ; un stemmer correct mérite sa propre décision
scopée, pas un ajout de passage dans un ADR de scoring BM25.

### 3. Frontière moteur/consommateur : `idx/fts` dans `basemyai-engine`

Troisième — quatrième avec la mémoire — index logique, sur le modèle exact
de `idx/graph` et `idx/memory` :

- **Index inversé (`postings`)** : `idx/fts/postings/<agent_len u32 BE><agent><term_len u32 BE><term><vec_id u64 BE>`
  → valeur `FtsPosting:1` (magic+version+`tf: u32`+crc32, même discipline
  minimale que `MemoryIndexMeta`). Un préfixe `idx/fts/postings/<agent><term>`
  scanne tous les documents contenant `term` pour cet agent — c'est aussi la
  lecture qui sert de `df(term)` (nombre d'entrées du scan), jamais un
  compteur caché séparé à maintenir en synchronisation (une leçon de N5.1 :
  moins d'état caché = moins de dérive possible).
- **Index direct (`docterms`)** : `idx/fts/docterms/<agent_len u32 BE><agent><vec_id u64 BE>`
  → valeur `FtsDocTerms:1` (magic+version+count borné+répétition
  `(term_len u16, term, tf u32)`+crc32). Nécessaire pour un `forget` précis
  (savoir quelles entrées `postings` supprimer sans state externe) et pour
  la longueur du document (`Σ tf`), sans dépendre d'`idx/memory` — l'index
  FTS reste auto-suffisant, aucune dépendance croisée vers `idx::memory`.
- **Stats BM25 par agent (`meta`)** : `idx/fts/meta/<agent_len u32 BE><agent>`
  → valeur `FtsStats:1` (magic+version+`doc_count: u64`+`total_terms: u64`+crc32).
  Mis à jour dans le **même** batch que chaque insert/delete (comme le
  compteur `vec_id`, ADR-027 §4). Recalculable — jamais healé silencieusement
  en `open()` (contrairement à `MemoryIndexMeta`, qui heal au niveau global
  du moteur) : la santé est **par agent** et paresseuse — un enregistrement
  absent/corrompu est re-dérivé à la demande par un scan du préfixe
  `docterms` de cet agent (même principe que
  `PersistentMemoryIndex::heal_next_vec_id`, juste borné à un agent au lieu
  du moteur entier, puisqu'il n'existe pas de liste globale des agents à
  healer au démarrage).

Pas de RAM cache dans `PersistentFts` (contrairement à
`PersistentMemoryIndex` qui cache `next_vec_id`) : chaque opération relit et
réécrit ses enregistrements via l'`Engine` directement — même choix de
simplicité que `PersistentGraph` (« pas de méta/rebuild : aucun état de
navigation global à mettre en cache »). Le coût d'un `get` supplémentaire par
insert n'est pas le chemin chaud (les requêtes de recherche dominent, pas les
écritures).

### 4. Atomicité : la même couture qu'ADR-027 §3

`PersistentFts` n'appelle jamais `engine.apply_batch` elle-même pour un
insert/delete de document individuel : elle expose
`stage_insert(engine, agent, vec_id, content, batch: &mut Batch)` et
`stage_delete(engine, agent, vec_id, batch: &mut Batch)`, qui **lisent** l'état
nécessaire (postings existantes pour `stage_delete` — via `docterms`, stats
courantes) mais n'écrivent que dans le `batch` fourni par l'appelant.
`PersistentMemoryIndex::put`/`forget` (déjà responsables de fusionner
mémoire+vecteur dans le même `apply_batch`, ADR-027 §3) grandissent d'un
paramètre `&mut PersistentFts` et empilent ces écritures dans le même
`extra: Batch` transmis à `insert_with`/`delete_with`. Un `remember` natif
reste **un seul enregistrement WAL** : compteur vec_id + record + vecmap +
nœud vectoriel + voisins re-prunés + méta vectorielle + postings FTS +
docterms FTS + stats FTS — présent ou absent en bloc. Même garantie qu'avant,
étendue au troisième index.

`stage_delete` applique les mêmes règles d'idempotence que le reste
d'ADR-027 §3 : un `vec_id` sans `docterms` (déjà effacé, ou jamais indexé —
`content` vide ne produit aucun token) est un no-op silencieux dans le
batch, jamais une erreur — un forget interrompu ne doit pas empêcher un
second forget de terminer le travail.

### 5. Scoring : BM25 Okapi, paramètres FTS5

```text
score(D,Q) = Σ_{t ∈ Q} IDF(t) · tf(t,D)·(k1+1) / (tf(t,D) + k1·(1 - b + b·|D|/avgdl))
IDF(t)     = ln(1 + (N - df(t) + 0.5) / (df(t) + 0.5))
```

`k1 = 1.2`, `b = 0.75` — les défauts de SQLite FTS5's `bm25()` (ADR-014 ne
les personnalise pas). `N`/`avgdl` viennent de `FtsStats` de l'agent
interrogé ; `df(t)` et `tf(t,D)` du scan `postings` du terme. Un document
présent dans **aucun** terme de la requête n'est jamais scoré — comme le
`MATCH` FTS5, qui ne retourne que des lignes correspondant à au moins un
terme (sémantique OR de `fts_match_expr`). Score le plus **élevé** =
meilleur, tri décroissant, borné à `k` — même convention de tri que
`vector_ranking_ids`/`bm25()` (SQLite trie `bm25()` par ordre croissant sur
un coût négatif ; ici le score est positif et trié décroissant, converti en
ids ordonnés avant `rrf_fuse`, qui ne regarde que le rang, pas l'échelle du
score — donc l'inversion de signe n'a aucun impact sur la fusion RRF).

### 6. Erreurs de parsing du `match_expr`

Un `match_expr` hors du sous-ensemble décrit en §1 (guillemets non appariés,
opérateur autre que ` OR `, chaîne vide) est une erreur franche
(`EngineError`/`MemoryError` dédiés), jamais un résultat vide silencieux —
même discipline que le reste d'ADR-027 (« jamais un faux vide qui ferait
passer un résultat dégradé pour correct »).

## Conséquences

- `NativeMemoryStore::keyword_ranking_ids` cesse de retourner une erreur
  franche ; `recall_hybrid` obtient un vrai second signal sur le backend
  natif. `EngineCapabilities::native().full_text` peut passer à `true`.
- Trois nouveaux formats dans `format.lock` (`FtsPosting`, `FtsDocTerms`,
  `FtsStats`) — tout drift de wire casse la CI, comme les neuf existants.
- Écart de parité assumé et documenté : pas de racinisation Porter en N5.2 —
  recall légèrement inférieur à libSQL sur les variantes morphologiques
  (pluriels, conjugaisons anglaises), jamais un faux résultat. Item de TODO
  séparé pour lever ce gap plus tard si mesuré comme significatif.
- Isolation par agent structurelle sur les trois nouveaux keyspaces, comme
  `idx/graph` et `idx/memory` — aucun filtre applicatif ajouté après coup.
- `PersistentMemoryIndex::put`/`forget` changent de signature (paramètre
  `&mut PersistentFts` additionnel) — mise à jour en miroir de tous les
  appelants (`NativeInner` dans `crates/basemyai/src/storage/native_store.rs`,
  tests engine).
- Le harnais crash-consistency gagne (à terme, item de suivi non bloquant
  pour N5.2 lui-même, comme le mode `memory` l'a été pour N5.1/N5.5) une
  couverture du triplet postings+docterms+stats sous kill réel.

## Alternatives rejetées

**Réimplémenter FTS5 en entier (opérateurs, colonnes, wildcards, `NEAR`)** —
travail jeté : aucun appelant du système ne produit ces formes aujourd'hui ;
le jour où `fts_match_expr()` change, ce serait de toute façon un nouveau
contrat à ADR-er.

**Tirer un moteur de recherche externe (Tantivy)** — déjà rejeté par
ADR-014 pour le backend libSQL (viole zéro-dépendance-externe, ADR-011) ;
s'applique encore plus fort ici, où l'objectif explicite du moteur natif est
de ne dépendre de rien d'externe (ADR-024).

**Cache RAM des stats BM25 (`doc_count`/`avgdl`) façon `next_vec_id`** —
rejeté : contrairement à l'allocateur (une valeur globale, un seul
`u64` à tenir cohérent), les stats sont par agent et leur nombre n'est pas
borné à l'avance ; un cache complet nécessiterait soit une éviction, soit
une fuite mémoire proportionnelle au nombre d'agents. Lire/écrire
directement via l'`Engine` à chaque opération est plus simple et n'est pas
le chemin chaud.

**Stocker le compteur `df(t)` séparément (mis à jour à chaque
insert/delete)** — rejeté : un compteur par terme est un état de plus à
maintenir en synchronisation parfaite avec les `postings` réelles ; le
dériver du scan (déjà nécessaire pour récupérer les `tf` de scoring) élimine
la classe de bug « compteur désynchronisé de la donnée qu'il résume »,
cohérent avec la préférence du moteur pour dériver plutôt que cacher quand
le coût est raisonnable.
