# ADR-041 — Index d'importance, index temporel et maintenance bornée sur le moteur natif

**Statut** : ✅ Accepted (§7.1 à §7.5 livrés)
**Date** : 2026-07-13
**Relation aux ADR existants** : prolonge ADR-012 (formule de score de l'oubli
adaptatif), ADR-037 (oubli adaptatif porté sur le natif, scan applicatif non
borné) et ADR-038 (GC temporel porté sur le natif, scan applicatif paginé
mais toujours O(n) par agent). N'amende aucun format existant : chaque ajout
est une **nouvelle** structure dérivée, jamais une réécriture d'une
existante. C'est le jalon **N10** du programme production-hardening
(`docs/PLAN-NATIVE-ENGINE.md` §7).

## Contexte

ADR-037 et ADR-038 ont porté l'oubli adaptatif et le GC temporel sur le
moteur natif en scan applicatif pur — et ont chacun documenté honnêtement,
dans leur propre section, la même limitation non résolue : un scan complet
de tous les souvenirs d'un agent, à chaque passe. ADR-038 §« Scope » nomme
explicitement l'index secondaire sur `valid_until` comme le correctif, mais
le reporte : *« desynchronise dès que le temps avance… même item de suivi
qu'ADR-037 »*. ADR-037 note symétriquement qu'aucune API publique ne permet
de fixer `importance` — le score `importance + H/(H+age)` (ADR-012 §4) se
réduit donc en pratique à un tri par seule récence.

N10 referme ces deux gaps, plus deux volets supplémentaires (suppression par
lot bornée, registre d'agents) — voir `docs/PLAN-NATIVE-ENGINE.md` §7 pour
le plan complet. Cet ADR documente les décisions au fur et à mesure de leur
implémentation plutôt que d'anticiper un design complet avant tout code —
même discipline que l'amendement en tête d'ADR-037.

## Décision

### 7.1 — API d'importance (✅ livré 2026-07-12/13)

Le moteur acceptait déjà une `importance: f64` arbitraire par record
(`NewMemoryRecord`/`MemoryRecord`, champ réservé depuis N5.1) — seule la
façade `basemyai` la figeait à `1.0`. Aucun changement de format n'était
nécessaire, seulement du plumbing :

- `Memory::remember_with_importance(text, layer, validity, importance)` —
  variante publique de `remember_with`, importance explicite.
- `Memory::set_importance(id, importance)` — réécrit l'importance d'un
  souvenir existant, no-op silencieux si absent/autre agent (même parité
  UPDATE qu'`invalidate`).
- `MemoryError::InvalidImportance` rejette NaN/infini (contaminerait tout
  tri d'oubli adaptatif) ; une importance **négative** reste acceptée
  (signal explicite « évincer en premier »).
- `MemoryStore::put_memory`/`NewMemory` gagnent un champ `importance: f64`
  (`DEFAULT_IMPORTANCE = 1.0` pour tout appelant qui ne le fixe pas —
  `remember`/`remember_batch*` inchangés en usage).

### 7.2 — Index temporel d'expiration + `Engine::scan_range` (✅ livré 2026-07-13)

**Constat qui a changé le design initialement envisagé** : poser un index
`idx/temporal/expiry/<agent><valid_until><id>` trié par `valid_until` ne
suffit pas à lui seul — le moteur n'exposait que `Engine::scan_prefix`, qui
matérialise **tout** ce qui partage un préfixe fixe. Sans borne supérieure,
lire « tout ce qui est `<= now` » via un préfixe d'agent revient à
matérialiser tout le sous-arbre temporel de l'agent : un gain de format
(clés minces, zéro décodage de `MemoryRecord`) mais pas le O(log n + k)
promis — poser un index sans le moyen de l'interroger efficacement aurait
été un faux progrès, contraire à la discipline « jamais de faux succès »
(ADR-040 §2).

Décision : ajouter une primitive moteur **avant** l'index qui en dépend.

1. **`BlockSstFile::entries_with_range(start, end)`** (`store/sst_block.rs`)
   — le pendant à deux bornes d'`entries_with_prefix` (N8.11) : même
   `partition_point` sur `block_index` pour trouver le premier bloc pouvant
   contenir `start`, arrêt dès qu'un bloc `first_key >= end` (aucun bloc
   suivant ne peut plus contenir un match, les blocs étant triés). Un bloc
   frontière peut contenir des voisins hors plage, d'où le filtre par
   entrée. `start >= end` est une plage vide, pas une erreur.
2. **`Engine::scan_range(start, end)`** (`store/engine.rs`) — même fusion
   memtable + SST par `BTreeMap` que `scan_prefix`, mais bornée des deux
   côtés. C'est la primitive générique ; rien dans sa signature ne connaît
   `valid_until` ou la mémoire — mécanisme au moteur, sens au consommateur
   (même principe que `basemyai-core`).
3. **`key::temporal_index`** (`key/mod.rs`) — nouveau module réservé,
   `idx/temporal/expiry/<agent_len: u32 BE><agent><valid_until: 8 octets
   sortable><id>`. `valid_until` (signé) est encodé par **inversion du bit
   de signe** (`(value as u64) ^ (1u64 << 63)`) : l'ordre des octets égale
   l'ordre numérique sur tout l'intervalle `i64`, pas seulement les
   timestamps positifs de ce domaine — testé explicitement sur la frontière
   négatif/positif. `expiry_upper_bound(agent, at)` calcule la borne
   supérieure exclusive de la plage `valid_until <= at`
   (`sortable(at.saturating_add(1))`) ; sature à `i64::MAX` (hors de portée
   réaliste pour des secondes Unix, documenté plutôt que masqué).
   **Aucune entrée `format.lock`** : la valeur est un marqueur vide (`id`
   et `valid_until` sont déjà entièrement recouvrables depuis la clé), et
   seuls les payloads de valeur sont verrouillés au format — pas les
   layouts de clé (règle déjà établie par `key::memory_index` et
   consorts).
4. **Composition atomique** — `PersistentMemoryIndex` reste seule
   propriétaire de la composition crash-critique (même principe que
   vecmap/FTS) :
   - `put_many` : une entrée d'expiration est empilée dans le même batch
     que le record, seulement si `valid_until.is_some()` (un souvenir
     éternel n'a jamais d'entrée — rien à jamais expirer).
   - `update` (le chemin `invalidate`/`set_importance`) devient
     **auto-suffisant** : il relit l'ancien `valid_until` du record avant
     de le réécrire, et ne touche l'index temporel que si `valid_until` a
     réellement changé — `invalidate` la déplace, `set_importance` (qui ne
     change jamais `valid_until`) ne la touche pas. Le tout sur **un seul**
     batch atomique (`update` passe de `engine.put` à `engine.apply_batch`
     — même garantie de durabilité pour la clé record, désormais étendue
     à la clé d'expiration).
   - `forget`/`purge_agent` suppriment l'entrée d'expiration si
     `stored.valid_until.is_some()`, dans le même batch que le reste.
5. **`PersistentMemoryIndex::scan_expiring(engine, agent, at)`** — la
   requête `[expiry_agent_prefix(agent), expiry_upper_bound(agent, at))`
   via `Engine::scan_range`, décodée directement depuis les clés (`id`,
   `valid_until`) sans jamais décoder un `MemoryRecord`. C'est la primitive
   que `NativeMemoryStore::scan_expired` (contrat public `MemoryStore`,
   inchangé — curseur `after_id: Option<&str>`, pagination stable) appelle
   désormais à la place du `scan_agent` + filtre + tri complet d'ADR-038 :
   le tri par id et le curseur restent en mémoire (contrat externe
   préservé à l'identique), mais portent désormais sur le seul ensemble
   déjà filtré aux souvenirs expirés — jamais tout l'agent.

### 7.3 — Oubli adaptatif à mémoire bornée (✅ livré 2026-07-13)

ADR-037 matérialisait **tous** les souvenirs actifs d'un agent
(`scan_for_forgetting` non borné) puis triait la liste complète — mémoire
`O(n)`, exactement ce que N10 doit fermer. Même constat qu'en §7.2 : la
pagination ne pouvait pas être posée au niveau du store seul, car le moteur
n'offrait que des scans qui matérialisent tout (`scan_prefix`,
`scan_range`) — paginer au-dessus d'une matérialisation complète aurait été
un faux progrès. La primitive moteur d'abord, donc :

1. **`BlockSstFile::entries_with_range_limited(start, end, limit)`**
   (`store/sst_block.rs`) — `entries_with_range` qui s'arrête dès `limit`
   matches collectés. Renvoie aussi `truncated` : la source n'est alors
   complète que jusqu'à sa dernière clé renvoyée. Granularité de bloc
   assumée (léger dépassement possible, jamais de troncature intra-bloc —
   elle briserait l'invariant « complet jusqu'à la dernière clé »).
2. **`Engine::scan_range_page(start, end, limit)`** (`store/engine.rs`) —
   une page d'au plus ~`limit` entrées vivantes, mémoire
   `O(sources × limit)`. Correction sous couches LSM par **frontière** :
   chaque source (SSTs + memtable) est lue bornée ; la page ne garde que
   les clés `<= min` des frontières des sources tronquées (au-delà, une
   couche plus récente pas encore lue pourrait écraser/tombstoner — le
   reliquat fusionné est jeté et relu à la page suivante). Le protocole
   d'appel est `next_start` : **une page vide avec `next_start = Some` est
   une progression, pas une fin** (plage de tombstones) — les appelants
   bouclent sur `next_start`, jamais sur `entries.is_empty()`. La frontière
   memtable est la dernière clé effectivement prise, pas la clé d'arrêt
   (qui peut masquer une entrée SST déjà fusionnée — testé).
3. **`key::memory_index::record_agent_upper_bound`** — borne supérieure
   exclusive du keyspace records d'un agent (dernier octet non-`0xff`
   incrémenté), le `end` que la range query paginée exige.
4. **`PersistentMemoryIndex::scan_agent_page(engine, agent, after_id,
   limit)`** — repose le contrat simple « curseur par id, page courte ⇔
   épuisé » au-dessus du protocole `next_start` (la boucle interne absorbe
   les pages vides-mais-non-finales).
5. **`MemoryStore::scan_for_forgetting(agent, now, after_id, limit)`** —
   le contrat passe de « tout l'agent » à une page de `limit` **candidats**
   (breaking, 0.2.0) : le filtre de validité s'applique après la pagination
   brute, et l'implémentation re-page en interne pour qu'une page pleine de
   bruts invalides ne raccourcisse jamais la page vue du consommateur —
   « page courte ⇔ agent épuisé » reste vrai.
6. **Deux passes dans `maintenance::adaptive_forgetting`** :
   - Passe 1 (`select_survivors`) : scan paginé, `SurvivorSelector` — tas
     binaire borné à `capacity` dont le sommet est le survivant le plus
     faible (score de rétention croissant, id décroissant en départage ; le
     rang total est inchangé depuis ADR-012 : score décroissant, id
     croissant survit). Mémoire `O(capacity + page)`, calcul
     `O(n log capacity)`, résultat indépendant de l'ordre des pages (ordre
     total sur ids uniques — testé). Le produit est l'ensemble des
     **survivants** (`O(capacity)`), jamais la liste des victimes
     (`O(n − capacity)`, non bornée).
   - Passe 2 (`next_victim_page`) : re-scan paginé **au même `now`** que la
     passe 1 (prédicat de population gelé), éviction de tout candidat hors
     survivants, page par page — le curseur est porté par l'id du dernier
     candidat brut, donc insensible aux évictions derrière lui (même
     argument qu'ADR-038). L'éviction reste souvenir par souvenir (une
     transaction moteur par victime — le lot borné est le ressort de §7.4).
   - `dry_run` s'arrête après la passe 1 : `evicted = scanned − capacity`,
     aucune mutation.
   - Les deux points d'entrée (`Memory::adaptive_forget`, événementiel ;
     `run` CLI, sans Candle) partagent sélection et pagination, seul le
     chemin d'éviction diffère — même découpage qu'ADR-037.

**Fenêtre entre les passes, assumée** : le prédicat étant gelé au `now` de
la passe 1, un souvenir inséré entre les passes avec la validité par défaut
(`valid_from = now(insertion) > now(passe 1)`) n'entre jamais dans la
passe 2. Seul un souvenir inséré entre les passes avec un `valid_from`
explicitement **antidaté** peut être évincé sans avoir été scoré (il n'est
pas dans l'ensemble des survivants). Fenêtre de la durée d'une passe,
tolérée pour une politique de capacité best-effort ; l'alternative (seuil
de score plutôt qu'ensemble de survivants) échangerait ce cas contre une
sensibilité à la dérive des scores entre les passes — non retenue.

**Changement de comportement visible** : l'ordre d'éviction (et donc des
événements `Forgotten`) passe de « par rang de score » à « par id
croissant » (l'ordre du scan). Aucun contrat public ne promettait l'ordre ;
le rapport (`scanned`/`evicted`) est inchangé.

### 7.4 — `forget_many` (✅ livré 2026-07-13)

Avant §7.4, toute suppression multiple (passe 2 de l'oubli adaptatif, GC
temporel, purge partielle) était une transaction moteur **par souvenir** —
correct mais N fsync/WAL records pour N victimes. L'agrégation naïve dans un
seul batch aurait produit l'autre extrême : un WAL record non borné. Deux
primitives par index, puis la composition bornée :

1. **`PersistentFts::stage_delete_many(engine, agent, vec_ids, batch)`** —
   le pendant suppression de `stage_insert_many` (N5.5), pour la même
   raison : `stage_delete` fait un read-modify-write sur le record de stats
   BM25, donc deux appels dans un batch partagé non appliqué liraient des
   stats périmées et perdraient des décréments (testé explicitement). Une
   seule écriture de stats agrégée pour tout le groupe ; ids dupliqués ou
   jamais indexés ne comptent pas. S'ajoute `delete_footprint(engine, agent,
   vec_id)` : les octets de clés qu'un futur `stage_delete` posterait — la
   sonde du budget d'octets, au prix d'un point-lookup docterms
   supplémentaire (assumé : la borne doit être connue **avant** de poser le
   chunk).
2. **`PersistentVectorIndex::delete_many_with(engine, ids, extra)`** — le
   pendant suppression d'`insert_many_with` : toutes les tombstones + **un
   seul** record de métadonnées (compteur décrémenté du groupe entier) + le
   batch compagnon `extra`, en un seul `apply_batch`. Ids absents/déjà
   tombstonés/dupliqués ignorés ; même asymétrie que `delete_with` : un
   passage sans aucune tombstone applique quand même un `extra` non vide
   (les suppressions compagnonnes d'une tentative interrompue ne doivent
   pas survivre). Les tombstones étant des réécritures de blocs
   indépendants (pas de re-pruning), l'« état mutable cohérent » du plan se
   réduit à la déduplication intra-appel + le compteur unique.
3. **`PersistentMemoryIndex::forget_many(engine, vectors, fts, agent, ids,
   options)`** avec `ForgetBatchOptions { max_items, max_wal_bytes }`
   (défauts : 256 items / ~4 Mio — ordre de grandeur, pas un optimum
   mesuré, même posture que le défaut du block cache N8.7). Le groupe est
   découpé en **chunks** : au sein d'un chunk, record + vecmap + entrée
   d'expiration + FTS agrégé + tombstones vectorielles = **un** WAL record
   atomique. La comptabilité d'octets est estimative
   (`Batch::approx_wire_bytes`, `approx_tombstone_wire_bytes` dérivé des
   paramètres d'index, `delete_footprint` FTS) : cible de dimensionnement,
   pas une borne exacte au fil — et un souvenir dont l'empreinte propre
   dépasse `max_wal_bytes` part quand même, seul dans son chunk
   (l'atomicité par souvenir est le plancher, un souvenir n'est jamais
   scindé entre deux batchs). **Reprise idempotente entre les chunks** :
   les ids absents (ou dupliqués) sont silencieusement sautés, un crash
   entre deux chunks se répare en relançant le même appel — même discipline
   que `purge_agent` (ADR-027 §6), la frontière élargie du souvenir au chunk.
4. **Surface consommateur** : `MemoryStore::forget_many(agent, ids,
   options)` (nouvelle méthode du trait, breaking 0.2.0 comme
   `scan_for_forgetting` §7.3), parité DELETE (ids absents/cross-agent
   ignorés). Consommateurs câblés : passe 2 de l'oubli adaptatif et GC
   temporel, dans leurs **deux** points d'entrée chacun — CLI
   (`adaptive_forgetting::run`, `expired_gc::run`) et façade `Memory`
   (`adaptive_forget`, `expired_gc` via `forget_batch_with_events` : couche
   capturée avant l'effacement, lot borné, événements `Forgotten` émis
   après commit, uniquement pour les souvenirs qui existaient — le contrat
   événementiel de `Memory::forget` préservé à l'identique).

### 7.5 — Registre d'agents (✅ livré 2026-07-13)

`meta/agents/<agent_len: u32 BE><agent>`, valeur = marqueur vide (l'id est
entièrement recouvrable depuis la clé — aucune entrée `format.lock`, même
règle que l'index temporel §7.2). Maintenu par `PersistentMemoryIndex` :

- **Inscription** : un marqueur est empilé dans le même batch atomique que
  chaque `put_many` — écrasement idempotent d'une valeur vide, plutôt qu'un
  read-before-write à chaque insertion.
- **Désinscription** : `purge_agent` supprime l'entrée **en dernier** — un
  crash au milieu de la purge laisse l'entrée en place, et relancer la
  purge (la reprise documentée ADR-027 §6) la retire ; jamais l'ordre
  inverse, qui rendrait des souvenirs non purgés invisibles aux
  consommateurs du registre.
- **`forget`/`forget_many` du dernier souvenir laissent l'entrée**,
  volontairement : le registre répond « quels agents une passe de
  maintenance doit-elle visiter » — visiter un agent vide est un no-op bon
  marché, alors qu'un read-modify-check à chaque forget ne le serait pas.
  Le registre n'est jamais un compteur.
- **Lecture** : `PersistentMemoryIndex::list_agents(engine)` (scan du
  préfixe réservé, erreur franche sur clé malformée), exposé côté produit
  par `NativeMemoryStore::list_agents()` — méthode **inhérente**, pas sur le
  trait `MemoryStore` : c'est une lecture conteneur, pas une opération
  d'agent (même statut que `total_memory_count`, ADR-032). Identifiants
  seuls, jamais aucune donnée par agent — l'isolation ADR-006 reste
  structurelle pour tout ce que l'id débloque ensuite.

**Limite documentée** : le registre n'est alimenté qu'à l'écriture — un
store créé avant N10 n'a pas d'entrées pour ses agents existants tant
qu'ils ne réécrivent pas. Acceptable avant toute publication du format
(0.2.0 non publiée, moteur non publié) ; un `rebuild` du registre depuis le
keyspace records serait mécanique si le besoin apparaît (même catégorie de
réparation que les dérivées d'ADR-040 §1).

## Conséquences

- (+) `Engine::scan_range` est une primitive générique réutilisable par tout
  futur index trié par plage (candidat naturel pour N14 — quantification/
  explicabilité — ou tout futur secondaire index), pas un raccourci à usage
  unique.
- (+) La frontière primaires/dérivées d'ADR-040 §1 est respectée :
  l'index temporel est une dérivée pure (marqueur vide, reconstructible
  depuis les records primaires) — sa catégorie de réparation (`rebuild`)
  suit naturellement le même modèle que vecmap/FTS, à câbler dans un futur
  jalon `rebuild-indexes` si un besoin réel apparaît (aucune primitive
  nouvelle requise, `scan_agent` + `put_many`'s staging suffiraient).
- (+) Le contrat public `MemoryStore::scan_expired` n'a **pas changé** —
  aucun consommateur (CLI `gc`, `ExpiredMemoryGcTask`, REST/MCP) n'a besoin
  d'être touché pour bénéficier du gain de performance.
- (−) `update()` fait désormais un `get` de plus par appel
  (`invalidate`/`set_importance`) pour connaître l'ancien `valid_until` —
  coût négligeable (un point-lookup) face au batch qu'il écrit de toute
  façon, mais un coût réel, assumé pour la simplicité (pas besoin de
  changer la signature de `update` ni celle de ses appelants côté
  `basemyai`).
- (+) `Engine::scan_range_page` (§7.3) est le scan paginé générique que la
  doc de `scan_prefix` différait (« a streaming scan is deliberately
  deferred until something needs it ») — désormais nécessaire et livré,
  réutilisable par tout futur consommateur de scan borné.
- (−) La passe 2 relit chaque page depuis le disque au lieu de rejouer une
  liste en mémoire — c'est le prix exact de la borne `O(capacity)` : deux
  scans au lieu d'un, assumé (les pages sont bornées, le coût est linéaire
  et sans pic mémoire).
- (+) Une passe d'éviction/GC de N victimes coûte désormais ~N/`max_items`
  WAL records (fsync compris) au lieu de N — sans jamais produire un record
  géant (§7.4). L'ordre des événements `Forgotten` est inchangé (id
  croissant, ordre du scan — §7.3), seule la granularité de commit a changé.
- (−) Le budget d'octets de §7.4 est **estimatif** (clés + empreinte FTS
  relue + majorant de tombstone dérivé des paramètres), pas une mesure du
  record WAL encodé — assumé : c'est une cible de dimensionnement, et
  `delete_footprint` coûte déjà un point-lookup docterms par souvenir.
- (−) Le registre §7.5 n'est pas rétroactif (voir la limite documentée) et
  n'est **pas** un compteur : une entrée peut désigner un agent
  actuellement vide (dernier souvenir oublié sans purge). Les consommateurs
  le traitent comme une énumération de candidats à visiter, rien de plus.
