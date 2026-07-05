# ADR-010 — Provisioning du modèle hardware-aware (setup intelligent)

**Statut** : ✅ Accepted | **Date** : 2026-06

**Contexte**

ADR-003 a tranché que l'`Embedder` n'auto-télécharge **jamais** le modèle : il reçoit un chemin local. Mais cela laisse ouverte une question : **qui** choisit le modèle, **lequel**, sur **quel device** (CPU / CUDA / Metal), et **quand** le fetch a lieu ?

Deux mauvaises réponses encadrent le bon choix :
- **Download silencieux au premier lancement** (l'approche du plan de dev initial) — réseau surprise qui viole « zéro réseau par défaut », et risque de tirer un modèle inadapté à la machine (OOM sur un laptop faible, ou inférence CPU alors qu'un GPU est dispo).
- **Configuration 100% manuelle** — hostile : l'utilisateur ne connaît pas forcément sa VRAM, son backend GPU, ni quel modèle convient.

Le bon modèle existe déjà dans l'écosystème : AnythingLLM résout ça avec un setup qui **détecte le matériel** et recommande/sélectionne le provider et le modèle adaptés. ForgeMyAI a déjà la même idée avec `fmyai setup` (détecte GPU/RAM, choisit le modèle).

**Décision**

Une étape de **setup explicite et hardware-aware**, orchestrée par le **produit** (jamais par `basemyai-core`), exposée via la CLI (`basemyai setup`) et le premier appel des SDK. Elle :

1. **Détecte les specs** : RAM totale, présence et VRAM d'un GPU (CUDA / Metal / ROCm), nombre de cœurs CPU, OS.
2. **Résout le device Candle** : CUDA > Metal > CPU selon disponibilité.
3. **Sélectionne le modèle d'embedding** : baseline garantie `all-MiniLM-L6-v2` (384 dims, CPU-friendly) **partout** ; un modèle plus capable n'est proposé que sur machine apte (réservé V2 — V1 reste sur le baseline pour préserver la compatibilité `.idx` côté ForgeMyAI, cf. ADR-003 / D1 de l'écosystème).
4. **Fetch explicite** du modèle (consentement utilisateur + vérification d'intégrité par checksum), mis en cache dans `~/.basemyai/models/`.
5. **Persiste le choix** (`model_id`, `dim`, device) dans la config, et le passe ensuite à `basemyai-core.Embedder` sous forme de **chemin + device déjà résolus**.

`basemyai-core` reste agnostique : il reçoit un chemin de modèle et un device, il n'a aucune logique de détection matérielle ni de sélection. Le mécanisme d'inférence est au core ; la décision de *quoi* charger est au produit (mécanisme au core, sens au consommateur).

```
basemyai setup           (ou 1ᵉʳ appel SDK si non configuré)
  ├─ détecte RAM / GPU / VRAM / cœurs / OS
  ├─ device := CUDA > Metal > CPU
  ├─ model  := all-MiniLM-L6-v2 (baseline V1)
  ├─ fetch explicite + checksum → ~/.basemyai/models/
  └─ persiste { model_id, dim, device }
              │
              ▼
basemyai-core.Embedder(model_path, device)   ← reçoit du résolu, ne décide rien
```

**Conséquences**

✅ Bon modèle / bon device pour chaque machine, sans configuration manuelle (façon AnythingLLM).
✅ Respecte « zéro réseau par défaut » : le seul fetch est dans le setup, explicite et consenti.
✅ `basemyai-core` reste agnostique : il reçoit chemin + device résolus, aucune détection matérielle dans le core.
✅ Le GPU est exploité s'il est présent (latence d'inférence réduite) ; repli CPU transparent sinon.
⚠️ La détection matérielle est plateforme-spécifique (NVML pour CUDA, Metal sur macOS, `/proc` + sysinfo sur Linux) → code conditionnel par OS, à tester sur les trois plateformes.
⚠️ V1 reste sur le **seul** modèle baseline pour préserver la compat `.idx` avec ForgeMyAI ; la sélection multi-modèles hardware-aware n'est pleinement active qu'en V2.
⚠️ Si l'utilisateur saute le setup, le premier usage échoue **proprement** avec un message « run `basemyai setup` » — jamais un download surprise.

**Alternatives rejetées**

Auto-download silencieux au premier lancement (plan de dev initial, Phase 2) — réseau surprise, viole « zéro réseau par défaut », peut choisir un modèle inadapté au matériel.

Modèle et device codés en dur — ignore le GPU sur une machine capable, ou provoque un OOM / une inférence trop lente sur une machine faible.

Configuration 100% manuelle (l'utilisateur fournit tout) — hostile ; il ne connaît pas forcément ses specs ML ni le modèle adéquat.

Détection matérielle **dans** `basemyai-core` — violerait l'agnosticité du core (la sélection de modèle est une décision produit, pas une primitive de stockage).
