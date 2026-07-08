# ADR-017 — Consolidation par sampling MCP (emprunter le LLM du client) + politique des modes LLM

**Statut** : ⛔ Superseded par **ADR-018** | **Date** : 2026-06
**Amende** : ADR-013, ADR-016 (ajoute une 3ᵉ source d'inférence ; clarifie la politique de bout en bout).

> **Superseded (13 juin 2026)** : le sampling MCP n'est **pas** le levier plug-and-play
> escompté. Vérifié sur sources officielles : Claude Code ne l'implémente pas (feature
> request ouvert, `-32601 Method not found`), et la primitive est **dépréciée** dans le
> protocole (SEP-2577, 2026-07-28). ADR-018 inverse la priorité : LLM côté serveur si
> disponible, sinon **consolidation pilotée par l'agent** (universelle), sampling devenu
> simple option opportuniste. `SamplingBackend` et l'outil `consolidate` restent, recâblés.

**Contexte**

La consolidation (ADR-012) exige un LLM. ADR-013/016 ont câblé deux sources : serveur local OpenAI-compat (Ollama, LM Studio…) et AnythingLLM. Les deux supposent que **l'utilisateur a installé et configuré un LLM** quelque part. Pour une part importante de l'audience visée — les utilisateurs de **Claude Code, Claude Desktop, Cursor, Windsurf, ChatGPT Desktop, Codex** — c'est une friction inutile : **ils ont déjà un LLM**, celui de leur agent. Leur demander d'installer Ollama *en plus* pour que BaseMyAI consolide est absurde.

Le rôle de BaseMyAI vis-à-vis de ces agents n'est pas de *consommer* un LLM : c'est de **leur fournir une mémoire persistante**. Le canal est **MCP** (`basemyai-mcp`, déjà implémenté). Or le protocole MCP expose une primitive **`sampling/createMessage`** : un serveur MCP peut demander au client de produire une complétion LLM. C'est exactement le levier manquant — le serveur **emprunte le cerveau du client**.

Vérifié (13 juin 2026) : `rmcp 1.7` expose `Peer<RoleServer>::create_message(CreateMessageRequestParams) -> CreateMessageResult` côté serveur, et `ClientHandler::create_message` côté client. Un test E2E in-memory (serveur + client MCP reliés par duplex) valide le chemin complet `remember → consolidate (sampling) → graphe peuplé → recall_graph`.

**Décision**

**1 — `SamplingBackend` (dans `basemyai-mcp`, pas `basemyai`)**

Un backend qui implémente `basemyai::LlmInference` en déléguant `complete()` à `peer.create_message(...)`. Il vit dans `basemyai-mcp` (le seul crate qui dépend de `rmcp`) : **le crate mémoire reste agnostique de MCP**. `model_id()` retourne `"mcp-sampling"` (le modèle réel est choisi par le client, connu seulement au retour via `CreateMessageResult::model`).

**2 — Outil MCP `consolidate`**

Nouvel outil : l'agent l'appelle avec un `agent_id` ; le handler récupère le `Peer` depuis le `RequestContext<RoleServer>`, construit un `SamplingBackend`, et exécute `consolidate(memory, &backend)`. Le sampling se produit **pendant l'appel d'outil** : le serveur sous-demande au client, le LLM du client extrait, le graphe est peuplé. Déclenchement **explicite** (déterministe, observable) ; le worker de fond avec `Peer` capturé est reporté en V2 (cycle de vie du peer + multi-sessions HTTP à cadrer).

**3 — Politique des sources de consolidation, ordonnée et explicite**

```text
1. Sampling MCP    — si BaseMyAI tourne comme serveur MCP (outil `consolidate`)
                     → emprunte le LLM du client. Zéro install, zéro clé.
2. LLM local       — Ollama / LM Studio / AnythingLLM (ADR-013/016)
                     → détection hardware-aware, reste sur la machine.
3. Cloud opt-in    — Claude API / OpenAI API (BYOK), UNIQUEMENT si configuré
                     explicitement → sort de la machine (voir implications).
4. Indisponible    — la mémoire (remember/recall/graphe manuel) fonctionne ;
                     la consolidation auto est simplement absente.
```

**4 — Implications de confidentialité, à exposer clairement (exigence produit)**

Chaque mode a un périmètre de données différent ; le produit DOIT le rendre lisible (doc + message au setup) :

| Mode | Où partent les épisodes | Privacy-first ? | Consentement |
| --- | --- | --- | --- |
| **Sampling MCP** | Vers le client MCP que l'utilisateur a **déjà** choisi (Claude Code, ChatGPT…). BaseMyAI n'impose aucun tiers. | ✅ Oui — c'est *son* client, *son* modèle, *son* choix (local ou cloud, décidé par lui). MCP prévoit un **consentement humain** au sampling côté client. | Implicite (le client a sa propre UX de consentement). |
| **LLM local** | Nulle part : reste sur la machine (localhost). | ✅ Oui — 100 % local, le pilier d'origine. | Implicite (serveur local lancé par l'utilisateur). |
| **Cloud opt-in (BYOK)** | Vers Anthropic / OpenAI (selon la clé fournie). Les épisodes — données les plus sensibles — **quittent la machine**. | ⚠️ **Non** — rompt le 100 % local. À n'activer qu'en connaissance de cause. | **Explicite obligatoire** : variable d'env. dédiée + avertissement au démarrage. Jamais de défaut, jamais silencieux. |

Le mode cloud n'est jamais le défaut et ne s'active jamais par simple présence d'une clé d'environnement générique : il exige une variable **dédiée et non ambiguë** (`BASEMYAI_CLOUD_LLM_OPTIN=1` + clé), et émet un avertissement explicite « vos épisodes sont envoyés à `<provider>` » au premier usage.

**Conséquences**

✅ **Vrai plug-and-play** pour les agents MCP : `claude mcp add basemyai …` suffit, la consolidation marche sans aucun LLM installé ni clé.
✅ Le crate `basemyai` **reste agnostique de MCP** : `SamplingBackend` est dans `basemyai-mcp`, derrière le trait `LlmInference`.
✅ Un seul backend de sampling couvre **tous** les hôtes MCP (Claude Code/Desktop, Cursor, Windsurf, ChatGPT Desktop…).
✅ Le sampling reste **privacy-first** : la donnée passe par le client choisi par l'utilisateur, pas par un tiers imposé par BaseMyAI.
✅ La politique à 4 niveaux dégrade proprement : il y a toujours une réponse claire (jusqu'au mode « consolidation absente mais mémoire fonctionnelle »).
⚠️ Le sampling exige une **session MCP active** : il ne marche pas pour les consommateurs SDK Python/Node/REST standalone (eux relèvent des modes local ou cloud).
⚠️ Le client peut **refuser** le sampling (consentement humain) → `complete` remonte une erreur claire ; l'appelant peut retomber sur un autre mode.
⚠️ Le modèle réel du sampling est **opaque** (`model_id = "mcp-sampling"`) et sa qualité dépend du client — un petit modèle local côté client donnera une extraction plus pauvre.
⚠️ Le mode cloud opt-in **viole le pilier 100 % local** : c'est un choix de l'utilisateur, encadré, jamais un défaut. Implémentation déférée (le backend Claude/OpenAI BYOK fera l'objet de son propre câblage, sous cette politique).

**Alternatives rejetées**

Consolidation en tâche de fond via sampling (worker périodique avec `Peer` capturé) — séduisant mais le `Peer` n'est valide que pendant une session ; multi-sessions HTTP et cycle de vie à cadrer. Reporté V2 ; l'outil explicite couvre le besoin V1.

Mettre `SamplingBackend` dans `basemyai` — importerait `rmcp` dans le crate mémoire, violant son agnosticité (ADR-001). Il vit dans `basemyai-mcp`, derrière le trait.

Cloud par défaut / activé par une clé générique (`OPENAI_API_KEY`…) — exfiltration silencieuse des données les plus sensibles. Inacceptable : le cloud est opt-in dédié et explicite, jamais déduit.

Forcer l'installation d'un LLM local pour tous — c'est la friction même que cet ADR supprime pour les utilisateurs d'agents MCP.
