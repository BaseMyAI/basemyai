# ADR-002 — sqlite-vec — vecteurs dans SQLite

**Statut** : 🔵 Superseded by ADR-011 | **Date** : 2026-06

> Remplacé par ADR-011 : libSQL fournit le vecteur **natif** (pas d'extension à
> linker). L'intention — vecteurs dans le même fichier, pas de DB externe — tient.

**Contexte**

Le RAG exige une recherche vectorielle (KNN par similarité cosine). L'approche standard de l'industrie est une base vectorielle dédiée (Qdrant, LanceDB, Pinecone). Mais BaseMyAI est *privacy-first, 100% local, mono-fichier*. Ajouter une base vectorielle externe signifie : deux systèmes à déployer, deux stores à synchroniser, deux fichiers (ou un service réseau) — et une violation directe du principe mono-fichier local.

**Décision**

Stocker les vecteurs **dans** SQLite via l'extension `sqlite-vec`. Une table virtuelle porte les embeddings ; le KNN s'exécute en SQL, dans le même fichier que le reste de la mémoire.

```
VectorIndex (basemyai-core)
  upsert(id, &[f32])
  knn(query, k, filtre SQL optionnel) -> Vec<(id, distance)>
```

Une requête de recall combine, en une seule requête SQL, la similarité cosine sqlite-vec **et** un filtre fourni par l'appelant (cf. RAG temporel, ADR-005 ; isolation, ADR-006).

**Conséquences**

✅ Mono-fichier conservé : un seul `.db` contient données + vecteurs.
✅ Pas de second système à déployer/synchroniser.
✅ Transactions ACID couvrant données ET vecteurs ensemble.
✅ Le filtre SQL permet de fusionner KNN + temps + agent en une requête.
⚠️ `sqlite-vec` est une dépendance C (extension à compiler/lier) — à tester Linux + Windows dès le 1ᵉʳ commit.
⚠️ Compatibilité de build sqlite-vec + sqlcipher à valider (cf. ADR-007).
⚠️ Pas d'index ANN sophistiqué (HNSW distribué) — acceptable à l'échelle visée (mémoire d'agent local, pas milliards de vecteurs).

**Alternatives rejetées**

Qdrant / LanceDB / base vectorielle externe — deux systèmes à synchroniser, viole le mono-fichier et le 100% local.

Embeddings dans SQLite + scan linéaire cosine maison — jetable, ne passe pas à l'échelle, réinvente ce que sqlite-vec fait déjà mieux.

API d'embedding/recherche cloud — fait fuiter les données, viole le zéro-cloud.
