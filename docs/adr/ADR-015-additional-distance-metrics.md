# ADR-015 — Métriques de distance additionnelles : euclidienne & hamming par re-classement

**Statut** : ✅ Accepted | **Date** : 2026-06

**Contexte**

L'index vectoriel natif libSQL est **cosinus** (`metric=cosine`). Certains cas d'usage veulent une autre métrique : euclidienne (L2, sensible à la magnitude) ou hamming (quantification binaire, rapide). Ré-indexer en plusieurs métriques alourdirait le schéma et le stockage.

**Décision**

`basemyai_core::Metric { Cosine, Euclidean, Hamming }` + `Store::vector_knn_metric(table, query, k, filter, metric)`. Pour `Cosine` : chemin natif inchangé. Pour `Euclidean`/`Hamming` : **sur-échantillonnage** du top-k cosinus (`k × 16`) puis **re-classement en Rust** sur les vecteurs réels récupérés via `vector_extract`. Hamming = nombre de dimensions où le signe diffère (1 bit/dim). Exposé côté `basemyai` par `Memory::recall_with_metric`.

**Conséquences**

✅ Métriques multiples sans index supplémentaire : un seul index cosinus alimente le rappel ANN.
✅ Mécanisme au core (agnostique), sens au consommateur (cohérent ADR-001).
⚠️ Le rappel reste piloté par l'ANN cosinus : pour des métriques très divergentes du cosinus, l'oversample ×16 est best-effort (un voisin L2-proche mais cosinus-lointain peut manquer).
⚠️ Re-classement = lecture des vecteurs des candidats (coût mémoire/CPU borné par `k × 16`).
