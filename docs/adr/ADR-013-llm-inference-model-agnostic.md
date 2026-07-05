# ADR-013 — Inférence LLM model-agnostic + provisioning hardware-aware

**Statut** : ✅ Accepted | **Date** : 2026-06

**Contexte**

La consolidation (ADR-012) exige un LLM pour l'extraction structurée. Même philosophie qu'ADR-010 (provisioning hardware-aware pour les embeddings) et ADR-003 (l'`Embedder` ne télécharge jamais) : le composant d'inférence ne doit pas savoir quel backend il utilise, et le choix du modèle doit être adapté à la machine. En 2026, les LLM locaux sont accessibles via de nombreux runners incompatibles en apparence (Ollama, LM Studio, Jan, vLLM, KoboldCPP, LocalAI, GPT4All) mais convergent tous sur l'API OpenAI `/v1/chat/completions`.

**Décision**

**Trait model-agnostic `LlmInference`** (dans `basemyai`, pas dans `basemyai-core`) :

```rust
#[async_trait]
pub trait LlmInference: Send + Sync {
    async fn complete(&self, prompt: &str) -> Result<String>;
    fn model_id(&self) -> &str;
}
```

Injecté comme `Embedder` — le pipeline de consolidation ne sait pas quel backend il appelle.

**Backend universel `OllamaBackend`** — `POST /v1/chat/completions` (API OpenAI-compat). Couvre sans modification de code : Ollama, LM Studio, Jan, vLLM, KoboldCPP, LocalAI, GPT4All. Un seul backend pour 8 runners locaux.

**Table `KNOWN_MODELS`** (20 modèles, juin 2026, trié RAM décroissant) — des modèles 1B à 70B couvrant Llama 3.3, Qwen 3, Gemma 3, Devstral, DeepSeek-R1 distills, Phi-4, Mistral. RAM en Mo (Q4_K_M), mise à jour périodique.

**Détection `detect_llm_options()`** — sonde 8 backends sans jamais échouer (timeout 1 s) :

| Backend     | Port  | Méthode                  |
|-------------|-------|--------------------------|
| Ollama      | 11434 | `GET /api/tags`          |
| LM Studio   | 1234  | `GET /v1/models`         |
| Jan         | 1337  | `GET /v1/models`         |
| GPT4All     | 4891  | `GET /v1/models`         |
| KoboldCPP   | 5001  | `GET /v1/models`         |
| vLLM        | 8000  | `GET /v1/models`         |
| LocalAI     | 8080  | `GET /v1/models`         |
| AnythingLLM | 3001  | `GET /api/ping` (sonde seule) |

AnythingLLM est un **proxy RAG multi-provider**, non utilisable directement pour l'inférence : détecté et signalé, mais exclu de `best_llm_option` (`ram_mb = None`).

**Sélection `best_llm_option()`** — budget mémoire = `VRAM × 90 %` (si GPU) sinon `RAM × 60 %`. Parmi les options qui rentrent dans le budget, prend la plus lourde (= la plus capable). Zéro auto-install : `choose_llm()` retourne `Err` avec hint d'installation si rien ne convient.

**Conséquences**

✅ Un seul backend (`OllamaBackend`) couvre tous les LLM locaux OpenAI-compat sans branching.
✅ Zéro auto-download / zéro auto-install : même philosophie qu'ADR-010.
✅ 20 modèles couvrant toute la gamme (700 Mo → 45 Go) : chaque machine trouve quelque chose.
✅ Detection never fails : retourne une liste vide si rien n'est actif, ne panique pas.
✅ AnythingLLM signalé avec un message d'aide clair (configure Ollama / LM Studio comme backend sous-jacent).
⚠️ KNOWN_MODELS est une table statique — à mettre à jour à chaque nouvelle génération de modèles.
⚠️ Les tags Ollama sont la convention (ex. `qwen3:14b`) ; les autres backends peuvent nommer les modèles différemment → `ram_for()` fait une correspondance par préfixe, approximative pour les runners non-Ollama.
⚠️ `choose_llm()` retourne `OllamaBackend` en dur — pour un futur backend sans API OpenAI-compat, il faudra un variant ou une factory.

**Alternatives rejetées**

Backend Ollama seul — exclut LM Studio, Jan, vLLM, LocalAI et les autres runners courants en 2026.

Détection automatique + installation silencieuse du modèle (`ollama pull`) — viole ADR-010 (zéro action non consentie).

API d'inférence cloud (OpenAI, Anthropic) — viole le privacy-first / 100 % local.

Branching par backend dans le pipeline — `OllamaBackend` + `/v1/chat/completions` unifie tout ; le branching est une fausse complexité.
