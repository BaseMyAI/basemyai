//! Backend d'inférence **par sampling MCP** (ADR-017).
//!
//! Au lieu d'appeler un serveur LLM local (Ollama, AnythingLLM…) ou une API
//! cloud, ce backend **emprunte le LLM du client MCP** : quand BaseMyAI tourne
//! comme serveur MCP dans Claude Code / Claude Desktop / Cursor / ChatGPT, le
//! protocole MCP permet au serveur de demander une complétion au client via
//! `sampling/createMessage`. Le modèle qui répond est celui que l'utilisateur a
//! **déjà** choisi dans son client.
//!
//! ## Pourquoi c'est le vrai plug-and-play
//!
//! - **Zéro configuration LLM** : pas d'Ollama à installer, pas de clé API à
//!   coller. Si l'utilisateur a un client MCP, il a déjà un LLM.
//! - **Reste privacy-first** : la requête transite par le client que
//!   l'utilisateur contrôle, pas vers un tiers imposé par BaseMyAI. C'est *son*
//!   modèle, *son* choix (local ou cloud, c'est sa décision).
//! - **Un seul backend, tous les clients** : Claude Code, Desktop, Cursor,
//!   Windsurf, ChatGPT Desktop… tous les hôtes MCP qui supportent le sampling.
//!
//! ## Implication à connaître (cf. ADR-017)
//!
//! Le client peut **refuser** une requête de sampling (l'utilisateur garde la
//! main : MCP impose en principe un consentement humain au sampling). Si le
//! client ne supporte pas le sampling ou refuse, `complete` retourne une erreur
//! claire — la consolidation est alors indisponible par ce canal, et l'appelant
//! peut retomber sur un LLM local ou un backend cloud opt-in.

use basemyai::{LlmInference, MemoryError, Result};
use rmcp::RoleServer;
use rmcp::model::{CreateMessageRequestParams, Role, SamplingMessage, SamplingMessageContent};
use rmcp::service::Peer;

/// Nombre maximal de tokens demandés au client pour une complétion de
/// consolidation. Généreux : l'extraction de faits + graphe peut être verbeuse.
const DEFAULT_MAX_TOKENS: u32 = 2048;

/// Température basse : la consolidation veut une extraction fidèle et stable,
/// pas de la créativité.
const DEFAULT_TEMPERATURE: f32 = 0.2;

/// System prompt qui cadre le client comme un extracteur JSON strict. Le prompt
/// utilisateur (construit par `consolidate`) porte déjà le schéma détaillé ;
/// celui-ci renforce la contrainte « JSON uniquement ».
const SYSTEM_PROMPT: &str = "You are a memory consolidation engine. You read raw episodes and \
     extract durable facts, entities and relations. You reply with ONLY a valid JSON object, \
     no markdown fences, no prose around it.";

/// Backend [`LlmInference`] qui délègue la complétion au **client MCP** via
/// `sampling/createMessage`. Voir le module pour le rationale (ADR-017).
///
/// Construit à la volée dans le handler de l'outil `consolidate`, à partir du
/// [`Peer`] présent dans le `RequestContext` : le peer n'est valide que pendant
/// une session MCP active.
pub struct SamplingBackend {
    peer: Peer<RoleServer>,
    max_tokens: u32,
    temperature: f32,
}

impl SamplingBackend {
    /// Construit le backend autour d'un peer (la connexion au client MCP).
    #[must_use]
    pub fn new(peer: Peer<RoleServer>) -> Self {
        Self {
            peer,
            max_tokens: DEFAULT_MAX_TOKENS,
            temperature: DEFAULT_TEMPERATURE,
        }
    }

    /// Remplace le plafond de tokens demandé au client (défaut : 2048).
    #[must_use]
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }
}

#[async_trait::async_trait]
impl LlmInference for SamplingBackend {
    async fn complete(&self, prompt: &str) -> Result<String> {
        let messages = vec![SamplingMessage::new(Role::User, SamplingMessageContent::text(prompt))];

        let params = CreateMessageRequestParams::new(messages, self.max_tokens)
            .with_system_prompt(SYSTEM_PROMPT)
            .with_temperature(self.temperature);

        let result = self
            .peer
            .create_message(params)
            .await
            .map_err(|e| MemoryError::Inference(format!("MCP sampling refusé ou échoué : {e}")))?;

        extract_text(&result.message).ok_or_else(|| {
            MemoryError::Inference("réponse de sampling MCP sans contenu textuel".into())
        })
    }

    fn model_id(&self) -> &str {
        // Le modèle réel est choisi par le client ; il n'est connu qu'au retour
        // (`CreateMessageResult::model`). On expose un identifiant stable du canal.
        "mcp-sampling"
    }
}

/// Extrait le texte d'un message de sampling. Le contenu `rmcp` est un
/// [`SamplingContent`](rmcp::model::SamplingContent) (single ou multiple) ; on
/// prend le premier élément textuel. Retourne `None` s'il n'y a pas de texte.
fn extract_text(message: &SamplingMessage) -> Option<String> {
    message.content.first().and_then(|c| c.as_text()).map(|t| t.text.clone())
}
