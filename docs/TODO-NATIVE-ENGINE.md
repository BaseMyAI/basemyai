# TODO — Moteur natif BaseMyAI

Backlog du chantier acté par `ADR-024-native-engine.md`, séquencé par
`PLAN-NATIVE-ENGINE.md` §5. Jalons N0→N6. Une case ne se coche que si le
critère de sortie est vérifié (test/CI/chiffre), pas « le code existe ».

## N0 — Chantier 0 : DX + organisation repo (préalable, Phase 0)

Constat : audit d'organisation 2026-07-02 (2 agents, référence SurrealDB).

### DX

- [x] `xtask` (crate workspace + alias `.cargo/config.toml`) encodant
  **exactement** la matrice CI (`ci.yml`) : `cargo xtask check` (fmt + clippy
  par-crate avec les vraies combinaisons de features), `cargo xtask test`,
  `cargo xtask test-embed`, `cargo xtask test-crypto`, `cargo xtask ci`.
  Choix xtask plutôt que justfile/cargo-make : zéro outil à installer (cargo
  suffit), pur Rust (2026-07-02)
- [x] `CONTRIBUTING.md` et `CLAUDE.md` pointent vers les cibles `cargo xtask`
  au lieu de commandes `--workspace` qui ne reproduisent pas la CI (2026-07-02)
- [ ] (optionnel, plus tard) faire appeler les mêmes cibles par `ci.yml`
  (single source of truth)

### Assainissement docs (P0/P1 de l'audit)

- [x] Une seule source de statut : items ouverts de `TODO.md` (racine) fusionnés
  dans `docs/status.md`, `TODO.md` racine supprimé, `docs/TODO.md` archivé
  sous `docs/archive/` (2026-07-02)
- [x] Un seul changelog : contenu FR non reporté de `docs/CHANGELOG.md` remonté
  dans `CHANGELOG.md` racine, puis `docs/CHANGELOG.md` supprimé (2026-07-02)
- [x] `AGENTS.md` réduit à un pointeur vers `CLAUDE.md` ; « deux crates »
  périmé corrigé dans `CLAUDE.md` (2026-07-02)
- [x] `.agents/skills/` (copie octet-à-octet de `.claude/skills/`) supprimé
  (identité vérifiée par diff avant suppression, 2026-07-02)
- [x] `openapi-sidecar.yaml` déplacé vers `crates/basemyai-rest/openapi.yaml`,
  références corrigées (dont le chemin fautif `analayse/openapi-sidecar.yaml`
  dans `basemyai-rest/src/lib.rs`) ; mentions restantes dans `docs/*.md`
  laissées à la passe docs (2026-07-02)
- [x] `docs/ANALYSIS.md` (snapshot git périmé) supprimé (2026-07-02)
- [x] Analyses ponctuelles déplacées sous `docs/research/` : `surrealdb-*.md`
  (×5), `type-mapping.md`, `mcp-blueprint.md` (2026-07-02)

### Reporté (P2 — décisions séparées, ne pas faire en passant)

- [ ] Éclater `docs/ADR.md` (monolithe 001–018) en `docs/adr/ADR-0XX-*.md`
- [ ] `bindings/` → `crates/` (ou documenter la règle de séparation)
- [ ] `basemyai-branding/` → `docs/branding/` ou repo séparé
- [ ] `examples/README.md` expliquant racine (SDK par langage) vs
  `crates/basemyai/examples/` (Rust)

## N1 — Spike Couche 1 (Phase 0b) ✅ clos le 2026-07-04

- [x] Vérifier le statut de maintenance 2026 de `redb`, `fjall`, `sled`
  (versions, activité, prod-readiness) — ne pas assumer (2026-07-04 : `redb`
  4.1 actif/mûr, `fjall` 3.1 maintenu mais développement de features en net
  ralentissement, `sled` écarté sans étude approfondie)
- [x] Prototype jetable A : B-tree copy-on-write (lecture concurrente, GC pages)
  — 107 296 inserts/s, 8,7 µs/lecture, ×14,3 amplification espace (pas de
  free-list dans le spike), 10/10 PASS crash-consistency
- [x] Prototype jetable B : LSM-tree (write-path, compaction, change-capture)
  — 435 894 inserts/s, 3,52 µs/lecture, ×1,05 amplification, 10/10 PASS
  crash-consistency
- [x] Comparatif écrit : perf write/read, complexité de crash recovery,
  aptitude au change-capture (pour sync P2P futur) —
  `docs/benchmarks/n1-storage-engine-spike-2026-07-04.md`
- [x] Décision fondation-maison vs fork/dépendance consciente d'un KV Rust
  (critère : propriété des couches 2–4 garantie dans les deux cas) —
  fondation-maison, famille LSM (`ADR-025`)
- [x] Mini-ADR de sortie de spike (architecture Couche 1 figée) —
  `docs/ADR-025-native-engine-storage-foundation.md`

## N2 — Couche 1 : store durable (Phase 1)

**Ordre imposé : le harnais d'abord, le moteur ensuite.**

- [ ] Harnais crash-consistency : kill -9 en boucle sous charge d'écriture,
  réouverture, vérification d'intégrité — en CI avant le premier commit du
  store
- [ ] Fuzzing cargo-fuzz (nightly séparée) : encodage/décodage clés, replay
  WAL, parsing pages
- [ ] `format.lock` + check CI (équivalent `revision.lock` SurrealDB) : chaque
  type persisté versionné, échec CI si changement sans bump
- [ ] Crate `crates/basemyai-engine` : `store/`, `key/`, `format/`, `error.rs`
  (layout PLAN §3.1) — feature `engine-native`, jamais défaut
- [ ] WAL + transactions + recovery, jugés sur le harnais
- [ ] Runner de tests déclaratifs multi-backend (`memory-tests`) : scénarios
  remember/recall/invalidate/graphe/validité rejoués sur `Libsql` et `Native`,
  diff des résultats
- [ ] `EngineKind::Native` dans `basemyai-core` (additif) + impl
  `MemoryStore` branchée

## N3 — Couche 2 : index vectoriel natif (Phase 2)

- [ ] Choix HNSW vs DiskANN (critère : profil mémoire vs disque pour mémoire
  d'agent locale ; libSQL utilise LM-DiskANN)
- [ ] Implémentation dans `basemyai-engine/src/idx/vector/`
- [ ] Parité bench M6 : mêmes scénarios 10k/100k que
  `docs/benchmarks/m6-knn-results-2026-07-01.md`, chiffres archivés
- [ ] Cible d'amélioration identifiée sur le coût de build d'index
  (libSQL : ~78-79 ms/ligne, quasi-linéaire — c'est LE point faible mesuré
  du backend actuel, le moteur natif doit faire mieux)

## N4 — Couche 3 : graphe natif (Phase 3)

- [ ] Stockage d'adjacence dans `idx/graph/` + traversée bornée
- [ ] Parité `tests/graph.rs` (scoping agent, cycle-safety, profondeur)

## N5 — Parité complète (Phase 4)

- [ ] 100 % de `storage_contract.rs` + `contracts.rs` verts sur `Native`
- [ ] Chiffrement au repos natif (équivalent ADR-007 — chantier crypto sérieux)
- [ ] FTS/BM25 équivalent (parité `recall_hybrid`)
- [ ] Barre hardening M6 : pool/concurrence, bench KNN, stress 1h, key rotation
- [ ] ADR de bascule du défaut (chiffres à l'appui) — décision séparée

## N6 — Aval (après Phase 4)

- [ ] Sync P2P (change-capture du WAL comme primitive ; VISION §5.6)
- [ ] Couche 4 : langage de requête (micro-crates `token`→`parser`→`ast`) —
  **décision produit préalable** : outil interne/CLI, pas surface agent
  (l'avantage `remember`/`recall` sans langage est documenté et se protège)

## Parallèle — indépendant du moteur

- [ ] Multi-modèles d'embedding (catalogue `EMBED_KNOWN_MODELS`, `schema(dim)`
  paramétré, `setup --model`) — ne dépend d'aucun backend, peut démarrer
  n'importe quand
