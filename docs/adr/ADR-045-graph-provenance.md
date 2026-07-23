# ADR-045 — Provenance et niveau de confiance pour le graphe (AGENT-MEM-1)

**Statut** : 🟡 Proposed — conçu et spécifié ci-dessous, **implémentation non
commencée**. Voir « Pourquoi ce correctif n'est pas encore implémenté » en
fin de document.
**Date** : 2026-07-23
**Relation aux ADR existants** : étend au graphe (`idx::graph`, N4) la
discipline de provenance qu'ADR-036 (`TrustLevel`, provenance publique,
anti-spoofing import) a déjà posée pour les souvenirs. N'amende ni ADR-027
(mémoire native) ni N4 (graphe, port du CTE récursif) — le mécanisme de
traversée BFS et le stockage KV du graphe restent inchangés ; seul le champ
de contenu de `GraphEntity`/`GraphEdgeMeta` gagne un attribut nouveau.

## Contexte

Vérifié dans le code réel (audit adversarial BaseMyAI, 2026-07-22,
finding AGENT-MEM-1) :

- `GraphEntity` (`crates/basemyai-engine/src/idx/graph/entity.rs`) :
  `{ kind, label, valid_from, valid_until }` — aucun champ de provenance.
- `GraphEdgeMeta` (`idx/graph/edge.rs`) : `{ weight, valid_from, valid_until }`
  — même absence.
- `Graph::add_entity`/`add_entity_with`/`add_edge`
  (`crates/basemyai/src/cognition/graph.rs`) ne prennent aucun paramètre de
  provenance.
- `apply_extraction` (`cognition/consolidation.rs`) — le point où le graphe
  est peuplé depuis une extraction LLM sur un épisode potentiellement
  adversarial — n'attache aucune trace de provenance à ce qu'elle écrit.
- `import_rows` (`storage/native_store/porting.rs`) réimporte des
  entités/arêtes sans tag équivalent à `SOURCE_IMPORT` (le tag que
  `memory/porting.rs` applique déjà systématiquement aux souvenirs, ADR-036).
- `recall_graph_filtered` (`storage/native_store/trait_impl.rs:300-358`)
  utilise **tous** les labels d'entités valides de l'agent, sans distinction
  de source, pour influencer/booster le classement du recall vectoriel.

Le même système applique déjà `TrustLevel`/`Record.source` de bout en bout
côté mémoire (stocké dans `MemoryRecord.source`, interprété par
`TrustLevel::from_source`, appliqué par défaut via
`ContextSourcePolicy::ExcludeImported`) — le graphe est la seule structure de
contenu de ce système sans équivalent. Un label empoisonné (extraction
manipulée d'un épisode adversarial, ou réimport d'un export forgé) influence
donc silencieusement et durablement le classement du recall pour toute
requête future de l'agent concerné, sans qu'aucun mécanisme d'exclusion ne
soit disponible.

## Décision

### 1. Champs ajoutés

```rust
pub struct GraphEntity {
    pub kind: String,
    pub label: String,
    pub valid_from: i64,
    pub valid_until: Option<i64>,
    pub source: GraphSource,        // NOUVEAU
}

pub struct GraphEdgeMeta {
    pub weight: f32,
    pub valid_from: i64,
    pub valid_until: Option<i64>,
    pub source: GraphSource,        // NOUVEAU
}

/// Même famille de valeurs que `basemyai::memory::trust::TrustLevel`
/// (ADR-036) — réutilisée telle quelle plutôt que dupliquée, pour que la
/// même politique d'exclusion (`ContextSourcePolicy`) s'applique
/// identiquement aux deux structures de contenu.
pub enum GraphSource {
    User,           // créé directement par l'utilisateur/l'agent appelant
    Consolidation,  // extrait par le pipeline de consolidation LLM (ADR-012)
    Import,         // réimporté depuis un export JSONL (anti-spoof, ADR-036 §"import")
    Inferred,       // dérivé par une règle interne sans passage LLM
}
```

`GraphSource` reprend la structure de `TrustLevel` (ADR-036) plutôt que d'en
créer une seconde taxonomie parallèle — le graphe et la mémoire partagent la
même notion de confiance, seul le support diffère.

### 2. Propagation obligatoire à chaque point d'écriture

| Producteur | `source` attribué |
|---|---|
| `Graph::add_entity`/`add_edge` appelé directement par un handler REST/MCP (écriture utilisateur explicite) | `GraphSource::User` |
| `apply_extraction` (consolidation LLM sur un épisode) | `GraphSource::Consolidation` |
| `import_rows` (réimport JSONL) | `GraphSource::Import` — **toujours**, y compris si le fichier importé prétend une autre source (même anti-spoof qu'ADR-036 pour les souvenirs) |
| Une future règle d'inférence interne sans LLM | `GraphSource::Inferred` |

`Graph::add_entity`/`add_edge` gagnent un paramètre `source: GraphSource`
(défaut `User` pour la compatibilité des appelants existants qui ne gèrent
pas encore la distinction — jamais un défaut silencieux `Consolidation` ou
`Import`, qui masquerait exactement le risque que ce document corrige).

### 3. Politique de filtrage au recall

`recall_graph_filtered` (`storage/native_store/trait_impl.rs`) gagne un
paramètre optionnel, symétrique à `ContextSourcePolicy` côté mémoire :
exclut par défaut les entités `GraphSource::Import` du filtre de labels
utilisé pour influencer le classement du recall vectoriel — un appelant qui
a explicitement besoin d'inclure du contenu importé (ex. un outil
d'administration) le demande explicitement, jamais par défaut.

### 4. Format wire — bump requis

`GraphEntity`/`GraphEdgeMeta` sont des enregistrements KV encodés/décodés
(`idx/graph/entity.rs`, `edge.rs`) avec leurs propres constantes de version
(`UnsupportedGraphEntityVersion`/`UnsupportedGraphEdgeVersion` existent déjà
comme erreurs typées). Ajouter `source` est un bump de version mineur
(nouveau champ de taille fixe, 1 octet suffit pour l'énumération) —
`format.lock` gagne les entrées mises à jour, deux nouvelles fuzz targets
(`graph_entity_decode`/`graph_edge_decode` existent déjà — elles couvrent le
nouveau format par construction une fois le decoder mis à jour, pas de
nouvelle cible nécessaire).

Même posture pré-1.0/natif-only qu'ADR-044 (WAL v2) §7 : refus typé d'un
ancien format plutôt qu'une tolérance silencieuse ; pas de migration
automatique d'un store existant tant qu'aucun besoin réel n'est démontré.

## Alternatives rejetées

- **Stocker la provenance hors du graphe (table séparée, `entity_id →
  source`)** : rejeté — duplique la clé primaire, introduit une
  désynchronisation possible (l'entité existe, sa provenance non, ou
  inversement) qu'un champ inline sur l'enregistrement lui-même élimine par
  construction.
- **Une taxonomie de confiance distincte de `TrustLevel`** : rejeté — le
  graphe et la mémoire répondent à la même question (« puis-je faire
  confiance à ce contenu pour influencer une décision ? »), une seconde
  taxonomie parallèle serait une source de divergence future sans bénéfice.

## Pourquoi ce correctif n'est pas encore implémenté

Même arbitrage qu'ADR-044 (§ »Pourquoi ce correctif n'est pas encore
implémenté ») : un bump de format sur une structure aussi centrale que le
graphe mérite son propre cycle de tests (roundtrip, compatibilité de
version, migration explicite) plutôt qu'une implémentation pressée en fin
de session de remédiation. Sévérité du finding sous-jacent (Medium — pas de
fuite cross-agent, impact borné au classement du recall au sein d'un même
agent) inférieure à celle des correctifs déjà livrés (DUR-LSM-01 P0,
CRYPTO-1 High), justifiant cet ordre de priorité.

## Critères de sortie (avant de passer ce statut à Accepted)

- [ ] `GraphSource` implémenté, `format.lock` mis à jour.
- [ ] `Graph::add_entity`/`add_edge`/`apply_extraction`/`import_rows` propagent
  `source` correctement — test couvrant chaque producteur.
- [ ] `recall_graph_filtered` exclut `GraphSource::Import` par défaut — test
  de régression reproduisant le scénario AGENT-MEM-1 (label empoisonné via
  import, absent du classement par défaut).
- [ ] Réimport d'un export JSONL contenant une entité prétendant
  `source=User` est re-tagué `Import` — test anti-spoof, même discipline
  qu'ADR-036 pour les souvenirs.
