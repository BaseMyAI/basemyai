# ADR-037 — Oubli adaptatif porté sur le moteur natif (scan applicatif, sans fenêtrage SQL)

**Statut** : ✅ Accepted
**Date** : 2026-07-10
**Relation** : porte la décision 4 d'ADR-012 (« Oubli adaptatif ») sur le
moteur natif après son retrait par ADR-033 ; amende ADR-012 (la formule de
score et la politique sont conservées à l'identique, seul le mécanisme de
sélection change) ; s'appuie sur ADR-024/025/027 (moteur natif,
`MemoryStore`) et sur le pattern d'injection d'ADR-008/`MaintenanceWorker`.

## Contexte

ADR-012 spécifiait l'oubli adaptatif comme une éviction périodique, par
agent, au-delà d'une capacité configurée :

```
score = importance + H / (H + max(0, now - last_access))
```

(`H` = `recency_half_life_secs`, decay **hyperbolique** — pas
`0.5^(age/H)`, qui sous-déborde en `0.0` dès que `age` atteint quelques
centaines de demi-vies avec des timestamps Unix réels).

L'implémentation libSQL sélectionnait les survivants avec une **requête de
fenêtrage SQL** :

```sql
DELETE FROM memory WHERE id IN (
  SELECT id FROM (
    SELECT id, ROW_NUMBER() OVER (
      PARTITION BY agent_id
      ORDER BY importance + H / (H + max(0, now - COALESCE(last_access, valid_from))) DESC, id
    ) AS rn FROM memory
  ) WHERE rn > capacity
)
```

ADR-033 a retiré libSQL et tout SQL du workspace. `ROW_NUMBER() OVER
(PARTITION BY …)` n'a **aucun équivalent** côté `basemyai-engine` : le
moteur natif n'expose pas de langage de requête, seulement des primitives
KV/vecteur/graphe/FTS (ADR-024). Le commentaire laissé dans
`crates/basemyai-cli/src/commands/maintenance.rs` et
`crates/basemyai/src/maintenance/mod.rs` documentait explicitement ce trou :
l'oubli adaptatif a été supprimé plutôt que porté à la hâte, avec la note
qu'« un portage mérite son propre design/tests ». Cet ADR est ce design.

Le moteur natif conserve déjà `importance`/`last_access` par souvenir
(`basemyai-engine::idx::memory::record::MemoryRecord`, champs réservés
« pour un GC futur » depuis le portage N5.1), et expose déjà un scan complet
par agent (`PersistentMemoryIndex::scan_agent`, déjà utilisé par
`NativeMemoryStore::export_rows` et `MemoryStore::list_memories`). Il ne
manque que la sélection des survivants.

## Décision

**Scan applicatif en Rust pur, à la place de la requête fenêtrée SQL.** Le
score et la politique (capacité par agent, demi-vie) restent ceux d'ADR-012,
inchangés. Seul le mécanisme de sélection change :

1. **Brique de lecture** — `MemoryStore::scan_for_forgetting(agent) ->
   Vec<ForgetCandidate>` (nouveau, `crates/basemyai/src/storage/mod.rs`) :
   scan complet des souvenirs de l'agent, uniquement `id`/`importance`/
   `last_access` (pas le contenu — inutile au score). Implémenté sur
   `NativeMemoryStore` par un simple appel à `PersistentMemoryIndex::scan_agent`,
   déjà existant.
2. **Brique de sélection** — fonction pure `select_victims(candidates, now,
   policy) -> Vec<String>` (`crates/basemyai/src/maintenance/adaptive_forgetting.rs`) :
   trie les candidats par score décroissant (`id` croissant en
   départage — même règle qu'ADR-012), tronque à `capacity`, renvoie les ids
   au-delà. Fonction pure, testée unitairement sans I/O ni horloge réelle.
3. **Brique d'éviction** — `Memory::adaptive_forget(policy) ->
   ForgettingReport` (`crates/basemyai/src/memory/mod.rs`) : scanne, sélectionne,
   puis appelle `Memory::forget(id)` (méthode déjà existante — suppression
   physique atomique + émission d'événement) pour chaque victime. Une
   éviction = un appel `forget` = une transaction moteur, pas un DELETE de
   masse : c'est le changement de coût assumé ci-dessous.
4. **Wiring `MaintenanceTask`** — `AdaptiveForgettingTask` (même fichier),
   même pattern que `ConsolidationTask` (auto-suffisante, `Arc<Memory>` +
   politique injectée, ignore le paramètre de store partagé du worker —
   ADR-032/033).

L'isolation par agent, qui venait de `PARTITION BY agent_id` en SQL, tombe
gratuitement : `Memory` est déjà scellée par `AgentId` (ADR-006), et
`scan_for_forgetting` ne lit que les clés structurellement préfixées par cet
agent (ADR-027 §2) — il n'y a pas de fuite possible entre agents à porter.

Le GC temporel (`ExpiredMemoryGc`, `valid_until` expiré) reste **hors
scope** : c'était un mécanisme indépendant avant son retrait (ADR-033), non
mentionné dans le mandat de ce chantier, et sa portée native est un item de
suivi séparé (voir Conséquences).

## Alternatives rejetées

- **Index secondaire trié par score, maintenu à jour à chaque écriture**
  (façon B-tree applicatif sur `score(now)`). Rejeté : le score dépend de
  `now` au moment de l'évaluation (terme `H/(H+age)`), donc un index
  « trié par score » se désynchronise dès que le temps avance — il faudrait
  soit le recalculer à chaque lecture (perd l'intérêt de l'index), soit
  indexer sur une clé qui ne bouge pas avec le temps (`last_access` seul,
  perdant `importance`). Complexité de maintenance disproportionnée pour un
  GC qui tourne en tâche de fond peu fréquente (pas un chemin chaud).
- **Un langage de requête minimal dans `basemyai-engine`** (mini-SQL,
  agrégations). Rejeté : ADR-024 exclut explicitement un langage de requête
  de V1 (« V2 : … langage de requête », `CLAUDE.md` § Statut) ; construire un
  moteur de requête pour un seul GC serait une abstraction sans second cas
  d'usage.
- **Conserver le calcul en SQL mais sur une base intermédiaire** (ex.
  ré-ouvrir une connexion SQLite en mémoire juste pour cette tâche). Rejeté :
  réintroduirait une dépendance SQL que ADR-033 a explicitement retirée du
  workspace ; un second format de données à synchroniser avec le moteur
  natif pour un seul mécanisme de maintenance.
- **`0.5^(age/H)` (decay exponentielle)** : toujours rejeté pour la même
  raison qu'ADR-012 (sous-débordement à `0.0` aux échelles Unix réelles) —
  aucune raison de revisiter ce choix en portant le mécanisme.

## Conséquences

✅ Formule et politique d'ADR-012 préservées à l'identique (aucune migration
de configuration côté appelant).
✅ `importance`/`last_access` n'ont pas eu besoin d'être ajoutés : ils étaient
déjà portés par `MemoryRecord` (réservés depuis N5.1 justement pour ce GC).
✅ Isolation par agent gratuite (structurelle, ADR-027 §2), pas de logique
`PARTITION BY` à reproduire.
✅ Sélection testable unitairement sans horloge réelle ni moteur ouvert
(fonction pure `select_victims`).
⚠️ **Coût perf : scan complet de l'agent à chaque passe**, `O(n log n)` en
mémoire (tri), contre une requête indexée côté SQL qui pouvait
théoriquement s'appuyer sur un index composite. En pratique la requête
libSQL originale était déjà un scan complet de la table `memory` filtré par
`agent_id` (`ROW_NUMBER() OVER` ne peut pas s'appuyer sur un index pour un
`ORDER BY` calculé) — le portage ne dégrade donc pas la complexité
asymptotique, mais il matérialise désormais tous les candidats en mémoire
process (`Vec<ForgetCandidate>`) plutôt que de les faire transiter par un
curseur SQL streamé. Pour un agent à très grand nombre de souvenirs, c'est
un pic mémoire à surveiller — aucune pagination n'est implémentée dans cette
première version.
⚠️ **Éviction ligne par ligne** (`N` appels `Memory::forget`, chacun une
transaction moteur) plutôt qu'un unique `DELETE` de masse. Plus simple et
réutilise le chemin `forget` existant (événements, atomicité souvenir+FTS
déjà garantis) au prix de `N` allers-retours dans le pool bloquant au lieu
d'un seul. Acceptable pour une tâche de fond peu fréquente ; à revisiter
(`forget_batch`) si le volume d'éviction par passe devient un point chaud
mesuré.
⚠️ Le GC temporel (`ExpiredMemoryGc`) reste non porté — hors scope de cet
ADR, item de suivi séparé.
⚠️ Pas de moyen public de fixer `importance` autrement que la valeur par
défaut (`1.0`, ADR-027) à l'écriture — le score se réduit donc aujourd'hui à
la seule composante de récence tant qu'aucune API ne permet de faire varier
`importance` par souvenir. Limitation préexistante (pas introduite par cet
ADR), documentée ici pour visibilité.
