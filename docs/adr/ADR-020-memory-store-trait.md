# ADR-020 — `MemoryStore` : contrat d'opérations mémoire dans `basemyai`

**Status**: Amended by ADR-032
**Date**: 2026-06-20
**Context**: suivi d'ADR-019 ; depuis ADR-032, `NativeMemoryStore` est
l'unique implémentation active du contrat `MemoryStore`.

## Contexte

`docs/status.md` marquait `StorageEngine` 🟡 : `basemyai_core::StorageEngine`
(`crates/basemyai-core/src/storage/engine.rs`) n'est qu'un contrat
d'identité/capacités (`capabilities() -> EngineCapabilities`), pas le trait
d'opérations mémoire (`put_memory`/`recall_vector`/`graph_upsert_entity`/…)
que recommandait `docs/strategy/2026-06-18-agent-memory-database-research.md`.
Conséquence concrète : `Filter`/`Value` (SQL-leaky,
`basemyai_core::storage::vector`) et du SQL brut fuyaient directement dans
`crates/basemyai/src/memory/mod.rs`, `cognition/graph.rs` et
`cognition/consolidation.rs`.

## Décision

1. Nouveau trait `MemoryStore` (object-safe, `#[async_trait]`), défini dans
   `crates/basemyai/src/storage/mod.rs` — **dans `basemyai`, pas
   `basemyai-core`**. Les opérations qu'il expose (`put_memory`,
   `agent_stats`, `graph_upsert_entity`…) connaissent `agent_id`, les couches
   mémoire et le graphe — exactement ce qu'ADR-001 interdit au core
   agnostique. C'est un *second* contrat, à un niveau sémantique différent du
   `basemyai_core::StorageEngine` existant (capacités), qui ne change pas.
2. `LibsqlMemoryStore` (`crates/basemyai/src/storage/libsql_store.rs`) est
   l'unique implémentation V1, enveloppant `basemyai_core::Store`. Elle
   concentre tout le `Filter`/`Value`/SQL brut qui vivait avant dans
   `memory/mod.rs`, `cognition/graph.rs` et `cognition/consolidation.rs` — le
   SQL est **déplacé**, pas réécrit. Le pattern dupliqué 5× (« lire
   `content`/`layer` par id + marquer `last_access` ») est factorisé en une
   seule méthode `hydrate`.
3. `Memory` (`memory/mod.rs`) garde son champ moteur typé concrètement
   (`Arc<LibsqlMemoryStore>`), mais toutes ses méthodes publiques
   (`remember*`, `recall*`, `invalidate`, `forget`, `purge_agent`, `stats`,
   `search_graph`) délèguent au trait `MemoryStore` plutôt qu'à du SQL inline.
   `Memory::engine()` (`pub(crate)`) expose une vue `Arc<dyn MemoryStore>`
   pour `Graph`/`consolidation`, par coercion non dimensionnée — pas de
   downcast, pas de `dyn Any`.
4. `Graph::new` prend désormais `Arc<dyn MemoryStore>` au lieu de
   `&basemyai_core::Store`. `MemoryStore` et `LibsqlMemoryStore` sont **publics**
   (`pub mod storage`) : `Graph::new` est un point d'entrée public déjà exercé
   directement par les tests d'intégration (`tests/graph.rs`), qui doivent
   pouvoir nommer le type du moteur sans accès `pub(crate)`.
5. `memory/porting.rs` (export/import JSONL) reste couplé au backend concret
   via `LibsqlMemoryStore::store()` (`pub(crate)`) : il lit des colonnes
   (`importance`, `last_access`) hors du contrat sémantique de `MemoryStore`,
   pour un usage de bas niveau (backup/restore complet) que le trait n'a pas
   vocation à porter en V1. Exclusion délibérée, au même titre que (6).
6. `crates/basemyai/src/maintenance/{gc.rs,forgetting.rs}` restent sur
   `&basemyai_core::Store` brut : ils passent par
   `basemyai_core::MaintenanceTask::run(&self, store: &Store)`, une signature
   qui appartient au core agnostique. Les migrer forcerait soit à changer
   cette signature (risque de réouvrir l'agnosticité du core), soit à
   instancier un `LibsqlMemoryStore` jetable par tâche pour un gain nul
   aujourd'hui.
7. Tests de contrat (`crates/basemyai/tests/storage_contract.rs`) : pilotés
   par `MemoryStore` directement (pas par `Memory`), pour rester valides
   verbatim contre une future implémentation autre que `LibsqlMemoryStore`.

## Conséquences

Positives :

- `memory/mod.rs`, `cognition/graph.rs` et `cognition/consolidation.rs` ne
  contiennent plus de SQL ni de `Filter`/`Value` — tout est concentré dans
  `storage/libsql_store.rs`.
- Aucune signature publique de `Memory` n'a changé ; les 5 sites appelants
  externes (`basemyai-mcp`, `basemyai-rest`, `basemyai-cli`,
  `bindings/basemyai-{py,node}`) n'ont nécessité **aucune modification**.
- Engagement de tests de contrat tenu (ADR-019, follow-up).

Compromis :

- `porting.rs` et `maintenance/{gc,forgetting}` restent couplés au backend
  concret. Si un second backend apparaît un jour, ces deux zones devront être
  revisitées (suivi possible, pas une régression introduite ici).
- `Memory` connaît le type concret `LibsqlMemoryStore` (pas seulement `dyn
  MemoryStore`) pour permettre à `porting.rs` d'accéder au `Store` brut sans
  `dyn Any`. Un second backend exigerait de généraliser ce point précis.

## Alternatives rejetées

**Scinder `MemoryStore` en plusieurs traits (`MemoryStore`/`GraphStore`…).**
Rejeté : un seul implémenteur prévu en V1, scinder maintenant serait une
abstraction sans second cas d'usage réel.

**`Memory::open_libsql` comme nouveau constructeur dédié.** Rejeté :
`Memory::open`/`Memory::new` prennent déjà un `basemyai_core::Store` (déjà
implicitement « le » backend libSQL) ; ajouter un constructeur parallèle
aurait été une API redondante pour zéro gain, les deux constructeurs
existants enveloppant déjà `LibsqlMemoryStore` en interne.

**Étendre `MemoryStore` aux besoins de `porting.rs` (export/import complet,
y compris `importance`/`last_access`).** Rejeté pour cette itération :
périmètre nettement plus large que le gap documenté dans `docs/status.md`,
et `porting.rs` est déjà isolé dans son propre fichier — un futur second
backend devra de toute façon revisiter le sujet.

## Suivi possible

- Si un second backend apparaît : généraliser `porting.rs` et
  `maintenance/{gc,forgetting}` derrière `MemoryStore` (ou un trait
  complémentaire dédié au backup/restore).
