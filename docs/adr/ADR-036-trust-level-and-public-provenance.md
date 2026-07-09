# ADR-036 — TrustLevel et provenance publique

**Statut** : ✅ Accepted  
**Date** : 2026-07-08  
**Relation** : complète ADR-035 (recall procedural, champ `source`) ; amends ADR-004
(types recall).

## Contexte

ADR-035 a introduit le champ wire `source` et l'exclusion procedural par défaut.
L'audit sécurité 2026-07-08 identifie encore :

1. **Provenance string libre** — pas d'enum stable pour filtrer ou afficher.
2. **Surfaces désalignées** — REST partiel, MCP/bindings sans `source` ni opt-in
   procedural explicite dans le schéma outil.
3. **Spoofing à l'import** — le champ `source` du JSONL était repris tel quel ;
   un export malveillant pouvait taguer `"user"` des souvenirs importés.
4. **Pas de filtre recall par provenance** — l'intégrateur devait post-traiter
   manuellement.

## Décision

### 1. `TrustLevel` (crate `basemyai`, public)

Enum `#[non_exhaustive]` :

| Variante | Tag wire `source` |
|---|---|
| `User` | `user` |
| `Consolidation` | `consolidation` |
| `Import` | `import` |
| `Unknown` | tout autre tag |

Constantes publiques : `SOURCE_USER`, `SOURCE_CONSOLIDATION`, `SOURCE_IMPORT`.

`Record::trust()` dérive la variante depuis `Record.source`.

**Sémantique** : `TrustLevel` = **provenance**, pas endorsement de sécurité.
Un recall `User` peut contenir du contenu hostile ; le consommateur LLM reste
responsable (cf. `docs/security/memory-poisoning.md`).

### 2. Anti-spoofing import

`import_jsonl*` **réécrit** toujours `source = "import"` pour les souvenirs
insérés, quel que soit le champ `source` du fichier — le tag exporté sert
d'audit dans le backup, pas de confiance à l'import.

### 3. `RecallOptions` étendu

```rust
pub struct RecallOptions {
    pub include_procedural: bool,  // défaut false (ADR-035)
    pub exclude_imported: bool,    // défaut false — post-filtre TrustLevel::Import
}
```

Post-filtre appliqué après recall vectoriel / hybride / métrique.

### 4. Surfaces alignées

- **REST** : `Record.source`, `Record.trust` (string), `include_procedural`,
  `exclude_imported` sur `RecallRequest` ; OpenAPI à jour.
- **MCP** : mêmes champs sur `RecallParams` / `RecallItem`.
- **Bindings Py/Node** : `Record.source`, `Record.trust` ; recall avec kwargs
  optionnels `include_procedural`, `exclude_imported`.
- **CLI** : `--include-procedural` sur recall (si applicable) ; `--trusted`
  inchangé sur import.

### 5. Hors scope (V2)

- `episode_ids[]` liant un fait sémantique à ses épisodes sources.
- Gate cryptographique sur `consolidate_apply`.
- Filtrage moteur par provenance (index dédié) — post-filtre mémoire suffit en V1.
- Explicabilité RRF (`Fused.contributions`, VISION §5.4).

## Conséquences

- **Breaking mineur bindings** : champs ajoutés sur `Record` (additive).
- **Breaking comportement import** : les souvenirs importés portent `import`,
  plus le tag du fichier — migration transparente pour les recalls (filtrage
  opt-in via `exclude_imported`).
- Tests CI : spoof import, import procedural sans `--trusted`, filtre
  `exclude_imported`.

## Alternatives rejetées

- Supprimer `source: String` au profit de `trust` seul — casse le wire natif
  et les exports JSONL existants.
- `min_trust` ordonné — fausses garanties (User ≠ sûr).
- Filtrage uniquement côté LLM — trop tardif pour procedural (ADR-035).
