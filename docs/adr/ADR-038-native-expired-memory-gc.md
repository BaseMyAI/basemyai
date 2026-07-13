# ADR-038 — GC temporel porté sur le moteur natif (`ExpiredMemoryGc`, scan applicatif paginé)

**Statut** : ✅ Accepted
**Date** : 2026-07-10
**Relation** : réintroduit le second mécanisme retiré par ADR-033 (le
premier, l'oubli adaptatif, a été porté par ADR-037) ; s'appuie sur
ADR-024/025/027 (moteur natif, `MemoryStore`), sur le pattern d'injection
d'ADR-008/`MaintenanceWorker`, et complète ADR-037 (voir l'amendement
« Périmètre affiné » ci-dessous).

## Contexte

Avant ADR-033, un GC temporel supprimait périodiquement les souvenirs dont
`valid_until` était déjà passé — un `DELETE FROM memory WHERE valid_until <=
?` en libSQL, câblé comme tâche de fond indépendante de l'oubli adaptatif
(cf. `docs/status.md` §3, historique). ADR-033 a retiré tout SQL du
workspace ; ce GC a été supprimé plutôt que porté à la hâte, avec la note
qu'un portage natif serait un item de suivi séparé (ADR-037 §Contexte le
documente explicitement comme hors de son propre périmètre).

Le moteur natif ne change rien à la donnée elle-même : chaque souvenir porte
toujours `valid_until: Option<i64>` (`basemyai-engine::idx::memory::record::
MemoryRecord`). `invalidate` (soft-delete applicatif) et l'expiration
naturelle d'une fenêtre de validité produisent toutes deux le même état —
`valid_until.is_some_and(|u| u <= now)` — sans que rien ne les distingue
mécaniquement ; les deux sont donc traitées identiquement par ce GC (comme
elles l'étaient en V1).

## Décision

**Même discipline qu'ADR-037** : scan applicatif en Rust pur, pas de requête
fenêtrée. Deux différences structurelles avec l'oubli adaptatif justifient
un ADR séparé plutôt qu'une extension d'ADR-037 :

1. **Le prédicat est temporel, pas un score.** Pas de tri, pas de départage,
   pas de politique de capacité — juste un filtre `valid_until <= now`.
2. **La pagination est un besoin réel, pas seulement souhaitable.** L'oubli
   adaptatif borne son propre coût par construction (`capacity` limite la
   population *survivante*, donc le nombre de victimes par passe reste
   généralement petit face à la population totale). Le GC temporel n'a pas
   cette borne implicite : un import massif suivi d'une invalidation en
   masse peut laisser des centaines de milliers de lignes expirées d'un
   coup. Charger silencieusement tout ça en un seul `Vec` avant de
   commencer à supprimer serait le genre de pic mémoire non borné que le
   mandat de ce portage interdit explicitement.

**Primitives ajoutées** (`crates/basemyai/src/storage/mod.rs`,
`native_store.rs`) :

- `MemoryStore::scan_expired(agent, now, after_id, limit) ->
  Vec<ExpiredCandidate>` : page triée par id croissant, curseur `after_id`
  **exclusif** porté par l'id (pas par une position numérique) — une page
  reste correcte même si des lignes disparaissent entre deux appels, ce qui
  est le cas normal ici puisque le GC efface au fur et à mesure. Implémenté
  sur `NativeMemoryStore` par un scan complet de l'agent
  (`PersistentMemoryIndex::scan_agent`, même limitation assumée qu'ADR-037 —
  voir Conséquences) filtré à `valid_until.is_some_and(|u| u <= now)`, trié,
  puis tronqué à `limit`.

**Boucle de pagination** (`crates/basemyai/src/maintenance/expired_gc.rs`,
`Memory::expired_gc`) :

```text
cursor = None
loop:
  page = scan_expired(agent, now, cursor, page_size)
  if page.is_empty(): break
  for candidate in page: forget(candidate.id)     // une transaction moteur par item
  cursor = page.last().id
  if page.len() < page_size: break                // dernière page (partielle)
```

Chaque suppression passe par `forget` (souvenir + vecteur + FTS en un seul
batch atomique côté moteur, ADR-027 §3) — jamais un DELETE de masse. Le
curseur avance par id **indépendamment** de ce qui a été supprimé : en mode
réel, les lignes disparaissent au fur et à mesure mais le curseur progresse
quand même correctement (id > cursor exclut ce qui a déjà été vu, supprimé
ou non) ; en mode dry-run (rien n'est supprimé), c'est **la seule** façon
correcte de paginer — une pagination qui supposerait que chaque page réduit
la population restante boucle indéfiniment sur la même page en dry-run.

**Deux points d'entrée**, même pattern qu'ADR-037 :
- `Memory::expired_gc(page_size)` : évince via `Memory::forget` (émission
  d'événement `Forgotten`), le chemin programmatique/`MaintenanceTask`
  (`ExpiredMemoryGcTask`).
- `maintenance::run_expired_gc(store, agent, page_size, dry_run)` : évince
  directement sur `MemoryStore`, sans `Memory` (donc sans charger l'embedder
  Candle) — le chemin CLI (`basemyai gc`), qui supporte le dry-run.

**`page_size == 0` est un rejet explicite** (`MemoryError::InvalidGcPageSize`),
pas un no-op silencieux : une page vide ne progresserait jamais, et un
rapport `{examined: 0, deleted: 0}` se lirait à tort comme « rien n'était
expiré » plutôt que comme une erreur de configuration.

**Portée : par agent uniquement, jamais de passage global.** L'isolation
étant structurelle (préfixe de clé par agent, ADR-027 §2), il n'existe
aucun registre d'agents à énumérer — un passage « tous agents » exigerait
une primitive nouvelle d'énumération inter-agent. Le mandat de ce portage
autorise un passage global *uniquement si l'API le permet déjà explicitement
et sans casser l'isolation* : ce n'est pas le cas aujourd'hui, donc ce
portage reste strictement scopé par agent (voir Conséquences).

**Sémantique : suppression physique, jamais invalidation.** Un souvenir
*déjà* `valid_until <= now` n'a par définition plus rien à invalider — il
l'est déjà. Le seul geste qui a un sens ici est la récupération d'espace
(la vraie fonction d'un GC). Cohérent avec `forget` (RGPD, droit à
l'effacement) et avec ADR-037 (l'oubli adaptatif est lui aussi une
suppression physique) : les trois mécanismes de fin de vie d'un souvenir
(`invalidate`, `forget` manuel, `adaptive_forget`, `expired_gc`) partagent
la même primitive de suppression physique quand ils suppriment, jamais une
suppression "douce" concurrente.

## Non-chevauchement avec l'oubli adaptatif (amendement ADR-037)

En auditant ce portage, un chevauchement conceptuel latent a été identifié
dans la version initiale d'ADR-037 : `scan_for_forgetting` incluait
délibérément les souvenirs déjà invalidés/expirés (« parité V1 »). Cela
permettait à un souvenir mort depuis longtemps mais resté à haute
`importance` de **protéger sa place** dans le calcul de capacité au
détriment d'un souvenir actif moins bien noté — un souvenir invalidé
comptait contre la capacité d'un agent alors qu'il ne devrait plus compter
pour rien.

**Ce portage corrige `scan_for_forgetting`** pour qu'il ne renvoie que les
souvenirs actifs à `now` (`record_valid_at`) — c'est un changement de
comportement par rapport à la version initiale d'ADR-037 (« Inclut les
souvenirs déjà invalidés »), fait ici plutôt que dans un ADR-039 séparé
parce qu'ADR-037 n'était pas encore mergé/publié au moment de ce chantier
(aucun consommateur externe n'a pu s'appuyer sur l'ancien comportement) et
parce que les deux décisions doivent se lire ensemble pour justifier
l'absence de chevauchement. Le fichier `ADR-037-native-adaptive-forgetting.md`
a été mis à jour en conséquence, avec cette note d'amendement.

Résultat : les deux mécanismes opèrent sur des **ensembles disjoints par
construction**.

| Mécanisme | Population considérée | Sémantique de suppression |
|---|---|---|
| Oubli adaptatif (ADR-037, périmètre affiné) | actifs (`valid_until` `None` ou `> now`) | physique, par capacité |
| GC temporel (ADR-038) | expirés (`valid_until <= now`) | physique, par expiration |

Un même souvenir ne peut jamais être candidat aux deux passes simultanément.

## Alternatives rejetées

- **Étendre ADR-037 pour couvrir aussi le GC temporel.** Rejeté : le
  prédicat (temporel vs. score+capacité) et le besoin de pagination réelle
  (borné par construction pour l'un, potentiellement pas pour l'autre) sont
  suffisamment différents pour justifier un mécanisme et un ADR séparés —
  les regrouper aurait produit un seul module avec deux responsabilités non
  liées.
- **Pagination par position numérique (offset/limit) plutôt que par
  curseur d'id.** Rejeté : un `OFFSET` se désynchronise dès que des lignes
  disparaissent entre deux pages (exactement ce qui arrive en mode réel,
  puisque le GC supprime au fur et à mesure) — soit il saute des lignes,
  soit il en retraite. Le curseur par id est stable face aux suppressions
  concurrentes du même passage.
- **Un passage global (tous agents) par défaut.** Rejeté : aucune primitive
  d'énumération d'agents n'existe aujourd'hui (isolation structurelle,
  ADR-027 §2) ; en construire une pour ce seul GC serait une abstraction
  disproportionnée et risquerait d'affaiblir l'isolation si elle est faite
  à la hâte. Un passage global reste possible en bouclant côté appelant sur
  une liste d'agents connue par lui (le CLI/les surfaces savent déjà quels
  agents ils servent) — ce n'est pas une primitive du moteur.
- **`DELETE` de masse par page (batch atomique multi-souvenir).** Rejeté
  pour la même raison qu'ADR-037 §Alternatives : le moteur n'expose pas de
  primitive de suppression multi-souvenir en un seul enregistrement WAL
  (`forget` est unitaire) ; en ajouter une pour ce seul GC serait une
  optimisation prématurée non mesurée. Chaque suppression individuelle reste
  atomique, la page n'est qu'un plafond mémoire, pas une unité de
  transaction.

## Conséquences

✅ Sémantique de suppression alignée avec `forget`/`adaptive_forget` :
physique, jamais une invalidation "en plus" d'un état déjà invalide.
✅ Pas de chevauchement possible avec l'oubli adaptatif (ensembles disjoints
par construction, cf. tableau ci-dessus) — et ce portage a corrigé une
fuite conceptuelle latente dans la version initiale d'ADR-037.
✅ Pagination par curseur d'id, correcte aussi bien en mode réel (lignes
qui disparaissent au fur et à mesure) qu'en dry-run (rien ne disparaît).
✅ Idempotent et reprennable par construction : une interruption laisse
chaque souvenir soit pleinement présent soit pleinement absent (jamais
d'état intermédiaire — chaque `forget` est sa propre transaction moteur),
et relancer le GC termine le travail restant sans double-traitement (un
souvenir déjà supprimé n'apparaît plus dans la page suivante).
⚠️ **Coût perf : scan complet de l'agent à chaque page**, même limitation
assumée qu'ADR-037 (pas d'index secondaire sur `valid_until` côté moteur
natif) — chaque appel à `scan_expired` reparcourt tous les souvenirs de
l'agent pour en extraire la sous-population expirée, avant de tronquer à
`limit`. Le résultat *matérialisé* est borné (`limit`), mais le *coût de
scan* ne l'est pas. Pour un agent à très grand nombre de souvenirs avec peu
d'expirés, c'est `O(n)` en I/O par page pour trouver une poignée de lignes
— acceptable pour une tâche de fond peu fréquente, à revisiter si mesuré
comme point chaud (même item de suivi qu'ADR-037 : un index secondaire par
`valid_until` résoudrait les deux mécanismes à la fois, mais desynchronise
dès que le temps avance — cf. ADR-037 §Alternatives rejetées, même
raisonnement s'applique ici).
⚠️ **Pas de passage global.** Un consommateur qui veut un GC "tous agents"
doit boucler lui-même sur les agents qu'il connaît — aucune primitive du
moteur ne les énumère. Item de suivi si un besoin réel émerge.
⚠️ Comme ADR-037, aucun moyen public de fixer `valid_until` autrement que
via `invalidate`/l'expiration naturelle d'une fenêtre passée à `remember_with` —
limitation préexistante, pas introduite ici.
⚠️ **Ni REST, ni MCP, ni les bindings ne câblent `forget-adaptive`/`gc`.**
Aucune des deux capacités ne faisait partie du contrat de ces surfaces avant
leur retrait (ADR-033), et l'architecture actuelle n'expose ni consolidation
ni maintenance via REST — seul MCP expose `consolidate`/`consolidate_apply`.
Ce portage reste donc scopé CLI + `Memory`/`MaintenanceTask` (le mandat
« REST/MCP/bindings uniquement si ces capacités faisaient partie de leur
contrat ou si l'architecture actuelle les expose déjà » ne s'applique pas
ici). Étendre l'exposition à REST/MCP est un item de suivi produit distinct,
pas une omission de ce chantier.
