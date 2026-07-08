// SPDX-License-Identifier: BUSL-1.1
//! Provisioning LLM **hardware-aware** (ADR-010 appliqué aux LLM, VISION §5.5).
//!
//! Même philosophie que le setup des embeddings :
//! - **Détection hardware** → profil machine (RAM, VRAM, cœurs).
//! - **Détection des serveurs LLM locaux actifs** → liste des modèles installés.
//! - **Sélection du meilleur modèle** qui tient dans la RAM/VRAM disponible.
//! - **Zéro auto-download silencieux** : on choisit parmi ce qui est déjà là. Si
//!   rien n'est disponible, on propose ce qui *pourrait* être installé (avec la
//!   commande Ollama correspondante) et on retourne une erreur claire.
//!
//! Les serveurs détectés exposent tous l'API compatible OpenAI
//! (`POST /v1/chat/completions`) — [`OpenAiCompatBackend`] couvre donc Ollama,
//! LM Studio, Jan, vLLM, KoboldCPP, LocalAI et tout serveur exposant cet
//! endpoint. ([`OllamaBackend`] reste un alias, nom historique d'ADR-013.)
//!
//! ## Backends sondés
//!
//! | Backend     | Port  | API type               |
//! |-------------|-------|------------------------|
//! | Ollama      | 11434 | `/api/tags` natif      |
//! | LM Studio   |  1234 | OpenAI-compat `/v1`    |
//! | Jan         |  1337 | OpenAI-compat `/v1`    |
//! | AnythingLLM |  3001 | détection seule        |
//! | GPT4All     |  4891 | OpenAI-compat partiel  |
//! | KoboldCPP   |  5001 | OpenAI-compat v2.6+    |
//! | vLLM        |  8000 | OpenAI-compat `/v1`    |
//! | LocalAI     |  8080 | OpenAI-compat `/v1`    |
//!
//! AnythingLLM est un proxy multi-provider : détecté et signalé, mais
//! non utilisable pour l'inférence directe (nécessite API key + workspace).
//! Configurer le backend sous-jacent (Ollama, LM Studio) pour l'inférence.

use std::time::Duration;

use reqwest::Client;
use serde::{Deserialize, Serialize};

use super::embedder::detect_hardware;
use crate::LlmInference;
use crate::{MemoryError, Result};

// ─── Table des modèles connus (Juin 2026) ────────────────────────────────────

/// Un modèle LLM local avec ses exigences hardware.
#[derive(Debug, Clone)]
pub struct KnownModel {
    /// Tag Ollama (ex. `"mistral:7b"`).
    pub ollama_tag: &'static str,
    /// RAM minimale estimée en Mo (Q4_K_M typique).
    pub ram_mb: u64,
    /// Description lisible.
    pub description: &'static str,
}

/// Table des modèles locaux supportés, du plus lourd au plus léger.
///
/// Le provisioning sélectionne le **premier** qui tient dans la RAM disponible
/// (parcours dans cet ordre = « le meilleur possible »). Mise à jour juin 2026.
pub const KNOWN_MODELS: &[KnownModel] = &[
    // ── Workstation / GPU haut de gamme (≥ 40 Go) ─────────────────────────
    KnownModel {
        ollama_tag: "llama3.3:70b",
        ram_mb: 45_600,
        description: "Llama 3.3 70B — haut de gamme, qualité supérieure à 3.1:70b, GPU requis",
    },
    // ── GPU haute gamme (≥ 20 Go) ──────────────────────────────────────────
    KnownModel {
        ollama_tag: "gemma3:27b",
        ram_mb: 22_500,
        description: "Gemma 3 27B — top open-source multimodal Google 2026",
    },
    KnownModel {
        ollama_tag: "qwen3:32b",
        ram_mb: 22_200,
        description: "Qwen 3 32B — raisonnement avancé, top open-source 2026",
    },
    // ── GPU milieu de gamme (12–16 Go) ─────────────────────────────────────
    KnownModel {
        ollama_tag: "devstral:24b",
        ram_mb: 14_400,
        description: "Devstral 24B — Mistral, spécialisé agents et génération de code",
    },
    KnownModel {
        ollama_tag: "gemma3:12b",
        ram_mb: 12_400,
        description: "Gemma 3 12B — excellent multilingue, code et instruction-following",
    },
    KnownModel {
        ollama_tag: "qwen3:14b",
        ram_mb: 10_700,
        description: "Qwen 3 14B — excellent code + raisonnement, recommandé milieu de gamme",
    },
    KnownModel {
        ollama_tag: "deepseek-r1:14b",
        ram_mb: 9_500,
        description: "DeepSeek-R1 14B distill — chain-of-thought, très précis en maths/code",
    },
    KnownModel {
        ollama_tag: "phi4:14b",
        ram_mb: 9_000,
        description: "Phi-4 14B Microsoft — fort raisonnement dans un modèle compact",
    },
    // ── GPU d'entrée de gamme / CPU haute mémoire (6–8 Go) ─────────────────
    KnownModel {
        ollama_tag: "mistral-nemo:12b",
        ram_mb: 7_200,
        description: "Mistral Nemo 12B — généraliste, fenêtre de contexte 128k",
    },
    KnownModel {
        ollama_tag: "llama3.3:8b",
        ram_mb: 6_200,
        description: "Llama 3.3 8B — qualité supérieure à 3.1:8b, même empreinte",
    },
    KnownModel {
        ollama_tag: "llama3.1:8b",
        ram_mb: 5_800,
        description: "Llama 3.1 8B — très répandu, fenêtre 128k, toujours pertinent",
    },
    KnownModel {
        ollama_tag: "qwen3:8b",
        ram_mb: 5_100,
        description: "Qwen 3 8B — excellent code et raisonnement, compact",
    },
    // ── CPU avec 8 Go RAM (4–5 Go) ─────────────────────────────────────────
    KnownModel {
        ollama_tag: "deepseek-r1:7b",
        ram_mb: 4_500,
        description: "DeepSeek-R1 7B distill — raisonnement chain-of-thought compact",
    },
    KnownModel {
        ollama_tag: "mistral:7b",
        ram_mb: 4_100,
        description: "Mistral 7B — référence CPU, très répandu, solide",
    },
    // ── CPU avec 6 Go RAM (3–4 Go) ─────────────────────────────────────────
    KnownModel {
        ollama_tag: "qwen3:4b",
        ram_mb: 3_600,
        description: "Qwen 3 4B — excellent rapport qualité/RAM 2026, meilleur choix léger",
    },
    KnownModel {
        ollama_tag: "gemma3:4b",
        ram_mb: 3_000,
        description: "Gemma 3 4B — multilingue, solide sur CPU",
    },
    KnownModel {
        ollama_tag: "phi4-mini",
        ram_mb: 2_800,
        description: "Phi-4 Mini 3.8B — fort raisonnement sur CPU, successeur de phi3.5",
    },
    // ── CPU avec 4 Go RAM (≤ 2 Go) ─────────────────────────────────────────
    KnownModel {
        ollama_tag: "llama3.2:3b",
        ram_mb: 2_000,
        description: "Llama 3.2 3B — basse mémoire, contexte multimodal",
    },
    KnownModel {
        ollama_tag: "gemma2:2b",
        ram_mb: 1_400,
        description: "Gemma 2 2B — très léger, qualité correcte",
    },
    KnownModel {
        ollama_tag: "llama3.2:1b",
        ram_mb: 700,
        description: "Llama 3.2 1B — minimaliste, CPU très contraint",
    },
];

// ─── Types de détection ──────────────────────────────────────────────────────

/// Type de serveur LLM local détecté.
///
/// `#[non_exhaustive]` : de nouveaux backends peuvent être ajoutés en minor.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum BackendKind {
    /// Ollama (`http://localhost:11434`) — runner multi-modèles le plus répandu.
    Ollama,
    /// LM Studio (`http://localhost:1234`) — OpenAI-compat, UI desktop.
    LmStudio,
    /// Jan (`http://localhost:1337`) — OpenAI-compat, open-source.
    Jan,
    /// GPT4All (`http://localhost:4891`) — OpenAI-compat partiel, offline.
    Gpt4All,
    /// KoboldCPP (`http://localhost:5001`) — OpenAI-compat depuis v2.6+.
    KoboldCpp,
    /// vLLM (`http://localhost:8000`) — OpenAI-compat, optimisé production.
    Vllm,
    /// LocalAI / llama.cpp server (`http://localhost:8080`) — OpenAI-compat.
    LocalAi,
    /// AnythingLLM (`http://localhost:3001`) — proxy/RAG multi-provider.
    ///
    /// **Non utilisable pour l'inférence directe** : il délègue à un backend
    /// (Ollama, LM Studio…). Si seul AnythingLLM est détecté, configurer son
    /// backend sous-jacent pour que BaseMyAI puisse l'atteindre directement.
    AnythingLlm,
    /// Tout autre serveur compatible OpenAI v1.
    OpenAiCompat,
}

/// Un modèle disponible localement, avec son backend et son coût mémoire.
#[derive(Debug, Clone)]
pub struct LlmOption {
    /// Tag du modèle, tel que connu par le serveur.
    pub model_id: String,
    /// URL de base du serveur (ex. `"http://localhost:11434"`).
    pub server_url: String,
    /// Type de backend.
    pub backend: BackendKind,
    /// RAM estimée en Mo (`None` si modèle inconnu de notre table).
    pub ram_mb: Option<u64>,
}

// ─── Réponses API ────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct OllamaTagsResponse {
    models: Vec<OllamaModelEntry>,
}

#[derive(Deserialize)]
struct OllamaModelEntry {
    name: String,
}

#[derive(Deserialize)]
struct OpenAiModelsResponse {
    data: Vec<OpenAiModelEntry>,
}

#[derive(Deserialize)]
struct OpenAiModelEntry {
    id: String,
}

// ─── Détection ───────────────────────────────────────────────────────────────

/// Délai de timeout court — si le serveur ne répond pas en 1 s, il est absent.
const DETECT_TIMEOUT: Duration = Duration::from_secs(1);

/// Détecte les serveurs LLM locaux actifs et les modèles qu'ils ont déjà
/// installés. **N'échoue jamais** : retourne un `Vec` vide si rien n'est trouvé.
///
/// Voir le tableau en tête de module pour la liste des backends et ports sondés.
pub async fn detect_llm_options() -> Vec<LlmOption> {
    let client = Client::builder().timeout(DETECT_TIMEOUT).build().unwrap_or_default();

    let mut out = Vec::new();
    // Ollama (port natif, API spécifique)
    out.extend(probe_ollama(&client, "http://localhost:11434").await);
    // Serveurs OpenAI-compat, par port
    out.extend(probe_openai_compat(&client, "http://localhost:1234", BackendKind::LmStudio).await);
    out.extend(probe_openai_compat(&client, "http://localhost:1337", BackendKind::Jan).await);
    out.extend(probe_openai_compat(&client, "http://localhost:4891", BackendKind::Gpt4All).await);
    out.extend(probe_openai_compat(&client, "http://localhost:5001", BackendKind::KoboldCpp).await);
    out.extend(probe_openai_compat(&client, "http://localhost:8000", BackendKind::Vllm).await);
    out.extend(probe_openai_compat(&client, "http://localhost:8080", BackendKind::LocalAi).await);
    // AnythingLLM : détection seule (proxy, non utilisable directement)
    out.extend(probe_anythingllm(&client, "http://localhost:3001").await);
    out
}

/// Sonde un serveur Ollama et retourne la liste de ses modèles installés.
async fn probe_ollama(client: &Client, base_url: &str) -> Vec<LlmOption> {
    let url = format!("{base_url}/api/tags");
    let Ok(resp) = client.get(&url).send().await else {
        return Vec::new();
    };
    let Ok(body) = resp.json::<OllamaTagsResponse>().await else {
        return Vec::new();
    };
    body.models
        .into_iter()
        .map(|m| LlmOption {
            ram_mb: ram_for(&m.name),
            model_id: m.name,
            server_url: base_url.to_string(),
            backend: BackendKind::Ollama,
        })
        .collect()
}

/// Sonde un serveur compatible OpenAI v1 (`GET /v1/models`) et retourne ses modèles.
async fn probe_openai_compat(client: &Client, base_url: &str, kind: BackendKind) -> Vec<LlmOption> {
    let url = format!("{base_url}/v1/models");
    let Ok(resp) = client.get(&url).send().await else {
        return Vec::new();
    };
    let Ok(body) = resp.json::<OpenAiModelsResponse>().await else {
        return Vec::new();
    };
    body.data
        .into_iter()
        .map(|m| LlmOption {
            ram_mb: ram_for(&m.id),
            model_id: m.id,
            server_url: base_url.to_string(),
            backend: kind.clone(),
        })
        .collect()
}

/// Détecte AnythingLLM via `GET /api/ping`. Retourne une sentinelle avec
/// `ram_mb = None` — `best_llm_option` la filtrera automatiquement.
/// Son rôle est d'enrichir les messages d'erreur de `choose_llm`.
async fn probe_anythingllm(client: &Client, base_url: &str) -> Vec<LlmOption> {
    let url = format!("{base_url}/api/ping");
    let Ok(resp) = client.get(&url).send().await else {
        return Vec::new();
    };
    if !resp.status().is_success() {
        return Vec::new();
    }
    vec![LlmOption {
        model_id: "anythingllm".to_string(),
        server_url: base_url.to_string(),
        backend: BackendKind::AnythingLlm,
        ram_mb: None,
    }]
}

/// Cherche la RAM estimée d'un tag dans [`KNOWN_MODELS`] (correspondance préfixe).
fn ram_for(tag: &str) -> Option<u64> {
    KNOWN_MODELS
        .iter()
        .find(|m| tag.starts_with(m.ollama_tag) || m.ollama_tag.starts_with(tag))
        .map(|m| m.ram_mb)
}

// ─── Sélection hardware-aware ────────────────────────────────────────────────

/// Parmi les `options` disponibles, sélectionne le **meilleur modèle** qui tient
/// dans la mémoire de la machine courante.
///
/// Budget mémoire : `total_ram_mb × 60 %` (laisse 40 % à l'OS + reste de l'app).
/// Si la machine a de la VRAM GPU, `VRAM × 90 %` est utilisé à la place.
///
/// Retourne `None` si aucune option ne tient, ou si `options` est vide.
/// Les entrées avec `ram_mb = None` (ex. AnythingLLM) sont toujours exclues.
#[must_use]
pub fn best_llm_option(options: &[LlmOption]) -> Option<&LlmOption> {
    let hw = detect_hardware();
    let budget_mb = hw.gpu_vram_mb.map(|v| v * 9 / 10).unwrap_or(hw.total_ram_mb * 6 / 10);

    options
        .iter()
        .filter(|o| o.ram_mb.is_some_and(|r| r <= budget_mb))
        .max_by_key(|o| o.ram_mb)
}

/// Modèles que l'on **pourrait** installer (pas encore dans `installed`), triés du
/// plus capable au plus léger, dans la limite du budget mémoire courant.
/// Sert à guider l'utilisateur vers `ollama pull <tag>`.
#[must_use]
pub fn propose_models_to_install(installed: &[LlmOption]) -> Vec<&'static KnownModel> {
    let hw = detect_hardware();
    let budget_mb = hw.gpu_vram_mb.map(|v| v * 9 / 10).unwrap_or(hw.total_ram_mb * 6 / 10);
    let installed_ids: Vec<&str> = installed.iter().map(|o| o.model_id.as_str()).collect();

    KNOWN_MODELS
        .iter()
        .filter(|m| m.ram_mb <= budget_mb && !installed_ids.iter().any(|id| id.starts_with(m.ollama_tag)))
        .collect()
}

// ─── Orchestration principale ─────────────────────────────────────────────────

/// Résultat de `choose_llm` : un backend prêt à l'emploi.
pub struct LlmProvision {
    /// Backend connecté, implémentant [`LlmInference`].
    pub backend: Box<dyn LlmInference>,
    /// Modèle sélectionné (tag ou slug workspace pour AnythingLLM).
    pub model_id: String,
    /// RAM estimée consommée par ce modèle en Mo.
    /// `None` pour AnythingLLM (modèle géré côté proxy).
    pub ram_mb: Option<u64>,
}

/// Détecte les serveurs locaux, choisit le meilleur modèle disponible et retourne
/// un backend prêt à l'emploi.
///
/// # Errors
/// - [`MemoryError::Inference`] si aucun serveur LLM local utilisable n'est actif,
///   avec des suggestions d'installation adaptées à la machine courante.
pub async fn choose_llm() -> Result<LlmProvision> {
    let options = detect_llm_options().await;

    // ── Niveau 1 : backend direct OpenAI-compat (hardware-aware) ─────────────
    if let Some(opt) = best_llm_option(&options) {
        return Ok(LlmProvision {
            backend: Box::new(OpenAiCompatBackend::new(&opt.server_url, &opt.model_id)),
            model_id: opt.model_id.clone(),
            ram_mb: opt.ram_mb,
        });
    }

    // ── Niveau 2 : fallback AnythingLLM (config explicite via env, ADR-016) ──
    if let Some(backend) = anythingllm_from_env() {
        let model_id = backend.workspace_slug.clone();
        return Ok(LlmProvision {
            backend: Box::new(backend),
            model_id,
            ram_mb: None,
        });
    }

    // ── Aucun backend — message d'aide contextuel ─────────────────────────────
    let has_anythingllm = options.iter().any(|o| o.backend == BackendKind::AnythingLlm);
    let usable: Vec<_> = options.iter().filter(|o| o.ram_mb.is_some()).collect();

    let hint = if usable.is_empty() {
        if has_anythingllm {
            "AnythingLLM détecté (port 3001). Pour l'utiliser comme backend d'inférence, \
             définissez BASEMYAI_ANYTHINGLLM_KEY et BASEMYAI_ANYTHINGLLM_WORKSPACE. \
             Alternativement, configurez Ollama ou LM Studio comme backend dans AnythingLLM."
                .to_string()
        } else {
            let proposals = propose_models_to_install(&[]);
            if proposals.is_empty() {
                "Aucun serveur LLM local détecté. Installez Ollama (https://ollama.com) \
                 puis `ollama pull <modèle>`. Ou configurez BASEMYAI_ANYTHINGLLM_KEY + \
                 BASEMYAI_ANYTHINGLLM_WORKSPACE pour utiliser AnythingLLM."
                    .to_string()
            } else {
                let tags: Vec<_> = proposals
                    .iter()
                    .take(3)
                    .map(|m| format!("`ollama pull {}`", m.ollama_tag))
                    .collect();
                format!(
                    "Aucun serveur LLM local détecté. Installez Ollama puis lancez : {}",
                    tags.join(" ou ")
                )
            }
        }
    } else {
        let proposals = propose_models_to_install(&options);
        if proposals.is_empty() {
            "Aucun modèle installé ne tient dans la mémoire disponible.".to_string()
        } else {
            let tags: Vec<_> = proposals
                .iter()
                .take(3)
                .map(|m| format!("`ollama pull {}` (~{} Mo) — {}", m.ollama_tag, m.ram_mb, m.description))
                .collect();
            format!("Modèles disponibles pour votre machine :\n{}", tags.join("\n"))
        }
    };

    Err(MemoryError::Inference(hint))
}

// ─── Backend OpenAI-compat universel ─────────────────────────────────────────

/// Timeout par défaut d'un appel d'inférence. Généreux : un prompt de
/// consolidation sur un petit modèle CPU peut prendre plusieurs minutes —
/// mais **borné** : un serveur local figé ne doit jamais bloquer la
/// maintenance indéfiniment.
const INFERENCE_TIMEOUT: Duration = Duration::from_secs(300);
/// Timeout d'établissement de connexion : un serveur local répond vite ou pas.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Backend LLM via **API compatible OpenAI v1** (`POST /v1/chat/completions`).
///
/// Couvre Ollama, LM Studio, Jan, vLLM, KoboldCPP, LocalAI et tout serveur
/// exposant cet endpoint sans modification.
pub struct OpenAiCompatBackend {
    client: Client,
    base_url: String,
    model: String,
    timeout: Duration,
}

/// Alias historique (nom d'ADR-013). Préférer [`OpenAiCompatBackend`] : le
/// backend parle à tout serveur OpenAI-compat, pas seulement Ollama.
pub type OllamaBackend = OpenAiCompatBackend;

// ─── Backend AnythingLLM (workspace-chat API) ─────────────────────────────────

/// Backend LLM via l'**API workspace-chat d'AnythingLLM**
/// (`POST /api/v1/workspace/{slug}/chat`).
///
/// Authentifié par Bearer token (`api_key`). Retourne le champ `textResponse`
/// de la réponse JSON. Le modèle effectif est déterminé par la configuration
/// du workspace côté AnythingLLM — BaseMyAI ne le connaît pas ; `model_id()`
/// retourne le `workspace_slug` comme identifiant opaque.
///
/// **Note (ADR-016)** : le prompt transite par le RAG du workspace. Si celui-ci
/// contient des documents, ils peuvent apparaître dans la réponse. Utiliser un
/// workspace vide ou dédié BaseMyAI pour la consolidation.
///
/// # Variables d'environnement
///
/// `choose_llm()` utilise automatiquement ce backend en fallback si les trois
/// variables suivantes sont définies :
///
/// - `BASEMYAI_ANYTHINGLLM_URL` (défaut : `http://localhost:3001`)
/// - `BASEMYAI_ANYTHINGLLM_KEY` (obligatoire)
/// - `BASEMYAI_ANYTHINGLLM_WORKSPACE` (slug du workspace, obligatoire)
pub struct AnythingLlmBackend {
    client: Client,
    base_url: String,
    workspace_slug: String,
    api_key: String,
    timeout: Duration,
}

impl AnythingLlmBackend {
    /// Construit un backend AnythingLLM.
    ///
    /// - `base_url` : URL de base, ex. `"http://localhost:3001"`.
    /// - `workspace_slug` : slug du workspace, ex. `"mon-espace-de-travail"`.
    /// - `api_key` : clé API Bearer.
    #[must_use]
    pub fn new(base_url: &str, workspace_slug: &str, api_key: &str) -> Self {
        Self {
            client: Client::builder()
                .connect_timeout(CONNECT_TIMEOUT)
                .build()
                .unwrap_or_default(),
            base_url: base_url.trim_end_matches('/').to_string(),
            workspace_slug: workspace_slug.to_string(),
            api_key: api_key.to_string(),
            timeout: INFERENCE_TIMEOUT,
        }
    }

    /// Remplace le timeout d'inférence (défaut : 300 s).
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

/// Lit la configuration AnythingLLM depuis les variables d'environnement
/// (`BASEMYAI_ANYTHINGLLM_URL`, `_KEY`, `_WORKSPACE`).
///
/// Retourne `None` si la clé ou le workspace slug sont absents.
#[must_use]
pub fn anythingllm_from_env() -> Option<AnythingLlmBackend> {
    let key = std::env::var("BASEMYAI_ANYTHINGLLM_KEY").ok()?;
    let slug = std::env::var("BASEMYAI_ANYTHINGLLM_WORKSPACE").ok()?;
    let url = std::env::var("BASEMYAI_ANYTHINGLLM_URL").unwrap_or_else(|_| "http://localhost:3001".to_string());
    Some(AnythingLlmBackend::new(&url, &slug, &key))
}

#[derive(Serialize)]
struct AnythingLlmChatRequest<'a> {
    message: &'a str,
    mode: &'static str,
}

#[derive(Deserialize)]
struct AnythingLlmChatResponse {
    #[serde(rename = "textResponse")]
    text_response: Option<String>,
    error: Option<String>,
}

#[async_trait::async_trait]
impl LlmInference for AnythingLlmBackend {
    async fn complete(&self, prompt: &str) -> Result<String> {
        let url = format!("{}/api/v1/workspace/{}/chat", self.base_url, self.workspace_slug);
        let body = AnythingLlmChatRequest {
            message: prompt,
            mode: "chat",
        };

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .timeout(self.timeout)
            .json(&body)
            .send()
            .await
            .map_err(|e| MemoryError::Inference(format!("AnythingLLM : requête échouée : {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(MemoryError::Inference(format!("AnythingLLM : HTTP {status} — {text}")));
        }

        let parsed: AnythingLlmChatResponse = resp
            .json()
            .await
            .map_err(|e| MemoryError::Inference(format!("AnythingLLM : réponse illisible : {e}")))?;

        if let Some(ref err) = parsed.error
            && !err.is_empty()
        {
            return Err(MemoryError::Inference(format!("AnythingLLM erreur : {err}")));
        }

        parsed
            .text_response
            .filter(|s| !s.is_empty())
            .ok_or_else(|| MemoryError::Inference("AnythingLLM : réponse vide (textResponse null)".into()))
    }

    fn model_id(&self) -> &str {
        &self.workspace_slug
    }
}

impl OpenAiCompatBackend {
    #[must_use]
    pub fn new(base_url: &str, model: &str) -> Self {
        Self {
            client: Client::builder()
                .connect_timeout(CONNECT_TIMEOUT)
                .build()
                .unwrap_or_default(),
            base_url: base_url.trim_end_matches('/').to_string(),
            model: model.to_string(),
            timeout: INFERENCE_TIMEOUT,
        }
    }

    /// Remplace le timeout d'inférence (défaut : 300 s). À allonger pour de
    /// très gros prompts sur CPU, à raccourcir pour des réponses interactives.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: [ChatMessage<'a>; 1],
    stream: bool,
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'static str,
    content: &'a str,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatChoiceMessage,
}

#[derive(Deserialize)]
struct ChatChoiceMessage {
    content: String,
}

#[async_trait::async_trait]
impl LlmInference for OpenAiCompatBackend {
    async fn complete(&self, prompt: &str) -> Result<String> {
        let url = format!("{}/v1/chat/completions", self.base_url);
        let body = ChatRequest {
            model: &self.model,
            messages: [ChatMessage {
                role: "user",
                content: prompt,
            }],
            stream: false,
        };

        let resp = self
            .client
            .post(&url)
            .timeout(self.timeout)
            .json(&body)
            .send()
            .await
            .map_err(|e| MemoryError::Inference(format!("requête LLM échouée : {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(MemoryError::Inference(format!("serveur LLM : HTTP {status} — {text}")));
        }

        let parsed: ChatResponse = resp
            .json()
            .await
            .map_err(|e| MemoryError::Inference(format!("réponse LLM illisible : {e}")))?;

        parsed
            .choices
            .into_iter()
            .next()
            .map(|c| c.message.content)
            .ok_or_else(|| MemoryError::Inference("réponse LLM vide (aucun choix)".into()))
    }

    fn model_id(&self) -> &str {
        &self.model
    }
}
