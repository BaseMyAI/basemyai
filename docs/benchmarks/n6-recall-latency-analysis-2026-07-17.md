# Analyse — latence `recall` du bench N6 : décomposition, cause dominante, remédiations (2026-07-17)

**Question posée** : le rerun N6 (`docs/benchmarks/n6-native-vs-mem0-qdrant-2026-07-10.md`)
publie `recall`/`recall_hybrid` BaseMyAI à ~351–358 ms mean contre ~94 ms pour
Mem0+Qdrant. `recall` est l'opération la plus fréquente d'un agent (lecture ≫
écriture, un appel par tour) — d'où vient ce coût, et que faire ?

**Réponse courte** : deux causes empilées, toutes deux vérifiées ici.

1. **Mesuré** — l'embedding de la requête via Candle (CPU, f32, sans MKL ni
   CUDA) coûte **~55–60 ms** et domine le recall à chaud (~70–95 % du temps).
   Le KNN + hydratation + `touch` du moteur natif ne pèsent que **~2–25 ms**
   à N=500.
2. **Mesuré** — les chiffres publiés par N6 sont en plus **inflatés ~3–4×
   par les conditions de la machine pendant ce run-là** : une reproduction
   exécutée le 2026-07-17, *même binaire, même corpus, même protocole, même
   machine*, donne `recall` **86,3 ms mean** (dernier quartile **61,0 ms**)
   au lieu de 357,8 ms. À conditions saines, BaseMyAI est **à parité avec
   les 94 ms de Mem0+Qdrant enregistrés par N6** — alors même que Mem0
   embed sa requête sur le **GPU** (Ollama) et BaseMyAI sur le CPU.

Tout ce qui suit distingue explicitement **mesuré** / **déduit** / **hypothèse**.

---

## 1. Comment le harnais appelle BaseMyAI (lu dans le code)

`benchmarks/p1-market/run.py` (`run_basemyai`, l.100–155) :

- **Binding Python in-process** (`import basemyai`, PyO3), pas de subprocess
  CLI ni de REST. `Memory.open(...)` est appelé **une seule fois** ; le modèle
  Candle est chargé une fois par run (mesuré : ~2,3 s au premier open). Il n'y
  a donc **pas** de rechargement du modèle par appel — cette hypothèse est
  écartée.
- Chaque op est chronométrée autour d'un `await` d'une coroutine PyO3
  (`pyo3_async_runtimes`, runtime tokio embarqué,
  `bindings/basemyai-py/src/memory.rs`). Overhead binding/asyncio : négligeable
  à cette échelle (le plancher observé bout-en-bout est 55 ms, identique au
  coût embed seul, voir §3).
- Le binaire du run N6 est un **build release** : le `.pyd` installé
  (`bindings/basemyai-py/python/basemyai/_internal.cp312-win_amd64.pyd`,
  11 648 512 octets, 2026-07-10 00:30) est octet-pour-octet
  `target/release/basemyai.dll` (même taille, même timestamp). L'hypothèse
  « wheel debug » est écartée. Ce même `.pyd` a servi à la reproduction du
  §3 — on compare donc exactement le binaire de N6.

Côté Mem0 (`run_mem0_qdrant`, l.158–266) : embeddings via **Ollama**
(`all-minilm`, 384d — provider `ollama`, l.185–189), c'est-à-dire un serveur
d'inférence **séparé, résident, et GPU** : la machine des benchs porte une
RTX 4060 Laptop 8 GiB et « Ollama offloads all layers »
(`docs/benchmarks/local-memory-vs-mem0-qdrant.md`, tableau machine ;
`benchmarks/p1-market/README.md` § Gotchas exige les deux modèles résidents
en VRAM avant le run). Qdrant est en mode local embarqué (in-process) pour N6.

## 2. Le chemin de code du recall BaseMyAI (lu dans le code)

`crates/basemyai/src/memory/mod.rs` :

- `recall_with_options` (l.468–483) : `self.embedder.embed(query)` —
  **synchrone, dans le contexte async, sans `spawn_blocking`** (le PRD
  REQ-004 prévoit que le consommateur enveloppe ; `basemyai` ne le fait pas —
  sans effet sur un bench séquentiel, mais bloque un worker tokio sous
  concurrence) — puis `recall_vector`.
- `recall_hybrid_with_options` (l.540–598) : embed + `vector_ranking_ids`
  + `keyword_ranking_ids` (BM25) + `rrf_fuse` + `hydrate`.

`crates/basemyai/src/storage/native_store/` :

- `trait_impl.rs::recall_vector` (l.253–298) : deux passes — recherche sous
  verrou de lecture (`search_filtered` : KNN LM-DiskANN oversamplé ×8,
  résolution `vec_id → (agent, id)`, hydratation, filtres validité/couche,
  `inner.rs` l.17–65), puis **`touch` de `last_access` sous verrou
  d'écriture** — c'est-à-dire **une écriture durable par recall** : le batch
  de touch passe par le WAL, fsyncé avant mise à jour de la memtable
  (`crates/basemyai-engine/src/store/engine.rs`, `sync_all` l.598, « record
  is fsynced before the memtable is updated » l.643).
- Chaque passe est un `spawn_blocking` (`mod.rs` l.427–461) : deux
  aller-retours pool bloquant par recall.

`crates/basemyai-core/src/embed/candle.rs` :

- BERT f32 (`DTYPE = F32`, l.23), mean-pooling + L2. **Aucune feature `mkl`,
  `cuda` ou `metal` n'existe dans le workspace** (`Cargo.toml` racine l.48–50 :
  `candle-* = "0.11"` sans features ; `crates/basemyai-core/Cargo.toml` :
  la feature `embed` n'active rien de plus). `to_candle_device` (l.157–163)
  replie **silencieusement** sur CPU si CUDA n'est pas compilé — ce qui est
  toujours le cas ici. Conséquence : la promesse ADR-010 « le GPU est
  exploité s'il est présent » n'est **pas tenue** par le binding livré ; la
  RTX 4060 de la machine de bench sert à Ollama (donc à Mem0), jamais à
  BaseMyAI. (Déduit du code + des manifestes, pas d'un profil GPU.)

## 3. Mesures locales du 2026-07-17 (nouvelles, non destructives)

Protocole : deux sondes Python jetables (scratchpad de session, DB `.bmai`
neuves dans `%TEMP%` — **aucun fichier du repo touché, aucun réseau, aucun
provisioning**), exécutées avec le venv du harnais et **le `.pyd` exact du
run N6** (vérifié §1), modèle local
`benchmarks/p1-market/.models/all-MiniLM-L6-v2`.

> Note de méthode : le `__init__.py` du working tree (modifié, non committé,
> 2026-07-17) exporte `ContextBundle` que le `.pyd` du 10 juillet n'a pas —
> la sonde importe le `.pyd` via un shim minimal pour rester sur le binaire N6.

**Sonde 1 — décomposition** (20 itérations par ligne, requête de longueur
comparable au corpus) :

| Mesure | mean | p50 | min | max |
|---|---|---|---|---|
| `open` (chargement Candle inclus) | 2 309 ms | — | — | — |
| `recall` sur **DB vide** (≈ embed pur + overhead binding, KNN=0) | **60,2 ms** | 59,0 | 55,0 | 72,8 |
| `remember` (embed + écriture) à N≤20 | 61,4 ms | 60,5 | 55,4 | 71,1 |
| `recall` à N=20 | 96,5 ms | 101,0 | 59,1 | 119,8 |
| `recall_hybrid` à N=20 | 98,3 ms | 101,0 | 60,4 | 130,6 |

**Sonde 2 — reproduction du protocole N6** (corpus `corpus.jsonl` complet,
500 `remember` puis 500 `recall` + 500 `recall_hybrid`, k=5, DB neuve
chiffrée, même agent) :

| Opération | mean | p50 | p95 | min | max | q1 mean | q4 mean |
|---|---|---|---|---|---|---|---|
| `remember` ×500 | **115,7 ms** | 109,8 | 186,2 | 55,1 | 533,0 | 109,2 | 112,9 |
| `recall` ×500 (N=500) | **86,3 ms** | 71,6 | 136,5 | 55,5 | 256,0 | 115,4 | **61,0** |
| `recall_hybrid` ×500 | **86,9 ms** | 71,8 | 136,0 | 55,7 | 241,4 | 115,4 | 61,5 |

À comparer aux chiffres publiés par N6 (2026-07-10, 04h16, même binaire,
même corpus, même machine) : `remember` 317,8 ms / `recall` 357,8 ms /
`recall_hybrid` 350,8 ms mean — soit **×2,7 à ×4,1 plus lent** que la
reproduction. Et au `recall` Mem0+Qdrant de N6 : **93,7–95,2 ms mean**.

### Décomposition de la latence `recall` (à conditions saines, N=500)

| Composante | Coût | Statut |
|---|---|---|
| Chargement/warm-up modèle | ~2,3 s **une fois par process** — amorti, hors chemin par-appel | mesuré |
| **Embedding requête (Candle, CPU f32, sans MKL/CUDA)** | **~55–60 ms** | **mesuré** (recall sur DB vide ; plancher identique sur toutes les séries : min 55,0–55,7 ms partout) |
| KNN LM-DiskANN (oversample ×8) + résolution + hydratation + filtres | ~1–7 ms à N=500–10k | mesuré indirectement (q4 recall 61,0 ms − embed ~58 ms) ; cohérent avec N7 (`memory-recall` ANN 4,0 ms à n=5k) et N5.5 (17 ms/query à n=10k, chemin complet) |
| BM25 + RRF (hybrid uniquement) | ~0,5 ms (86,9 − 86,3) | mesuré (différence de moyennes) |
| `touch` `last_access` (écriture WAL fsyncée **par recall**) + 2× `spawn_blocking` | ~1–5 ms à chaud ; explique une partie des ~25–35 ms au-dessus de l'embed observés à froid | déduit (code + plancher fsync ~0,25–1 ms/op mesuré par N7 §5) |
| Overhead binding PyO3/asyncio | < 5 ms (inclus dans le plancher 55 ms) | déduit |
| **Écart résiduel du run N6 publié** (357,8 − ~86) | **~270 ms — environnemental, pas intrinsèque** | **mesuré par différence** ; cause précise : hypothèse (§4) |

Le q1/q4 de la sonde 2 (115 → 61 ms) montre le moteur qui **accélère** en se
réchauffant après la phase d'insertion — l'inverse du run N6 (`growth_ratio`
recall **1,55**, 341 → 529 ms à N constant, `out/basemyai-n6.json`), ce qui
confirme que la dérive de N6 n'était pas une propriété des données.

## 4. Pourquoi le run N6 était ~4× plus lent (hypothèses, honnêtement)

Faits (données brutes `out/basemyai-n6.json`) : les **6 premiers**
`remember` du run N6 sont à 96–116 ms — exactement le régime de la
reproduction — puis sautent durablement à ~360–530 ms dès l'item 7 (~0,65 s
après le début). Les minima du run (95–96 ms) prouvent que la machine
*pouvait* tenir ~95 ms ce soir-là. La phase recall dérive de 341 à 529 ms à
N constant, avec des outliers à 4 143 ms.

Hypothèses compatibles (non départageables a posteriori) :

- **Priorité/QoS de processus** : run lancé à 04h16, vraisemblablement en
  tâche de fond ; sous Windows 11 sur CPU hybride (i7-13620H, P+E cores), un
  process classé « efficiency » (EcoQoS) est cantonné aux E-cores — un ×3–5
  typique sur de l'inférence AVX. Le basculement net après ~0,6 s ressemble
  à une reclassification, pas à du throttling thermique (trop rapide).
- **Contention** : Ollama résident (exigé par le protocole, VRAM+RAM sur une
  machine à 13,7 GiB), autres tâches de la session nocturne.

Ce qui est **exclu** par les mesures : rechargement du modèle par appel
(open unique), build debug (§1), coût intrinsèque du moteur (§3), croissance
liée aux données (dérive à N constant, et inverse en reproduction).

## 5. Pourquoi Mem0+Qdrant est à ~94 ms dans ce même harnais

- **Embedding sur GPU, hors process** : `all-minilm` servi par Ollama
  (llama.cpp), toutes couches sur la RTX 4060. Le coût côté Python =
  un POST HTTP localhost + l'inférence GPU (quelques ms). (Déduit de la
  config `run.py` l.185–217 + notes machine ; pas re-mesuré ici — aucun
  appel réseau dans cette analyse.)
- **Recherche triviale à cette échelle** : Qdrant local in-process sur
  100–500 vecteurs. (Déduit.)
- Le ~94 ms est remarquablement stable (p95 115–118 ms) et **identique entre
  le run Docker de juin et le run embarqué de juillet** (92,6 vs 93,7 ms) —
  la constante est l'aller-retour Ollama + l'orchestration Python de Mem0,
  pas Qdrant. (Mesuré, en comparant les deux rapports.)
- Mem0 ne fait par requête **ni** filtre de validité temporelle, **ni**
  écriture `last_access`, **ni** chiffrement au repos du store interrogé —
  le périmètre par appel est plus mince (déjà noté par N6 §« Reading the
  numbers honestly »).

Point clé : à conditions saines, BaseMyAI (86,3 ms mean, 61 ms à chaud,
embed **CPU**) fait jeu égal avec ce chiffre-là (93,7–95,2 ms, embed
**GPU**). Le moteur n'est pas le problème ; le run publié l'était.

## 6. Remédiations, classées impact/effort

| # | Remédiation | Impact | Effort | Nature |
|---|---|---|---|---|
| 1 | **Re-runner N6 en avant-plan contrôlé et republier avec la décomposition embed vs moteur** : ajouter au harnais un mode qui chronomètre séparément l'embed de la requête (ou publie un `recall` sur DB vide comme plancher embed) + noter priorité process/état machine dans le protocole. Les chiffres de ce doc montrent ~86 ms mean / 61 ms à chaud — parité avec Mem0 | Très fort (positionnement : le claim honnête devient « à embedder égal, le moteur est au moins compétitif ; le KNN natif coûte ~2–7 ms ») | Faible (protocole + quelques lignes de harnais) | Mesure/narratif |
| 2 | **Cache LRU d'embeddings de requêtes** dans `Memory` (clé : texte normalisé) + **API `recall_by_vector`** publique pour les appelants qui ont déjà le vecteur. Un agent répète massivement ses requêtes ; hit = recall à ~5 ms. Déjà identifié par `docs/strategy/2026-06-21-post-refactor-reassessment.md` §9 | Fort sur charge réelle (élimine les ~58 ms sur hit) | Faible/moyen (invalidation triviale : la requête n'est pas mutée par l'état du store) | Code |
| 3 | **Combler l'écart ADR-010 : variante de build GPU** (feature `cuda` Candle propagée `basemyai-core/embed` → wheels `+cu12x`), et à défaut **MKL/accélération CPU** (`candle` feature `mkl`) ou dtype réduit. Aujourd'hui le fallback CPU est silencieux et la détection NVML (`cuda-detect`) ne sert à rien côté embedder. Attendu : 55–60 ms → ~5–15 ms | Fort (plancher divisé par ~4–10) | Moyen/élevé (matrice de build wheels, CI, validation GPU réelle — déjà « reste ouvert » dans `docs/status.md`) | Code/CI |
| 4 | **`spawn_blocking` autour de `embedder.embed()`** dans `Memory` (contrat PRD REQ-004) — sans effet sur la latence séquentielle, mais évite de bloquer un worker tokio sous concurrence (REST/MCP) | Moyen (débit concurrent, pas latence unitaire) | Faible | Code |
| 5 | **`touch` `last_access` hors chemin critique** : batcher/différer l'écriture (ou group commit, déjà prévu N13 §10.4) — un recall paie aujourd'hui un fsync WAL. ~1–5 ms/appel seulement, mais c'est une écriture durable par lecture | Faible/moyen | Moyen (sémantique `last_access` à préserver pour l'oubli adaptatif) | Code |
| 6 | ~~ONNX/fastembed~~ — écarté par ADR-003 (toolchain C fragile Windows = risque produit n°1) ; la voie quantization/GGUF-like **dans Candle** peut se réévaluer en V2 multi-modèles | — | — | Rappel de décision |

**Remédiation narrative (si publication avant re-run)** : ne plus citer
« recall 358 ms » comme coût intrinsèque. La formulation défendable, ancrée
dans les mesures de ce doc : *« le moteur répond en ~2–7 ms ; l'embedding de
requête CPU coûte ~55–60 ms ; bout-en-bout ~60–90 ms à chaud — au niveau de
Mem0+Qdrant dont l'embedding tourne sur GPU »* — et publier le split pour
que le claim soit vérifiable.

## 7. Limites de cette analyse

- La reproduction (§3) est **un run unique**, machine de dev en journée —
  même discipline « chiffres réels, pas de moyenne multi-essais » que
  N5.5/N7, mêmes limites.
- La sonde mesure le plancher embed via « recall sur DB vide », qui inclut
  l'overhead binding : le coût Candle pur est ≤ 55–60 ms (borne supérieure).
- La cause précise de l'inflation du run N6 (§4) reste une **hypothèse** ;
  ce qui est établi, c'est qu'elle est environnementale et reproductiblement
  absente à conditions saines.
- Aucune mesure Ollama/Qdrant refaite ici (zéro réseau) : le §5 s'appuie sur
  la config du harnais et les deux rapports existants.
