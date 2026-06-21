# ADR-022 — `MemoryEvent` : abonnements mémoire en direct via canal tokio broadcast

**Status**: Accepted
**Date**: 2026-06-21
**Context**: suivi de PLAN.md §P2.1 (« Live queries / subscriptions ») ;
**étend ADR-006** (isolation multi-agent par `agent_id`) **au plan
événementiel** ; n'amende ni ne supersède ADR-001 (agnosticité du core) ni
ADR-011 (backend libSQL V1).

## Contexte

Les agents réactifs doivent **réagir à un changement de mémoire** — un autre
agent (ou un autre process, ou le worker de consolidation) met à jour un fait,
l'agent qui le consomme doit re-planifier — **sans interroger en boucle** la
base. Le polling est coûteux, ajoute de la latence, et défait le principe même
de réactivité.

Le besoin est **convergent** sur deux documents indépendants :

- **PLAN.md §P2.1** spécifie déjà l'API cible
  (`async for event in memory.watch(agent_id, layer)`) et le mécanisme
  pressenti : « canal tokio broadcast dans `basemyai` ».
- **`docs/surrealdb-gap-analysis.md`** classe le gap #6 (« `MemoryEvent`
  broadcast ») en **P1** — révision motivée du verdict précédent
  (`surrealdb-patterns.md` §2 l'avait écarté en V1) : le cas d'usage existe dès
  qu'il y a **deux** consommateurs de la même mémoire. Le document précise que
  BaseMyAI n'est **pas** un broker généraliste : un `tokio::sync::broadcast`
  émis par la façade `Memory`, avec les variantes
  `Remembered`/`Invalidated`/`Forgotten`/`Consolidated`.

Cette ADR **fixe le mécanisme in-process et le contrat d'isolation** ; les
surfaces (REST/MCP/bindings) sont une seconde vague, explicitement reportée
(voir « Suivi possible »).

## Décision

1. **Le canal vit sur `Memory`, dans la crate `basemyai` — PAS
   `basemyai-core`.** Un `tokio::sync::broadcast::Sender<MemoryEvent>` est porté
   par la façade `Memory`. Le **pourquoi** : `MemoryEvent` connaît `agent_id`,
   les couches mémoire (`MemoryLayer`) et la sémantique de mutation — exactement
   ce qu'ADR-001 **interdit** au core agnostique. Le mécanisme « canal
   broadcast » serait certes générique, mais le *sens* (qui réagit à quoi, par
   agent et par couche) est métier ; il reste donc côté consommateur, dans
   `basemyai`.

2. **Type de l'événement.** `Memory` émet après chaque mutation committée :

   ```rust
   pub struct MemoryEvent {
       pub agent_id: AgentId,
       pub kind: MemoryEventKind,
       pub layer: MemoryLayer,
       pub id: RecordId,
   }

   pub enum MemoryEventKind {
       Remembered,
       Invalidated,
       Forgotten,
       Consolidated,
   }
   ```

   Le payload reste **minimal** (identité de l'enregistrement + nature du
   changement), pas le contenu : l'abonné qui veut le détail rappelle la mémoire
   par `id`. Cela évite de transformer le canal en réplication de données et
   garde l'événement bon marché à diffuser.

3. **Abonnement.** Les consommateurs s'abonnent via
   `Memory::watch(agent_id, layer_filter)`, qui rend un flux (`Subscription`)
   ne livrant que les événements pertinents (voir §isolation). Le
   `layer_filter` optionnel restreint en plus à une couche
   (`Some(MemoryLayer::Semantic)` → seulement la couche sémantique ; `None` →
   toutes).

## Émission **après commit** uniquement

Un `MemoryEvent` n'est publié **qu'après** que la transaction d'écriture
sous-jacente a **committé** — jamais pour une écriture annulée (rollback).
Concrètement, l'émission est placée **après** le retour réussi de la transaction
`begin_write`/commit (ADR : atomicité écritures, gap #1), pas avant.

**Garantie d'ordre / d'observabilité** : si un abonné reçoit
`MemoryEvent { id, kind }`, alors un `recall`/`get` ultérieur sur cet `id`
**observe l'état committé** correspondant (l'écriture est durable au moment où
l'événement part). L'inverse est garanti aussi : **aucun** événement n'est émis
pour une écriture qui a échoué ou a été rollback. Pas de fantômes, pas de
fuite d'état non committé.

## L'isolation est un invariant dur (extension d'ADR-006 au plan événementiel)

ADR-006 isole les agents par `agent_id` au niveau **données** (filtre SQL,
aucune mémoire partagée en V1). Cette ADR **étend la même garantie au plan
événementiel** :

- Un abonnement pour l'agent A reçoit **uniquement** les événements dont
  `agent_id == A`. Les événements des autres agents sont **filtrés
  côté serveur, à l'intérieur de la `Subscription`** — jamais livrés au mauvais
  tenant.
- Le canal `tokio::sync::broadcast` transporte **physiquement** tous les
  événements de tous les agents (il est in-process, partagé). Le filtrage par
  `agent_id` (et `layer_filter`) est appliqué **dans la `Subscription` elle-même**,
  avant de rendre l'événement à l'appelant. L'isolation **n'est jamais déléguée
  à l'appelant** : la bibliothèque la garantit, on ne peut pas « oublier de
  filtrer ».
- **Attente de test adversarial** : un test doit prouver que les écritures de
  l'agent **B** sont **invisibles** à l'abonnement de l'agent **A** — A subit
  une rafale de `remember`/`forget` émise au nom de B et ne reçoit **aucun**
  événement. C'est le pendant événementiel du test d'isolation données d'ADR-006.

## Sémantique des abonnés lents (`Lagged`)

`tokio::sync::broadcast` a une capacité **bornée** et **abandonne les messages
les plus anciens** pour un récepteur trop lent (`RecvError::Lagged(n)`). Le
choix est délibéré : un abonné lent ne doit **jamais** faire grossir la mémoire
sans borne ni bloquer les écrivains.

- **Capacité par défaut : 1024** messages (documentée, ajustable en
  configuration si la charge le justifie).
- La `Subscription` **tolère `Lagged` gracieusement** : elle **saute le trou**
  (les `n` événements perdus), **ne panique pas**, et **continue** à consommer.
  Le retard est exposé comme un **signal non fatal** (l'abonné peut, s'il le
  souhaite, déclencher un re-`recall` complet de réconciliation), pas comme une
  erreur terminale qui ferme le flux.

## Surfaces : reportées à une tranche de suivi (hors de cette ADR)

Cette ADR ne livre **que** le mécanisme in-`basemyai` + le contrat d'isolation.
Les surfaces qui se construisent **par-dessus** ce cœur sont une **seconde
vague**, non implémentée ici :

- **REST** : un endpoint SSE (ou WebSocket) qui relaie le flux d'un agent.
- **MCP** : notifications serveur (`notifications/*`) sur événement mémoire.
- **PyO3 / NAPI** : callbacks / async iterator côté Python et Node
  (l'API `async for event in memory.watch(...)` de PLAN.md §P2.1).

Toutes consomment le même `Memory::watch` et héritent **gratuitement** de
l'isolation par `agent_id` : elles ne refont pas le filtrage, elles passent
l'`agent_id` du tenant à `watch`.

## Conséquences

Positives :

- Réactivité réelle sans polling : un agent re-planifie sur événement, le
  `MaintenanceWorker` peut déclencher la consolidation **à l'écriture
  d'épisodes** plutôt qu'à intervalle fixe (moins de réveils à vide, meilleure
  latence — cf. surrealdb-gap-analysis §6).
- Isolation d'ADR-006 **étendue et garantie en bibliothèque** au plan
  événementiel ; les surfaces futures en héritent sans la réimplémenter.
- Émission après commit : pas d'état fantôme, ordre observable bien défini.

Compromis :

- **In-process uniquement** : le canal vit dans **une** instance de moteur. Les
  abonnés doivent partager le même process que `Memory`. L'**abonnement
  multi-process est hors scope V1** — une future surface réseau (le SSE/WS REST
  de la tranche de suivi) **pontera** le flux entre process, mais le cœur reste
  in-process.
- Sémantique **at-most-once avec pertes possibles** sous charge (`Lagged`) : ce
  n'est **pas** un journal d'événements durable. Un abonné qui exige zéro perte
  doit réconcilier par `recall` après un signal `Lagged`. (Les change feeds
  persistés sont **délibérément écartés** — surrealdb-gap-analysis : `Validity`
  valid_from/until + audit couvrent déjà l'historique métier.)

## Alternatives rejetées

**Polling périodique de la mémoire.** Rejeté : coûteux (réveils à vide,
requêtes répétées), haute latence (granularité = intervalle de poll), et défait
l'objectif même de réactivité. Le besoin explicite de PLAN.md §P2.1 est le
push, pas le pull.

**Pub/sub au niveau base (LISTEN/NOTIFY, triggers).** Rejeté : libSQL/SQLite
n'offre **pas** de mécanisme de pub/sub robuste (pas de `LISTEN/NOTIFY` façon
Postgres). Des triggers SQL bricolés seraient fragiles, non typés, et
contourneraient la façade `Memory` (donc l'isolation et la sémantique des
couches). Le canal tokio vit là où vit déjà le sens : dans `basemyai`.

**Mettre les événements dans `basemyai-core`.** Rejeté : **viole ADR-001**. Le
core est agnostique métier — il ne connaît ni `agent_id`, ni les couches
mémoire, ni la notion d'« événement de consolidation ». `MemoryEvent` est
intrinsèquement métier ; le placer au core forcerait à y faire entrer
`agent_id`/`MemoryLayer`, ce que le test d'agnosticité interdit.

## Suivi possible

- **Tranche surfaces** (seconde vague) : SSE/WebSocket REST, notifications MCP,
  callbacks PyO3/NAPI — tous par-dessus `Memory::watch`.
- **Réveil de consolidation sur événement** : brancher le `MaintenanceWorker`
  sur le flux `Remembered{ layer: Episodic }` pour déclencher
  `ConsolidationTask` à l'écriture plutôt qu'au timer.
- Capacité du canal / `layer_filter` exposés en configuration si la charge réelle
  le justifie.
- Si un besoin multi-process ferme apparaît : pont réseau durable (au-delà du
  relais SSE best-effort), à acter dans un ADR dédié.
