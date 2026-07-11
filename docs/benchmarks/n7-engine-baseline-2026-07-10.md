# N7.5 — Baseline moteur avant N8 (2026-07-10)

**Rôle** : la baseline exigée par `PLAN-NATIVE-ENGINE.md` §4 (« une baseline
complète est archivée avant toute modification du format SST »). Tout
changement N8 (ADR-039, SST par blocs) se jugera contre ces chiffres.

- **Outil** : `cargo xtask engine-bench` (binaire `engine_bench`, N7.2),
  release, workloads canoniques déterministes (seeds fixes).
- **Machine** : Windows x86_64, Intel Core i7-13620H (la machine des benchs
  M6/N3/N5.5 — comparabilité intra-repo).
- **Commit** : `c0aaa38` (+ patches N7 non commités au moment du run).
- **Données brutes** : `docs/benchmarks/data/n7-baseline/*.json`
  (schema `basemyai-engine-bench/1`, 4 fichiers : {10k, 100k} × {clair, chiffré}).
- **Options moteur** : défauts (`memtable_flush_threshold=1000`,
  `compaction_sst_threshold=4`) — la baseline mesure le moteur tel que livré.

## Chiffres clés (clair)

| Workload | n=10 000 | n=100 000 |
|---|---|---|
| `kv-fill` (mean/op) | 1,36 ms¹ | 0,23 ms |
| `kv-point-read` (mean) | 0,3 µs | 0,9 µs |
| `kv-prefix-scan` (bucket 1 000 clés) | 0,18 ms | 0,38 ms |
| `mixed-read-write` (mean) | 0,27 ms¹ | 0,05 ms |
| `delete-churn` (mean/op) | 1,38 ms¹ | 0,24 ms |
| `flush-compaction` (flush p95) | 19,4 ms | **91,8 ms** |
| `open-large-store` (réouverture froide) | 9,1 ms | 35,5 ms |
| `memory-remember` (mean/op, 384d)² | 3,85 ms (n=5k) | 6,42 ms (n=10k) |
| `memory-recall` (ANN top-10, mean) | 4,03 ms (n=5k) | 7,23 ms (n=10k) |
| RSS pic du run | 92,6 Mo | 166,4 Mo |

¹ Le run 10k clair a été le premier de la session (caches froids) : ses
latences d'écriture (~1,3 ms/op) sont ~5× celles de tous les runs suivants
(~0,25 ms/op, y compris 10k chiffré et 100k clair/chiffré). Un seul run par
config, pas de répétitions — le bruit fsync inter-runs sur ce laptop est du
même ordre que certains écarts. Les chiffres 100k (runs adjacents) sont les
plus comparables entre eux.

² `memory-remember` = chemin composé complet (record + vecmap + nœud/voisins
DiskANN + postings FTS, un enregistrement WAL par op, ADR-027 §3).
`memory-recall` = recherche vectorielle seule (l'hydratation/filtre temporel
vivent dans `basemyai`, benchés par `native_memory_store_bench`).

## Chiffré vs clair (100k, runs adjacents)

| Workload | clair | chiffré | surcoût |
|---|---|---|---|
| `kv-fill` total | 23,4 s | 25,8 s | +10 % |
| `delete-churn` total | 29,0 s | 30,0 s | +4 % |
| `memory-remember` total | 64,2 s | 67,0 s | +4 % |
| `memory-recall` mean | 7,23 ms | 7,25 ms | ~0 % (in-RAM) |
| `open-large-store` | 35,5 ms | 44,0 ms | +24 % (unseal fichier entier) |
| RSS pic | 166 Mo | 200 Mo | +20 % |

Le surcoût AEAD par enveloppe est faible sur le chemin d'écriture (le fsync
domine) ; il se voit surtout à l'ouverture (déchiffrement de SST entières) —
précisément ce que l'AEAD **par bloc** de N8 rend incrémental.

## Ce que la baseline prouve (le dossier à charge de N8/N13)

1. **Amplification d'écriture de la compaction naïve — LE problème mesuré.**
   `memory-remember` à 10k records : **2,46 Go écrits pour 30,6 Mo de données
   vivantes (×80)**, dont 1,87 Go d'entrées de compaction sur 92 passes
   full-merge. Sur le KV pur (`kv-fill` 100k) : 179 Mo écrits pour 12,5 Mo
   vivants (×14,3). Chaque compaction relit et réécrit **tout** le store —
   coût par compaction linéaire en taille du store, donc O(n²) cumulé.
2. **Write stalls croissants.** Le flush p95 passe de 19,4 ms (10k) à
   91,8 ms (100k) : la compaction inline dans `flush()` bloque le writer
   pendant toute la réécriture. À 1M ce serait ~1 s de stall par compaction.
   → N8 (compaction sur un format par blocs) + N13 (compaction concurrente).
3. **Ouverture = chargement intégral.** `open-large-store` lit 100 % des
   octets SST (`open_bytes_read == sst_bytes`, vérifié par compteur) et le
   RSS croît d'autant. 35,5 ms pour 12,7 Mo → un store de 1 Gio se projette
   à ~3 s d'ouverture et ~1 Gio de RSS. → N8 §critère « RSS borné à 1 Gio ».
4. **Le point lookup est in-RAM aujourd'hui** (0,3–0,9 µs, `bytes_read` ne
   bouge pas pendant les lectures) : c'est la conséquence directe du point 3.
   N8 doit garder le lookup rapide (bloom + index de blocs + cache) tout en
   cessant de charger les SST entières —
   l'invariant instrumenté `point_lookup_full_sst_read == 0` a maintenant
   ses compteurs (`block_cache_hits/misses` déjà dans le schéma JSON).
5. **Une écriture = un fsync.** ~0,25 ms/op de plancher sur ce NVMe. Le
   group commit (N13 §10.4) est le levier mesurable suivant côté débit.

## Run 1M (ajouté le soir même — runs détachés séquentiels, clair puis chiffré)

| Workload (n=1 000 000) | clair | chiffré |
|---|---|---|
| `kv-fill` total / mean | 332 s / 0,33 ms | 353 s / 0,35 ms |
| `kv-point-read` mean | 1,8 µs | ~1,8 µs |
| `kv-prefix-scan` (bucket 1 000 clés) | **4,10 ms** | ~4,1 ms |
| `flush-compaction` (flush p95) | **860 ms** | **1 003 ms** |
| `open-large-store` (125 Mo de SST) | **339 ms** | **452 ms** |
| RSS pic du run | **759 Mo** | **996 Mo** |

Compteurs (clair, fin de `kv-fill`) : **15,85 Go écrits pour 125 Mo vivants
(amplification ×127)**, 249 compactions. La courbe d'amplification est bien
super-linéaire : ×14 à 100k → ×127 à 1M — le O(n²) cumulé de la compaction
full-merge, mesuré, plus projeté.

Deux enseignements de plus au dossier N8 :

- **Le scan préfixé se dégrade en O(store), pas O(bucket)** : 0,38 ms à 100k
  → 4,10 ms à 1M pour le même bucket de 1 000 clés — `scan_prefix` itère
  toutes les entrées de chaque SST. L'index de blocs (ADR-039 §4) le ramène
  à la suite contiguë de blocs couvrant l'intervalle.
- **Le write stall à ~1 s est atteint dès 1M** (flush p95 : 860 ms clair,
  1 s chiffré) — la valeur projetée « à 1M ce serait ~1 s » au §« Ce que la
  baseline prouve » est maintenant un chiffre mesuré.

RSS : 759 Mo pour 125 Mo de SST vivantes (~×6 — structures par entrée en
mémoire), et presque 1 Gio en chiffré. Le critère ADR-039 §8.1 (RSS borné,
jamais proportionnel aux data blocks) se juge contre ces valeurs.

## Limites honnêtes

- **Un run par configuration** — pas de répétitions ni d'intervalles de
  confiance ; cf. note ¹. Les ratios (amplification, octets) sont exacts
  (compteurs déterministes) ; les latences absolues portent le bruit du
  laptop.
- `memory-n` plafonné (5k/10k, y compris dans le run 1M) : le coût d'insert
  DiskANN croît avec N (documenté depuis N3) et domine le temps de run.
- RSS = process entier (sampler in-process, mêmes limites documentées que
  `vector_bench` N3.1).
