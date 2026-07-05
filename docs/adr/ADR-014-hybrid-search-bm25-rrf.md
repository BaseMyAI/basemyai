# ADR-014 — Recherche hybride : full-text BM25 (FTS5) fusionné au vecteur par RRF

**Statut** : ✅ Accepted | **Date** : 2026-06

**Contexte**

Le recall purement vectoriel rate les **termes exacts rares** : sigles, identifiants, références, noms propres peu fréquents (« ACME-42 », un UUID, un nom de fichier). L'embedding les noie dans la sémantique. À l'inverse, une recherche par mots-clés seule rate les reformulations. La RRF (`rrf_fuse`, ADR-012) était déjà en place mais sans second signal à fusionner. libSQL embarque **FTS5 + `bm25()`** dans son cœur (vérifié : virtual table + MATCH + ranking BM25 disponibles sans extension externe ni feature).

**Décision**

Index **full-text autonome** `memory_fts` (migration schéma V4) : table virtuelle FTS5 `(id UNINDEXED, agent_id UNINDEXED, content, tokenize='porter unicode61 remove_diacritics 2')`. Racinisation (`porter`) + pliage des accents, en-DB, 100 % local, zéro dépendance nouvelle.

- **Tenue à jour par la façade `Memory`** : INSERT au `remember`, DELETE au `forget` et au `purge_agent`. La migration V4 **backfille** les souvenirs déjà présents (`INSERT … SELECT FROM memory`).
- **`Memory::recall_hybrid(query, k)`** : produit deux classements — vecteur (`vector_knn`) et BM25 (`memory_fts MATCH … ORDER BY bm25`) — tous deux bornés `agent_id` + validité temporelle, puis les fusionne par `rrf_fuse`. Le `score` du `Record` retourné porte le score RRF fusionné.
- **Sûreté MATCH** : la requête libre est tokenisée (alphanumérique), chaque terme cité en littéral (insensible aux mots-clés FTS5 AND/OR/NEAR) et joint par OR (orienté rappel) — pas d'erreur de syntaxe ni d'injection.
- **Surfaces** : exposé via l'outil MCP `recall_hybrid`. REST/SDK : à câbler ultérieurement (même signature que `recall`).

**Conséquences**

✅ Parité (et différenciation) sur la recherche hybride sans quitter libSQL : pas de moteur externe, pas de réseau.
✅ Réutilise `rrf_fuse` (mécanisme pur déjà testé) — la fusion reste agnostique des poids.
✅ Isolation agent + validité temporelle préservées sur les **deux** signaux.
⚠️ `memory_fts` duplique le `content` (pas external-content) : coût disque ~1× le texte. Choix assumé pour la simplicité de synchronisation et l'absence de triggers.
⚠️ FTS5 exige le **nom réel** de la table dans `MATCH`/`bm25` (pas d'alias).
⚠️ `valid_until` n'est pas dans l'index FTS : la validité est filtrée par jointure sur `memory` au moment de la requête (un soft-delete laisse la ligne FTS, masquée au recall).

**Alternatives rejetées**

FTS5 **external-content** (`content='memory'`) — évite la duplication mais impose des triggers de synchronisation et un mapping `content_rowid` ; complexité supérieure pour un gain disque modéré.

Moteur de recherche externe (Tantivy, Meilisearch) — viole le single-file local / zéro-dépendance-externe (pile commune ADR-011).

Pondération linéaire vecteur+BM25 — exige de calibrer des poids hétérogènes ; la RRF s'en affranchit (ADR-012).
