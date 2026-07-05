# ADR-018 — Consolidation pilotée par l'agent — politique d'inférence à niveaux (supersède ADR-017)

**Statut** : ✅ Accepted | **Date** : 2026-06-13
**Supersède** : ADR-017. **Amende** : ADR-012, ADR-013, ADR-016.

**Contexte**

ADR-017 pariait sur le **sampling MCP** comme levier « plug-and-play » : le serveur emprunte le LLM du client via `sampling/createMessage`. Le test E2E réel dans Claude Code (v2.1.176, 13 juin 2026) a invalidé le pari :

- **Claude Code n'implémente pas le sampling** : `consolidate` remonte `MCP error -32601: Method not found`. C'est documenté comme *feature request* ouvert ([anthropics/claude-code#1785]), pas un bug de notre code.
- **Le sampling est déprécié dans le protocole** : SEP-2577 (2026-07-28) déprécie Roots, Sampling et Logging. *« New implementations should NOT adopt it. »*
- **Aucun autre client majeur** (Claude Desktop, Cursor, Windsurf, ChatGPT) ne confirme le support.

Or BaseMyAI tourne le plus souvent **dans un agent qui est lui-même un LLM** (Claude dans Claude Code). Le bon levier n'est donc pas de *demander* une complétion au client (sampling), mais d'**inverser le contrôle** : laisser l'agent faire l'extraction avec son propre raisonnement, et n'exposer côté serveur que la préparation (épisodes + consigne) et la persistance. C'est universel (outils + prompts MCP, supportés partout), non déprécié, et de meilleure qualité (c'est le modèle de l'agent, pas un petit LLM local).

**Décision**

**1 — `consolidate()` du crate `basemyai` scindé en briques réutilisables**

`consolidation_prompt(memory) -> Option<ConsolidationInput>` (lit les épisodes valides + bâtit le prompt), `parse_extraction(raw) -> Extraction`, `apply_extraction(memory, &Extraction) -> ConsolidationReport` (peuple le graphe + promeut les faits, idempotent). `consolidate(memory, &dyn LlmInference)` compose les trois — **signature inchangée, rétrocompatible**. Les types `Extraction` / `ExtractedEntity` / `ExtractedRelation` deviennent publics (sans dépendance à `schemars` : le crate mémoire reste pur).

**2 — Outil MCP `consolidate` : politique d'inférence à niveaux**

```text
1. Sampling MCP   — SEULEMENT si le client annonce la capability `sampling`
                    (rare ; déprécié). Vérifié via peer.peer_info().capabilities.
2. LLM local      — choose_llm() : Ollama/LM Studio/AnythingLLM détecté ou env.
                    Autonome : le serveur fait l'extraction, l'agent reçoit le bilan.
3. Piloté agent   — sinon : renvoie status="extraction_required" + episodes +
                    instructions. L'AGENT appelant extrait avec son propre LLM,
                    puis persiste via `consolidate_apply`. Universel, zéro install.
```

**3 — Nouvel outil MCP `consolidate_apply`** — reçoit `agent_id` + `facts`/`entities`/`relations` (types `JsonSchema` propres au crate MCP, convertis en `basemyai::Extraction`), appelle `apply_extraction`. Idempotent.

**4 — Nouveau prompt MCP `consolidate_memory`** — pilote le flux de bout en bout en mode interactif : `/mcp__basemyai__consolidate_memory agent_id=X` injecte les épisodes + la consigne ; l'agent extrait et appelle `consolidate_apply`.

**5 — Annotations d'outils** (best-practice MCP) sur les 8 outils : `read_only_hint`, `destructive_hint`, `idempotent_hint`, `open_world_hint=false` (mémoire = monde fermé, local).

**Conséquences**

✅ La consolidation marche **dans Claude Code** (et tout client MCP) sans serveur LLM ni clé, en empruntant le LLM de l'agent — le vrai plug-and-play, supérieur au sampling (qualité du modèle de l'agent).
✅ Le mode autonome (worker de fond, SDK, REST) garde le LLM local/cloud (pas d'agent pour piloter).
✅ Plus de dépendance à une primitive dépréciée ; le sampling reste branché en option opportuniste à coût nul (sauté si non annoncé).

⚠️ Le mode « piloté agent » consomme des tokens de l'agent et suppose qu'il suit la consigne (extraire → `consolidate_apply`). La description de l'outil et le prompt cadrent ce flux.
⚠️ La sélection du niveau dépend de l'environnement (un LLM local détecté prime sur l'agent-driven) ; documenté. Un utilisateur voulant la qualité de l'agent malgré un LLM local utilise le prompt `consolidate_memory`.
⚠️ Le mode cloud opt-in BYOK reste **déféré** (mode hérité d'ADR-017, sous garde-fous).

**Alternatives rejetées**

Garder le sampling en primaire (ADR-017) — invalidé : non supporté par le client cible, déprécié dans le protocole.

Forcer un LLM local embarqué (llama.cpp/mistral.rs in-process) pour l'autonomie totale — lourd ; reporté V2 (le modèle Candle actuel est *embedding-only*, incapable de génération). L'agent-driven couvre le besoin interactif sans ce coût.

Élicitation MCP pour l'extraction — l'élicitation demande une saisie **humaine** structurée, pas une génération LLM ; inadaptée à l'extraction de faits.

[anthropics/claude-code#1785]: https://github.com/anthropics/claude-code/issues/1785
