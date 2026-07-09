# Memory poisoning — mitigations

## Menace

Un acteur écrit du contenu hostile (prompt injection, instructions système
usurpées) dans la mémoire d'un agent. Lors d'un recall ultérieur, ce contenu
est injecté dans le contexte du LLM consommateur.

## Mitigations BaseMyAI

### 1. Couche `procedural` exclue par défaut (ADR-035)

Les souvenirs `MemoryLayer::Procedural` (workflows, playbooks internes) ne
sortent **pas** dans `recall()`, `recall_hybrid()` ni la recherche vectorielle
générale. Opt-in explicite :

```rust
memory.recall_with_options(
    query,
    k,
    RecallOptions {
        include_procedural: true,
        ..Default::default()
    },
).await?;
```

API REST / MCP : `include_procedural` sur les requêtes recall.

### 2. Provenance typée `TrustLevel` (ADR-036)

Chaque [`Record`](../adr/ADR-004-four-memory-layers.md) expose :

- `source: String` — tag wire persisté (`user`, `consolidation`, `import`, …)
- `trust()` / champ `trust` REST-MCP — enum stable pour filtrage et affichage

**Important** : `TrustLevel::User` n'est **pas** un endorsement de sécurité ;
un texte mémorisé directement peut être hostile. L'intégrateur filtre avant
de faire confiance.

Filtre recall optionnel :

```rust
RecallOptions {
    exclude_imported: true, // exclut TrustLevel::Import
    ..Default::default()
}
```

### 3. Import JSONL non fiable

- Lignes `procedural` refusées sans `--trusted` (CLI) ou `trusted: true`.
- Tout souvenir importé est **re-tagué** `source = "import"` (anti-spoofing :
  le champ `source` du fichier exporté n'est pas repris tel quel).

### 4. Autres garde-fous

- Isolation `agent_id` (pas de poisoning cross-tenant via recall).
- Validité temporelle (`valid_until`) + `invalidate`.
- Bornes sur `consolidate_apply` (taille des entités/relations extraites).
- Anti-injection dans le prompt de consolidation (épisodes délimités UUID).

## Tests CI

```bash
cargo test -p basemyai --features test-util --test poisoning_procedural_recall
cargo test -p basemyai --features test-util --test provenance_trust
```

## Responsabilité LLM

Les embeddings mesurent la **pertinence**, pas la **confiance**. Le modèle
consommateur reste responsable de traiter le contexte rappelé comme non fiable
par défaut.

## Hors scope V1 (V2)

- `episode_ids[]` liant un fait sémantique à ses épisodes sources.
- Gate cryptographique sur `consolidate_apply`.
- Explicabilité RRF (`Fused.contributions`).
