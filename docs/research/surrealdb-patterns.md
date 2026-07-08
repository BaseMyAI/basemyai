# Patterns SurrealDB applicables à BaseMyAI

- Date : 2026-06-13
- Statut : **analyse / recommandations** (ce n'est pas un ADR — une décision retenue ici se matérialisera par un ADR dédié)
- Source : analyse des crates `analayse/surrealdb/surrealdb/{strand,server,src}` (fiches `CLAUDE.md` par module dans l'arbre surrealdb)

Ce document distille trois crates de SurrealDB en recommandations concrètes pour
BaseMyAI. Pour chaque pattern : ce que fait SurrealDB, où ça atterrirait chez
nous, et un **verdict** (Adopter / Adapter / Écarter) avec sa justification au
regard de nos invariants d'écosystème.

> Garde-fou transverse : `basemyai-core` est **agnostique métier** (ni `agent_id`,
> ni couche mémoire, ni temps). Tout pattern « sens » va dans `basemyai` ou les
> bindings, jamais dans le core. Mécanisme au core, sens au consommateur.

---

## 1. `strand` — type string immuable à SSO (small-string optimisation)

### Ce que c'est

Un type `Strand` de **24 octets** (taille de `String`) avec trois stockages
choisis à la construction, et un format binaire **identique octet-pour-octet à
`String`** (donc remplaçable à chaud sans casser le on-disk) :

- **Inline** (≤ 23 octets) — sur la pile, zéro alloc, `clone` = copie bitwise.
- **Static** (`&'static str`, `const`) — zéro alloc *quelle que soit la longueur*,
  `clone` = copie de pointeur, `drop` = no-op. Idéal pour les littéraux connus à
  la compilation (noms de tables, clés de réponse, mots-clés).
- **Boxed** (`Box<str>`) — chaînes longues dynamiques, une alloc.

L'octet 23 sert de **tag + longueur** : pour Inline il porte la longueur
(`0..=23`), pour Static/Boxed c'est `254`/`255`. `as_str()` est **sans
branche**, et l'égalité de deux Inline compare directement les 24 octets (un
seul compare SIMD-friendly).

### Où ça nous concerne

Le moteur mémoire manipule en masse des **petites chaînes répétées** :
- clés/IDs d'enregistrements mémoire,
- `AgentId` (newtype d'isolation, `memory/isolation.rs`),
- noms de couches (`MemoryLayer`), labels d'entités/arêtes du graphe
  (`cognition/graph.rs`),
- mots-clés SQL et fragments de `Filter` paramétré (`storage/vector.rs`).

Beaucoup sont soit **courts**, soit **connus à la compilation** — exactement les
deux cas où `Strand` supprime l'allocation.

### Verdict : **Adapter — ciblé, pas global, et pas en V1**

- **Pour** : gains réels sur `clone`/`eq`/`cmp` des clés et labels chauds ; la
  variante `Static` élimine des allocations sur les constantes (noms de couches,
  fragments SQL). Aligné avec notre règle `clone_on_ref_ptr`/perf.
- **Contre / nuances** :
  - Notre backend est **libSQL**, pas un value-layer maison. La valeur de
    `Strand` vient de son intégration aux formats `revision`/`storekey` de
    SurrealDB ; chez nous les chaînes transitent surtout par des `params` SQL
    liés et des embeddings, **pas** un format binaire propriétaire. L'invariant
    « byte-identique à String » qui justifie 700 lignes d'`unsafe` ne s'applique
    pas à nous.
  - ~700 lignes d'`unsafe` (union, `unreachable_unchecked`, `Drop` manuel) à
    maintenir et fuzzer. Notre politique lib interdit `unwrap()` et le code
    `unsafe` non justifié : l'adoption exigerait le même niveau de tests que
    SurrealDB (`*_wire_matches_string`, fuzz `arbitrary`).
- **Recommandation** :
  1. **Court terme** : récolter le bénéfice à coût nul via des crates éprouvées —
     `compact_str` (SSO 24 o, API `String`) ou `Arc<str>` pour les labels
     partagés. Pas d'`unsafe` chez nous.
  2. Remplacer les `String` constants par des `&'static str` / `Cow<'static, str>`
     pour les noms de couches, mots-clés et fragments SQL (capte le gain
     « Static » sans le type).
  3. **N'envisager un `Strand` maison que si** un profilage (flamegraph via le
     profil `profiling`) montre que `clone`/`cmp` de petites chaînes domine un
     chemin chaud (recall, RRF, traversée de graphe). Décision → ADR dédié.
- **Invariant** : un tel type irait dans `basemyai-core` (mécanisme pur,
  agnostique) — jamais de sémantique mémoire dedans.

---

## 2. `server` — composition de routeur + séparation build/serve (pour `basemyai-rest`)

### Ce que c'est

Le serveur Axum de SurrealDB sépare proprement trois choses :

1. **Composition des routes via un trait** (`RouterFactory` + `CommunityComposer`) :
   les routes ne sont pas codées en dur dans le démarrage ; un composer les
   assemble par `.merge()`, donc on peut ajouter/retirer/wrapper sans forker.
2. **`build` (construit le routeur configuré) vs `init` (bind socket + serve)** :
   `SurrealRouter::build()` applique toute la pile middleware + l'état et rend un
   `axum::Router` *embarquable* (`into_router()`), sans toucher au socket.
   `init()` est le chemin tout-en-un du binaire.
3. **Pile middleware tower** explicite et ordonnée : `catch_panic` →
   request-id → `concurrency_limit` (backpressure) → compression → auth →
   CORS → trace/metrics, chaque couche étant **retirable**.
4. **Arrêt gracieux par `CancellationToken`** propagé à toutes les tâches
   spawnées (notifications LIVE incluses) + `Datastore::shutdown()`.

### Où ça nous concerne

Notre **sidecar REST `basemyai-rest`** (cf. `sdk-surfaces-status.md`, profil
`profiling` dédié dans le `Cargo.toml` racine) est précisément un serveur HTTP
au-dessus du moteur mémoire. C'est le miroir direct de `server/`.

### Verdict : **Adopter (à l'échelle), pour `basemyai-rest`**

- **Pile middleware tower** : reprendre l'ordre et l'esprit — `catch_panic`,
  `concurrency_limit` (backpressure, cohérent avec le `Store` async),
  request-id, headers sensibles obfusqués (jamais de credential en clair dans
  les traces — aligné avec notre règle « ne jamais logger de données
  sensibles »), CORS. **Adopter.**
- **Séparation `build`/`serve`** : exposer `build_router(...) -> axum::Router`
  distinct du `serve(addr)`. Permet de tester le routeur sans socket et
  d'embarquer le sidecar dans un hôte tiers (ex. une extension VSCode).
  **Adopter.**
- **Arrêt gracieux par `CancellationToken`** : on a déjà un `MaintenanceWorker`
  à tâches injectées ; un token d'annulation propagé au worker **et** au serveur
  HTTP donne un arrêt propre (flush, fermeture libSQL). **Adopter.**
- **`RouterFactory` (composer générique)** : **Adapter / différer.** SurrealDB en
  a besoin pour ses éditions community/enterprise. Nous n'avons qu'une surface
  REST ; un trait de composition est de la sur-ingénierie tant qu'il n'y a pas
  deux compositions de routes. Garder les `router()` par module
  (`memory::router()`, `graph::router()`…) mergés dans une fonction simple ;
  promouvoir en trait seulement si un besoin d'édition apparaît.
- **LIVE queries / fan-out de notifications** : **Écarter en V1.** Pas de cas
  d'usage temps-réel mémoire ; complexité (registre `RwLock<HashMap>`,
  équilibrage de gauge) non justifiée.

> Le sidecar reste une **surface** : la sémantique vit dans `basemyai`, le HTTP
> n'est qu'un transport. Ne pas laisser fuiter de logique mémoire dans les
> handlers.

---

## 3. `src` (SDK) — routeur acteur + enum `Command` + type-state (pour les bindings)

### Ce que c'est

Le SDK Rust expose **une** API (`Surreal<C>`) qui pilote tous les moteurs
(embarqué, distant, navigateur) :

- **Routeur acteur par canal** : chaque méthode sérialise l'appel en une variante
  de l'enum **`Command`** et la poste sur un `async_channel` ; une tâche moteur
  possède le `Receiver`, exécute, et répond sur un canal de réponse par-requête.
  L'API publique est **découplée** de l'exécution.
- **`Command` = enum fermé** : *toute* la capacité du client est un seul type
  revuable (`Query`, `Create`, `Set`, `Authenticate`, `SubscribeLive`…), pas des
  méthodes de trait éparpillées.
- **Type-state `Surreal<C>`** + trait scellé `Connection` : le backend est choisi
  à la compilation ; un backend non activé = **erreur de compilation**.
- **Builders `IntoFuture`** : `db.create(..).content(..).await` — fluide, paresseux.
- **Moteur `Any`** : choix du backend au runtime selon le schéma de l'URL.

### Où ça nous concerne

BaseMyAI a **4 surfaces SDK** (MCP / PyO3 / NAPI / REST, cf.
`sdk-surfaces-status.md`) qui exposent *le même* moteur mémoire. C'est exactement
le problème que ce SDK résout : une logique, plusieurs façades.

### Verdict : **Adapter — le `Command` comme contrat partagé des bindings**

- **Enum `Command` / `MemoryOp` partagé** : définir dans `basemyai` un enum
  fermé décrivant les opérations mémoire (`Remember`, `Recall { query, k, filter }`,
  `RecallByLayer`, `Invalidate`, `Forget`, `Stats`, `SearchGraph`, `Consolidate`).
  Les 4 bindings deviennent de **fins traducteurs** (JSON-RPC MCP, args PyO3, JS
  NAPI, JSON REST) → `MemoryOp` → `Memory`. Un seul point de revue pour la
  surface, déduplication des 4 façades. **Adopter** — gros gain de cohérence,
  c'est notre vrai analogue. (Voir `mcp-blueprint.md` / `type-mapping.md` qui
  vont déjà dans ce sens.)
- **Routeur acteur par canal** : **Écarter / inutile.** SurrealDB en a besoin
  pour unifier embarqué *et* distant *et* wasm derrière une API sync-looking et
  gérer la reconnexion. Notre `Memory`/`Store` est **déjà async** (tokio) et
  in-process : un `Arc<Memory>` partagé entre bindings suffit. Ajouter un canal
  acteur n'apporterait que de la latence et de l'indirection. Garder l'appel
  direct `Arc<Memory>`.
- **Type-state pour le backend** : **Écarter en V1.** Un seul backend (libSQL,
  ADR-011) et un modèle baseline unique (compat `.idx`). Le multi-backend est V2+
  (chemin Turso) ; le type-state `Surreal<C>` se justifiera alors, pas avant.
- **Builders `IntoFuture`** : **Adapter, optionnel.** Sympa pour l'ergonomie de la
  crate `basemyai` côté Rust (`memory.recall("q").k(8).layer(..).await`), sans
  valeur pour les bindings non-Rust. À faire si l'API publique Rust gagne en
  fluidité, pas une priorité.
- **Moteur `Any`** : **Écarter** tant qu'il n'y a qu'un backend.

---

## Synthèse

| Pattern (source) | Cible BaseMyAI | Verdict | Priorité |
|---|---|---|---|
| `Strand` SSO complet | `basemyai-core` (clés/labels) | **Adapter** — d'abord `compact_str`/`&'static str`, type maison seulement si le profilage l'exige | Basse (post-profilage) |
| `&'static str`/`Cow` sur constantes | core + basemyai | **Adopter** — gain « Static » sans `unsafe` | Basse, facile |
| Pile middleware tower | `basemyai-rest` | **Adopter** | Moyenne (au scaffolding REST) |
| Séparation `build`/`serve` | `basemyai-rest` | **Adopter** | Moyenne |
| Arrêt gracieux `CancellationToken` | `basemyai-rest` + `MaintenanceWorker` | **Adopter** | Moyenne |
| `RouterFactory` (composer) | `basemyai-rest` | **Différer** — pas avant 2 compositions | — |
| Enum `Command`/`MemoryOp` partagé | `basemyai` + 4 bindings | **Adopter** — notre vrai analogue | **Haute** |
| Routeur acteur par canal | — | **Écarter** — `Arc<Memory>` async suffit | — |
| Type-state backend / moteur `Any` | — | **Écarter en V1** (1 seul backend libSQL) ; revoir en V2 (Turso) | — |
| LIVE queries / fan-out notif | — | **Écarter** (pas de temps-réel mémoire en V1) | — |

### Prochain pas suggéré

Le plus rentable et le plus aligné avec l'existant (`mcp-blueprint.md`,
`type-mapping.md`, `sdk-surfaces-status.md`) est l'**enum `MemoryOp` partagé** :
il déduplique les 4 surfaces et donne un contrat unique revuable. S'il est
retenu, le matérialiser en **ADR** (le contrat d'opérations mémoire est une
décision d'architecture publique), puis brancher les bindings dessus.
