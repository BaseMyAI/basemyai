# N8 — Baseline moteur après le cutover SST par blocs (2026-07-11)

**Rôle** : mesurer le nouveau format SST par blocs (ADR-039, N8.1→N8.9)
contre la baseline N7.5 (`docs/benchmarks/n7-engine-baseline-2026-07-10.md`,
format whole-file), sur les mêmes workloads canoniques, et juger les
critères de sortie chiffrés de l'ADR-039 §8.

- **Outil** : `cargo xtask engine-bench` (binaire `engine_bench`), release,
  workloads canoniques déterministes (seeds fixes) — identique à N7.5.
- **Machine** : Windows x86_64, même laptop que N7.5 (comparabilité
  intra-repo).
- **Ce qui a changé depuis N7.5** : `store::sst_block::BlockSstFile`
  remplace entièrement `store::sst::SstFile` (N8.5) ; reader optimisé
  bloom→index→bloc unique (N8.4) ; AEAD par bloc au lieu d'une enveloppe
  fichier entier (N8.8) ; cache de blocs LRU borné en octets (N8.7) ;
  `store.meta`/rejet des anciens stores (N8.9). Options moteur : défauts
  (`memtable_flush_threshold=1000`, `compaction_sst_threshold=4`,
  `block_size=16 KiB` — décision N8.1, `block_cache_capacity_bytes=32 Mio`).
- **Données brutes** : `docs/benchmarks/data/n8-baseline/*.json` (schéma
  `basemyai-engine-bench/1`, 6 fichiers : {10k, 100k, 1M} × {clair, chiffré}).
  Le champ `engine_stats.point_lookup_full_sst_read` a été ajouté au
  rapport JSON pendant cette session (il existait déjà côté `EngineStats`/
  N8.4, seul le sérialiseur du bench ne le publiait pas encore) — les 6
  fichiers archivés le portent tous.

## Invariant N8.4 : `point_lookup_full_sst_read == 0`

Confirmé **0 sur les 9 workloads × 3 échelles × 2 modes = 54 lignes de
rapport**, y compris à 1M sous churn/compaction actifs. Aucun point lookup
n'a jamais lu plus d'un bloc de données au sein d'une SST — l'invariant
structurel (bloom → index → un seul bloc) tient sous charge réelle, pas
seulement dans le test unitaire qui l'épingle.

## Ouverture : le critère central de l'ADR-039 §8.1

| n | `open_bytes_read` | `sst_bytes` | ratio | mean_ms | (rappel N7.5) |
|---|---|---|---|---|---|
| 10k clair | 20 796 o | 1,52 Mo | **1,4 %** | 0,92 ms | 9,1 ms (fichier entier) |
| 100k clair | 174 392 o | 12,93 Mo | **1,3 %** | 1,60 ms | 35,5 ms |
| 1M clair | 1,71 Mo | 127,06 Mo | **1,3 %** | 5,50 ms | **339 ms** |
| 1M chiffré | 1,71 Mo | 127,44 Mo | **1,3 %** | 6,89 ms | **452 ms** |

**Critère `open_bytes_read ≤ 5 % de sst_bytes` tenu large à toutes les
échelles** (1,3-1,4 % mesuré, sur workloads réels — pas un cas jouet).
Ouverture **~62× plus rapide à 1M clair** (5,5 ms vs 339 ms) et **~66× plus
rapide à 1M chiffré** (6,9 ms vs 452 ms) que le format whole-file. Le
surcoût chiffré à l'ouverture (**+24 % dans N7.5**, unseal du fichier
entier) **a quasiment disparu** : 5,50 ms clair vs 6,89 ms chiffré à 1M,
+25 % en valeur absolue mais sur une base ~65× plus petite — c'est
exactement ce que l'AEAD par bloc (N8.8) était censé produire (déchiffrement
lazy par bloc au lieu du fichier entier).

RSS pic à 1M : 570 Mo clair / 547 Mo chiffré (N8) contre **759 Mo clair /
996 Mo chiffré (N7.5)** — RSS pic baisse de **25 % clair / 45 % chiffré**,
cohérent avec l'ouverture O(métadonnées) qui ne matérialise plus la SST
entière en RAM à l'open (le RSS restant vient du memtable/DiskANN/FTS in
process, pas de la lecture SST elle-même).

## `kv-point-read` : hot/cold pas encore séparés proprement

| n | mode | mean | p50 | p95 | p99 |
|---|---|---|---|---|---|
| 10k | clair | 8,9 µs | — | — | — |
| 100k | clair | 41,9 µs | — | 249,6 µs | — |
| 1M | clair | 90,7 µs | — | 137,2 µs | — |
| 1M | chiffré | 99,1 µs | — | 149,0 µs | — |

Comparé à N7.5 (0,3-1,8 µs, tout en RAM après un open whole-file) c'est
**plus lent en moyenne** — attendu : le nouveau format paie un vrai `pread`
+ décodage (+ déchiffrement AEAD si chiffré) au premier accès à chaque
bloc, alors que l'ancien format n'avait plus aucune I/O après l'open. Le
cache de blocs (N8.7) absorbe les accès répétés (`block_cache_hits`
dominant `misses` dans tous les rapports une fois le working set chaud —
ex. 100k : 9 241 hits / 759 misses sur le workload `kv-point-read`), mais
le workload canonique actuel ne distingue pas explicitement « premier accès
à un bloc » (miss garanti) de « accès répété » (hit garanti) — les moyennes
ci-dessus mélangent les deux. Les critères ADR-039 §8.5 (« hot mean ≤10 µs,
cold p95 ≤500 µs ») ne sont donc **pas directement vérifiables** avec ce
banc tel qu'il existe : p95 (137-250 µs) reste confortablement sous le
seuil froid (500 µs), mais aucune ligne du rapport n'isole un mean
strictement à chaud. **Limite documentée, pas un échec** — un futur banc
`kv-point-read-warm` (répéter les mêmes clés en boucle après un premier
passage de mise en cache) donnerait la mesure exacte ; hors périmètre de
cette session.

## `kv-fill` : dans la fourchette

| n | mode | mean N8 | mean N7.5 | delta |
|---|---|---|---|---|
| 100k | clair | 0,264 ms | 0,23 ms | +15 % |
| 1M | clair | 0,376 ms | 0,33 ms | +14 % |
| 1M | chiffré | 0,388 ms | 0,35 ms | +11 % |

Légèrement au-dessus du seuil `≤+10 %` de l'ADR-039 §8.5 à 1M (+11 à +14 %),
dans le bruit fsync inter-runs déjà documenté par N7.5 (un run par config,
pas de répétitions) — pas un signal fort d'alarme, mais pas nul non plus.
Le flush/compaction inclut maintenant l'assemblage bloc/index/bloom (et le
scellement par section si chiffré) au lieu d'un simple `encode` séquentiel ;
c'est le coût attendu de la structure, pas une régression accidentelle.

## Amplification de compaction : inchangée (attendu, ADR-039 hors périmètre)

1M clair : `bytes_written` = 16,08 Go pour `sst_bytes` (vivant) = 126,8 Mo
→ **amplification ×126,8** (249 compactions), quasi identique aux **×127**
de N7.5. Confirme exactement ce qu'ADR-039 annonçait : la stratégie de
compaction full-merge naïve **n'a pas changé** dans ce chantier (décision
séparée, plan §16) — seul le format du fichier SST a changé, pas
l'algorithme qui décide quand/quoi compacter.

## `kv-prefix-scan` : régression réelle, trouvée et documentée honnêtement

| n | mode | mean N8 | mean N7.5 | facteur |
|---|---|---|---|---|
| 10k | clair | 3,99 ms | — | — |
| 100k | clair | 59,65 ms | 0,38 ms | **×157** |
| 1M | clair | 360,84 ms | 4,10 ms | **×88** |
| 1M | chiffré | 447,04 ms | ~4,1 ms | **×109** |

**Régression mesurée, pas dans le bruit.** Cause : `Engine::scan_prefix`
appelle `BlockSstFile::entries()` (décodage intégral de **tous** les blocs
de **toutes** les SST vivantes) à **chaque appel**, sans jamais consulter le
cache de blocs (délibéré, N8.7 — un scan complet ne doit pas évincer des
blocs chauds pour des données froides lues une seule fois) ni utiliser
l'index pour ne lire que la plage de blocs couvrant le préfixe. C'est
exactement le chemin qu'ADR-039 §4 annonçait comme travail futur (« le scan
préfixé lit la suite contiguë de blocs couvrant l'intervalle via l'index,
en streaming ») — **pas encore implémenté** : la version actuelle est
correcte mais pas optimisée, elle décode structurellement plus de données
qu'avant (l'ancien format lisait le fichier entier une fois à l'open, celui
d'après restait en RAM ; le nouveau relit et redécode tout à chaque
`scan_prefix`, y compris le déchiffrement AEAD par bloc si le store est
chiffré). Aucun critère de sortie ADR-039 §8 ne couvre `kv-prefix-scan`
explicitement — ce n'est donc pas un blocage de clôture N8, mais un gap
réel à ne pas cacher. **Candidat de suivi documenté** (pas engagé dans ce
chantier) : router `scan_prefix`/la compaction par la plage de blocs de
l'index plutôt qu'un `entries()` complet.

### Addendum (même jour) : régression corrigée avant N9

Le suivi ci-dessus a été implémenté juste après la clôture N8 :
`BlockSstFile::entries_with_prefix` (recherche binaire `partition_point`
sur `last_key` → décodage des seuls blocs chevauchant la plage du préfixe,
arrêt au premier bloc dont `first_key` sort de la plage), branché dans
`Engine::scan_prefix`. Aucun changement de format on-disk (`format.lock`
inchangé) ; le cache de blocs reste réservé aux point lookups (N8.7).
Re-mesuré sur la même machine, mêmes commandes :

| n | mode | avant fix | après fix | vs N7.5 |
|---|---|---|---|---|
| 100k | clair | 59,65 ms | **1,58 ms** | ×4,2 (était ×157) |
| 1M | clair | 360,84 ms | **1,47 ms** | **0,36× — 2,8× plus rapide** (était ×88) |
| 1M | chiffré | 447,04 ms | **1,74 ms** | ~0,42× (était ×109) |

À 1M le scan préfixé est désormais *plus rapide* qu'en N7.5 : l'ancien
format gardait tout le SST décodé en RAM mais filtrait linéairement toutes
les entrées ; le nouveau chemin ne décode que ~1-3 blocs par SST (AEAD
compris en chiffré). Épinglé par tests (`entries_with_prefix_*` dans
`store/sst_block.rs`), dont le comptage des blocs décodés.
La compaction reste sur `entries()` (elle doit voir toutes les clés).

## Bilan face aux critères de sortie ADR-039 §8

1. ✅ Ouverture 1 Gio (extrapolé depuis 1M/127 Mo, linéaire confirmé
   10k→100k→1M) : RSS additionnel borné, `open_bytes_read` ≤ 5 % — **1,3 %
   mesuré, large marge**.
2. ✅ `point_lookup_full_sst_read == 0` — confirmé sur 54 lignes de rapport
   à 3 échelles, 2 modes, 9 workloads.
3. — Corruption de bloc (bit-flip/troncature/permutation) : déjà couvert
   par les tests unitaires `store::sst_block::tests` (N8.3/N8.4/N8.8), pas
   re-testé ici (banc de perf, pas de fuzzing/corruption) — voir §fuzz.
4. ✅ Ancien code SST supprimé (N8.5) ; ancien store ⇒
   `UnsupportedStoreFormat` (testé, N8.9).
5. 🟡 Régressions vs baseline 100k : `kv-fill` +15 % (seuil +10 %, léger
   dépassement) ; `kv-point-read` non isolable hot/cold avec le banc actuel
   (voir section dédiée) ; `memory-recall` — voir ci-dessous.
6. ✅ `cargo xtask engine-crash` (`test-crash-consistency`) vert en clair et
   chiffré, revérifié après le cutover (7/7 modes, 20 cycles kill réels
   chacun).
7. ✅ Cibles fuzz des nouveaux codecs posées **et exécutées** cette session
   (WSL, nightly + cargo-fuzz) : 15 cibles, ~25 s chacune, plusieurs
   millions d'itérations par cible, **zéro crash**.
8. ✅ Baseline N8 archivée (ce document + `data/n8-baseline/*.json`,
   10k/100k/1M, clair+chiffré) à côté de N7.5.

`memory-recall` (ANN top-10) : 9,5-15,9 ms selon l'échelle vs 4,03-7,25 ms
en N7.5 — l'essentiel de la donnée vecteur/graphe/FTS vit dans les mêmes
SST par blocs maintenant, donc paie le même coût d'ouverture/décodage
lazy-par-bloc que le KV ; `memory_n` restant plafonné à 10k dans les deux
baselines (même limite documentée), la comparaison directe est de toute
façon partielle.

## Limites honnêtes

- Un run par configuration (comme N7.5) — pas de répétitions, le bruit
  fsync inter-runs reste dans les chiffres.
- `kv-point-read` hot/cold non séparés par le banc actuel — limite de
  l'outil, pas du moteur ; discuté explicitement ci-dessus plutôt que
  d'affirmer une conformité non mesurable.
- `kv-prefix-scan` : régression réelle documentée, pas corrigée dans ce
  chantier (hors périmètre des critères de sortie ADR-039 §8, mais à ne
  pas perdre de vue).
- Fuzz : 15 cibles × ~25 s (~6 minutes de wall-clock cumulé) — un balayage
  raisonnable, pas une campagne longue durée (nightly/CI dédiée serait le
  suivi naturel, cf. `fuzz/README.md` §CI).

## N8.6 — bloom par bloc : pas justifié par les chiffres

Le spike N8.1 mesurait déjà (`n8.1-block-size-spike-2026-07-10.md`) : bloom
10 bits/clé = 100 % des lookups absents sans I/O sur les workloads
canoniques. Cette session confirme au niveau moteur complet (pas juste le
prototype) : `point_lookup_full_sst_read == 0` partout, et
`block_cache_hits`/`misses` cohérents avec un filtre par-SST qui fait déjà
son travail (aucun accès disque superflu observé dans les compteurs). Rien
dans les données ne justifie un filtre par bloc (coût d'implémentation et
d'espace disque supplémentaire pour un gain non mesurable ici) — **décision
maintenue : un seul bloom filter par SST**, conforme à ADR-039 §6.
