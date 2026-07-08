# ADR-016 — AnythingLLM comme backend LLM de premier rang via API workspace-chat

**Statut** : ✅ Accepted | **Date** : 2026-06
**Amende** : ADR-013 (AnythingLLM n'est plus « détection seule » : c'est un backend d'inférence à part entière).

**Contexte**

ADR-013 a fait d'AnythingLLM un cas particulier : il est détecté (via `GET /api/ping`) mais exclu de `best_llm_option` avec `ram_mb = None` et ce commentaire : *« proxy multi-provider, non utilisable directement pour l'inférence »*.

Ce jugement était correct pour le cas général (AnythingLLM ne répond pas à `POST /v1/chat/completions`), mais trop restrictif : AnythingLLM expose une **API workspace-chat propre** authentifiée, `POST /api/v1/workspace/{slug}/chat`, qui :

1. Accepte un corps `{"message": "...", "mode": "chat"}` + header `Authorization: Bearer <api_key>`.
2. Retourne `{"textResponse": "...", "metrics": {...}}` — pas de format OpenAI.
3. Délègue au backend LLM **configuré dans le workspace** (Ollama, LM Studio, OpenAI, Anthropic…).
4. Répond en **moins de 50 ms** (mesure sur `qwen3-vl:4b-instruct` local via Ollama).

L'extraction JSON structurée a été validée en E2E (13 juin 2026) : le modèle `qwen3-vl:4b-instruct` via AnythingLLM extrait correctement des faits d'épisodes au format `{"facts":[{"subject":...,"predicate":...,"object":...,"confidence":...}]}`.

**La friction majeure d'ADR-013 était l'absence d'API key et de workspace slug dans le flux de détection automatique.** Ces informations ne sont pas découvrables sans authentification ; elles doivent être fournies explicitement par l'utilisateur (variables d'environnement ou config).

**Décision**

**1 — Nouveau backend `AnythingLlmBackend`** dans `basemyai/src/provision/llm.rs` :

```rust
pub struct AnythingLlmBackend {
    client:         Client,
    base_url:       String,
    workspace_slug: String,  // slug du workspace AnythingLLM (ex. "mon-espace-de-travail")
    timeout:        Duration,
}

impl AnythingLlmBackend {
    pub fn new(base_url: &str, workspace_slug: &str, api_key: &str) -> Self;
    pub fn with_timeout(mut self, timeout: Duration) -> Self;
}

#[async_trait]
impl LlmInference for AnythingLlmBackend {
    async fn complete(&self, prompt: &str) -> Result<String>;
    fn model_id(&self) -> &str;   // retourne le workspace_slug
}
```

Corps de la requête :

```json
{ "message": "<prompt>", "mode": "chat" }
```

Réponse parsée via `text_response` (champ `"textResponse"` JSON). Erreur explicite si `textResponse` est `null` ou vide.

**2 — Variables d'environnement pour la config AnythingLLM** :

| Variable | Usage |
| --- | --- |
| `BASEMYAI_ANYTHINGLLM_URL` | URL de base (défaut : `http://localhost:3001`) |
| `BASEMYAI_ANYTHINGLLM_KEY` | Clé API Bearer (obligatoire si fallback activé) |
| `BASEMYAI_ANYTHINGLLM_WORKSPACE` | Slug du workspace (obligatoire si fallback activé) |

**3 — Mise à jour de `choose_llm()`** :

La politique de sélection devient **à deux niveaux** :

```text
Niveau 1 (hardware-aware, inchangé)
  detect_llm_options() → best_llm_option() → OpenAiCompatBackend
  Si un modèle direct tient en RAM → retourner ce backend.

Niveau 2 (fallback AnythingLLM, si niveau 1 échoue)
  Si les trois variables d'env. BASEMYAI_ANYTHINGLLM_* sont définies
      → AnythingLlmBackend::new(url, slug, key)
  Sinon → Err(MemoryError::Inference) avec hint (inchangé + nouvelle ligne d'aide
           "ou configurer BASEMYAI_ANYTHINGLLM_KEY + BASEMYAI_ANYTHINGLLM_WORKSPACE")
```

Le niveau 2 n'exige **pas** de connaître le modèle ou sa RAM : AnythingLLM gère ça en interne. `LlmProvision.ram_mb` vaut `None` dans ce cas.

**4 — `probe_anythingllm` inchangée** : la détection reste sans auth (simple `GET /api/ping`). Les informations de configuration (clé, workspace) ne transitent jamais dans la phase de découverte automatique.

**5 — Test E2E gated** : `tests/consolidation_e2e.rs`, annoté `#[ignore]` (jamais exécuté en CI), déclenché manuellement par `cargo test -- --ignored consolidation_e2e`. Lit les trois variables d'env., crée des épisodes, appelle `consolidate()`, vérifie que des entités et arêtes apparaissent dans le graphe. C'est la première exécution E2E du pipeline consolidation→graphe contre un vrai LLM.

**Conséquences**

✅ AnythingLLM devient un backend d'inférence à part entière — aucune modification du code consommateur (`consolidate` injecte un `&dyn LlmInference`, agnostique).
✅ Couvre le cas où Ollama n'est pas accessible directement mais AnythingLLM tourne en proxy (cas fréquent : Ollama configuré dans AnythingLLM mais non exposé sur le port 11434).
✅ Zéro regression : la politique niveau 1 (Ollama/LM Studio directement) est prioritaire et inchangée.
✅ `LlmInference` reste le seul contrat — `AnythingLlmBackend` est un détail d'implémentation.
✅ La validation E2E de la consolidation est enfin possible sans Ollama exposé.
⚠️ Le modèle effectivement utilisé est opaque pour BaseMyAI : `model_id()` retourne le `workspace_slug`, pas le nom du modèle. `ram_mb = None` dans `LlmProvision`.
⚠️ Le prompt passe par le RAG d'AnythingLLM (similarité de workspace) avant d'atteindre le LLM — si le workspace contient des documents, ils peuvent polluer la réponse. Recommandation : utiliser un workspace vide ou dédié BaseMyAI.
⚠️ `mode: "chat"` conserve l'historique de session côté AnythingLLM (par `sessionId`). La consolidation n'envoie pas de `sessionId` → chaque appel est sans état de son côté, mais AnythingLLM crée une nouvelle session à chaque requête. Sans `sessionId`, les sessions orphelines s'accumulent. **Mitigation V2** : passer un `sessionId` fixe par agent (ex. `"basemyai-{agent_id}"`).
⚠️ AnythingLLM n'expose pas `GET /v1/models` → exclu de la détection automatique hardware-aware (`ram_mb = None` reste correct). La RAM consommée dépend du backend sous-jacent configuré dans AnythingLLM.

**Alternatives rejetées**

Tenter `/api/v1/openai/chat/completions` — retourne `401` en mode single-user (testé 13 juin 2026) ; ce endpoint nécessite le mode multi-utilisateur avec un JWT distinct. Incompatible avec la configuration par défaut d'AnythingLLM.

Lire la config AnythingLLM (`~/.config/anythingllm/...`) pour extraire automatiquement la clé — dépendance à des détails d'implémentation internes non documentés, fragile et plateforme-spécifique.

Forcer l'utilisateur à configurer Ollama directement plutôt qu'AnythingLLM — UX dégradée pour qui a déjà AnythingLLM en service ; le proxy ajoute des features (RAG sur workspace, logs UI) que l'utilisateur a peut-être intentionnellement choisies.

Ajouter AnythingLLM au niveau 1 (hardware-aware) — impossible sans connaître le modèle et sa RAM, informations non accessibles sans auth. Le niveau 2 (fallback explicitement configuré) est le bon modèle.
