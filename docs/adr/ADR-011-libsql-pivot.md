# ADR-011 — Pivot vers libSQL (vecteur natif + chiffrement), traits async

**Statut** : ✅ Accepted | **Date** : 2026-06
**Supersede** : ADR-002 (sqlite-vec). **Amende** : ADR-003 (Candle tient), ADR-007 (chiffrement désormais via libSQL).

**Contexte**

ADR-002 prévoyait `sqlite-vec` (extension C à linker) + `rusqlite` + pool `r2d2` + `sqlcipher`. Trois frictions : le **linkage de l'extension** (le risque D4), la compatibilité de build **sqlcipher + sqlite-vec**, et une recherche **brute-force exacte** (pas d'ANN). Le pool était par ailleurs synchrone.

La recherche d'écosystème 2026 a fait émerger **libSQL** (fork SQLite production-ready, « le nouveau standard ») et **Turso DB** (réécriture Rust pure, async-natif, beta). libSQL apporte, **sans extension** : vecteur natif (`F32_BLOB`, `libsql_vector_idx`, `vector_top_k` en ANN), chiffrement au repos intégré, et une API **async**. Validé par un smoke test : libSQL compile sur Windows MSVC **sans CMake** et le vecteur natif fonctionne en mémoire.

**Décision**

- **Backend = libSQL** (crate `libsql`, embarqué local). Vecteur **natif**, plus de `sqlite-vec` ni `rusqlite`/`r2d2`.
- **Traits du core async** pour `Store` et les opérations vectorielles. L'`Embedder` **reste sync** (CPU-bound ; le consommateur l'enveloppe dans `spawn_blocking` si besoin). `MaintenanceTask` async (via `async-trait`).
- **Suppression du trait `VectorIndex`** : les ops vecteur sont natives sur `Store` (`vector_upsert`, `vector_knn`). La seule abstraction laissée aux consommateurs : `Embedder` (apporter le sien) + `Filter` (exprimer son sens).
- **Connexion partagée clonée** : libSQL synchronise l'accès en interne ; nécessaire pour que les bases `:memory:` restent cohérentes.
- **Chiffrement = feature `crypto`** (chiffrement libSQL, exige CMake) — opt-in, déféré. C'est l'équivalent résiduel du risque D4, désormais réduit à une **dépendance de toolchain**, pas un risque de code.
- Filtre paramétré (anti-injection) + validation d'identifiant de table conservés.
- **Chemin de migration vers Turso DB** (pur Rust, zéro C) quand il passera production (V2/V3). **Note ADR-024 (2026-07-02)** : ce chemin est abandonné — voir `docs/adr/ADR-024-native-engine.md`.

**Conséquences**

✅ Risque D4 (linkage d'extension) **supprimé** : vecteur natif.
✅ **ANN natif** (`vector_top_k`) dès V1 — plus de brute-force jetable.
✅ Chiffrement au repos **intégré** (plus de combo sqlcipher + sqlite-vec à valider).
✅ **Async-natif** → colle à la vision « OS cognitif » (consolidation, appels LLM, retrieval multi-signal, sync).
✅ **SQLite-compatible** → le **graphe** (entités/relations) que la vision réclame se modélise en **tables + CTE récursives dans le même fichier** — pas de Kuzu/Neo4j. Hybride vecteur + graphe + temporel, **mono-fichier**.
✅ Chemin Turso (pur Rust) ouvert pour plus tard.
⚠️ Le chiffrement exige **CMake** → feature `crypto` à provisionner.
⚠️ Connexion partagée = accès sérialisé (acceptable embarqué ; pool multi-connexions pour fichiers à raffiner si besoin).
⚠️ libSQL reste un **fork C** (pas pur Rust) — assumé jusqu'à ce que Turso DB soit production.
⚠️ `vector_top_k` applique le filtre **après** le top-k → sur-échantillonner pour garantir `k` résultats filtrés (TODO).

**Alternatives rejetées**

`rusqlite` + `sqlite-vec` + `sqlcipher` (ADR-002 d'origine) — extension à linker (D4), brute-force, sync, combo sqlcipher fragile.

Turso DB **maintenant** — beta, pas production-ready ; trop risqué pour bâtir V1 dessus.

DB vectorielle ou graphe **externes** (Qdrant, Kuzu, Neo4j) — multi-systèmes à synchroniser, viole le mono-fichier local.

`fastembed`/ONNX pour les embeddings — réintroduit le risque ONNX/Windows ; **Candle (pur Rust) maintenu** (ADR-003).
