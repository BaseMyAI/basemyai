// SPDX-License-Identifier: BUSL-1.1
//! Serveur MCP : pool multi-agent + les outils mémoire (voir `INSTRUCTIONS`
//! ci-dessous pour la liste à jour — ne pas répéter un compte ici, il dérive).
//!
//! Une [`Memory`] est scellée par un `agent_id` (ADR-006) ; le serveur en
//! maintient une par agent dans un pool `Arc<RwLock<HashMap<..>>>`, ouverte à la
//! demande via le [`MemoryProvider`] injecté. Le verrou d'écriture n'est **jamais**
//! tenu à travers un `.await` long : l'ouverture (I/O) se fait hors verrou.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use rmcp::ErrorData;
use rmcp::RoleServer;
use rmcp::ServerHandler;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{
    AnnotateAble, GetPromptRequestParams, GetPromptResult, ListPromptsResult, ListResourcesResult, LoggingLevel,
    LoggingMessageNotification, LoggingMessageNotificationParam, PaginatedRequestParams, Prompt, PromptArgument,
    PromptMessage, PromptMessageRole, RawResource, ReadResourceRequestParams, ReadResourceResult, ResourceContents,
    ServerCapabilities, ServerInfo, ServerNotification,
};
use rmcp::service::{Peer, RequestContext};
use rmcp::{tool, tool_handler, tool_router};
use serde::Serialize;
use tokio::sync::RwLock;

use basemyai::{AgentId, Memory, MemoryEvent, MemoryEventKind, MemoryLayer, RecallOptions};

use crate::audit::{Outcome, emit_audit};
use crate::config::Config;
use crate::error::McpError;
use crate::provider::MemoryProvider;
use crate::sampling::SamplingBackend;
use crate::tools::{
    self, CompileContextParams, CompileContextResult, ConsolidateApplyParams, ConsolidateParams, ConsolidateResult,
    EntityItem, InvalidateParams, InvalidateResult, RecallGraphParams, RecallGraphResult, RecallItem, RecallParams,
    RecallResult, RememberParams, RememberResult, StatsParams, StatsResult, WatchParams, WatchResult,
};

/// Nom du "logger" MCP porté par chaque notification `notifications/message`
/// émise pour un événement mémoire — permet au client de les distinguer d'un
/// log applicatif générique sans introduire de méthode JSON-RPC custom.
const MEMORY_EVENT_LOGGER: &str = "basemyai.memory";

/// Serveur MCP basemyai. `Clone` : requis par le transport HTTP (un handle par
/// session). Tous les champs sont partagés (`Arc`), le clone est bon marché.
#[derive(Clone)]
pub struct McpServer {
    /// Une mémoire ouverte par `agent_id`.
    pool: Arc<RwLock<HashMap<String, Arc<Memory>>>>,
    /// Ouvre la mémoire d'un agent absent du pool.
    provider: Arc<dyn MemoryProvider>,
    /// Configuration partagée (plafonds, timeouts).
    config: Arc<Config>,
    /// Routeur d'outils généré par `#[tool_router]`.
    tool_router: ToolRouter<Self>,
}

impl McpServer {
    /// Construit le serveur autour d'un provider de mémoire et d'une config.
    #[must_use]
    pub fn new(provider: Arc<dyn MemoryProvider>, config: Config) -> Self {
        Self {
            pool: Arc::new(RwLock::new(HashMap::new())),
            provider,
            config: Arc::new(config),
            tool_router: Self::tool_router(),
        }
    }

    /// Récupère (ou ouvre puis met en cache) la mémoire de `agent_id`.
    ///
    /// Chemin chaud : un `read` lock suffit. Chemin froid : ouverture **hors
    /// verrou** (I/O async), puis insertion sous `write` lock sans `.await`.
    async fn memory_for(&self, agent_id: &str) -> Result<Arc<Memory>, McpError> {
        let agent = AgentId::new(agent_id).ok_or(McpError::InvalidAgentId)?;

        if let Some(mem) = self.pool.read().await.get(agent_id) {
            return Ok(Arc::clone(mem));
        }

        // Ouverture hors verrou (peut faire de l'I/O et migrer le schéma).
        let opened = Arc::new(self.provider.open(agent).await?);

        // Insertion atomique : si un autre appel a gagné la course, on garde
        // l'entrée existante (l'ouverture redondante est simplement abandonnée).
        let mut pool = self.pool.write().await;
        Ok(Arc::clone(pool.entry(agent_id.to_string()).or_insert(opened)))
    }

    async fn remember_impl(&self, p: RememberParams) -> Result<RememberResult, McpError> {
        tools::validate_agent_id(&p.agent_id)?;
        tools::validate_text(&p.text)?;
        let layer = tools::parse_layer(&p.layer)?;
        let mem = self.memory_for(&p.agent_id).await?;
        let id = mem.remember(&p.text, layer).await?;
        Ok(RememberResult { id })
    }

    async fn recall_impl(&self, p: RecallParams) -> Result<RecallResult, McpError> {
        tools::validate_agent_id(&p.agent_id)?;
        tools::validate_query(&p.query)?;
        tools::validate_k(p.k)?;
        let mem = self.memory_for(&p.agent_id).await?;
        let options = RecallOptions {
            include_procedural: p.include_procedural,
            exclude_imported: p.exclude_imported,
        };
        let records = mem.recall_with_options(&p.query, p.k, options).await?;
        let items: Vec<RecallItem> = records.into_iter().map(recall_item_from_record).collect();
        let (items, truncated) = tools::truncate_to_fit(items, self.config.max_result_bytes);
        Ok(RecallResult { items, truncated })
    }

    async fn recall_hybrid_impl(&self, p: RecallParams) -> Result<RecallResult, McpError> {
        tools::validate_agent_id(&p.agent_id)?;
        tools::validate_query(&p.query)?;
        tools::validate_k(p.k)?;
        let mem = self.memory_for(&p.agent_id).await?;
        let options = RecallOptions {
            include_procedural: p.include_procedural,
            exclude_imported: p.exclude_imported,
        };
        let records = mem.recall_hybrid_with_options(&p.query, p.k, options).await?;
        // Ici `score` est le score RRF fusionné (pas la similarité cosinus).
        let items: Vec<RecallItem> = records
            .into_iter()
            .map(|r| {
                let trust = r.trust().as_str().to_string();
                RecallItem {
                    id: r.id,
                    text: r.text,
                    layer: r.layer.table().to_string(),
                    score: r.score,
                    source: r.source,
                    trust,
                }
            })
            .collect();
        let (items, truncated) = tools::truncate_to_fit(items, self.config.max_result_bytes);
        Ok(RecallResult { items, truncated })
    }

    async fn recall_graph_impl(&self, p: RecallGraphParams) -> Result<RecallGraphResult, McpError> {
        tools::validate_agent_id(&p.agent_id)?;
        tools::validate_start(&p.start)?;
        tools::validate_max_depth(p.max_depth)?;
        let mem = self.memory_for(&p.agent_id).await?;
        let reached = mem.graph().traverse(&p.start, p.max_depth).await?;
        let entities: Vec<EntityItem> = reached
            .into_iter()
            .map(|e| EntityItem {
                id: e.id,
                kind: e.kind,
                label: e.label,
                depth: e.depth,
            })
            .collect();
        let (entities, truncated) = tools::truncate_to_fit(entities, self.config.max_result_bytes);
        Ok(RecallGraphResult { entities, truncated })
    }

    async fn compile_context_impl(&self, p: CompileContextParams) -> Result<CompileContextResult, McpError> {
        tools::validate_agent_id(&p.agent_id)?;
        tools::validate_query(&p.query)?;
        let source_policy = tools::parse_source_policy(&p.source_policy).map_err(McpError::Validation)?;
        let profile = tools::parse_profile(&p.profile).map_err(McpError::Validation)?;
        let render_format = tools::parse_render_format(&p.render_format).map_err(McpError::Validation)?;
        let mem = self.memory_for(&p.agent_id).await?;
        let mut request = basemyai::ContextRequest::new(&p.query, p.token_budget)
            .candidate_limit(p.candidate_limit)
            .source_policy(source_policy)
            .profile(profile)
            .render_format(render_format);
        if p.include_procedural {
            request = request.include_procedural();
        }
        if p.explain {
            request = request.explain();
        }
        let bundle = mem.compile_context(request).await?;
        Ok(CompileContextResult::from(bundle))
    }

    async fn invalidate_impl(&self, p: InvalidateParams) -> Result<InvalidateResult, McpError> {
        tools::validate_agent_id(&p.agent_id)?;
        let mem = self.memory_for(&p.agent_id).await?;
        mem.invalidate(&p.id).await?;
        Ok(InvalidateResult { invalidated: true })
    }

    async fn stats_impl(&self, p: StatsParams) -> Result<StatsResult, McpError> {
        tools::validate_agent_id(&p.agent_id)?;
        let mem = self.memory_for(&p.agent_id).await?;
        let s = mem.stats().await?;
        Ok(StatsResult {
            short_term: s.short_term,
            episodic: s.episodic,
            procedural: s.procedural,
            semantic: s.semantic,
            total: s.total(),
        })
    }

    /// Consolide les épisodes de l'agent en faits + graphe, via une **politique à
    /// niveaux** (ADR-018, supersède ADR-017) :
    /// 1. **sampling MCP** si le client l'annonce (rare ; déprécié SEP-2577) ;
    /// 2. **LLM local** autonome (Ollama/LM Studio/AnythingLLM) via `choose_llm` ;
    /// 3. sinon **piloté par l'agent** : renvoie les épisodes + instructions ;
    ///    l'agent extrait avec son propre LLM et persiste via `consolidate_apply`.
    ///
    /// Le `peer` provient du `RequestContext` — valide seulement pendant la session.
    async fn consolidate_impl(
        &self,
        p: ConsolidateParams,
        peer: Peer<RoleServer>,
    ) -> Result<ConsolidateResult, McpError> {
        tools::validate_agent_id(&p.agent_id)?;
        let mem = self.memory_for(&p.agent_id).await?;

        // Niveau 1 : sampling, seulement si le client l'a annoncé à l'init.
        if client_supports_sampling(&peer) {
            let backend = SamplingBackend::new(peer);
            let report = basemyai::consolidate(&mem, &backend).await?;
            return Ok(ConsolidateResult::done("sampling", report));
        }

        // Niveau 2 : LLM local détecté (hardware-aware) ou AnythingLLM (env).
        if let Ok(provision) = basemyai::choose_llm().await {
            let via = format!("local:{}", provision.model_id);
            let report = basemyai::consolidate(&mem, provision.backend.as_ref()).await?;
            return Ok(ConsolidateResult::done(&via, report));
        }

        // Niveau 3 : pas de LLM côté serveur → on délègue à l'agent appelant.
        match basemyai::consolidation_prompt(&mem).await? {
            None => Ok(ConsolidateResult::done(
                "none",
                basemyai::ConsolidationReport::default(),
            )),
            Some(input) => Ok(ConsolidateResult::extraction_required(&p.agent_id, input)),
        }
    }

    /// Applique une extraction produite par l'agent (consolidation pilotée par
    /// l'agent, ADR-018) : peuple le graphe et promeut les faits, idempotent.
    async fn consolidate_apply_impl(&self, p: ConsolidateApplyParams) -> Result<ConsolidateResult, McpError> {
        tools::validate_agent_id(&p.agent_id)?;
        let mem = self.memory_for(&p.agent_id).await?;
        let report = basemyai::apply_extraction(&mem, &p.into_extraction()).await?;
        Ok(ConsolidateResult::done("agent", report))
    }

    /// Démarre le relais des événements mémoire de `p.agent_id` vers **ce**
    /// client MCP (ADR-022, seconde vague). Renvoie immédiatement ; les
    /// événements arrivent ensuite en notifications `notifications/message`
    /// (`logger = "basemyai.memory"`), poussées via le `peer` de la session —
    /// même mécanisme de push serveur→client que [`SamplingBackend`], sans
    /// attendre de réponse du client.
    ///
    /// L'isolation par agent/couche est déjà garantie par
    /// `MemorySubscription::recv` (ADR-022) : cette méthode ne refait aucun
    /// filtrage, elle passe `agent_id` tel quel à [`Memory::watch`].
    async fn watch_impl(&self, p: WatchParams, peer: Peer<RoleServer>) -> Result<WatchResult, McpError> {
        tools::validate_agent_id(&p.agent_id)?;
        let layer = match p.layer.as_deref() {
            Some(name) => Some(tools::parse_layer(name)?),
            None => None,
        };
        let mem = self.memory_for(&p.agent_id).await?;
        let agent_id = p.agent_id.clone();
        tokio::spawn(relay_memory_events(mem, agent_id, layer, peer));
        Ok(WatchResult { watching: true })
    }
}

/// Payload minimal poussé pour un [`MemoryEvent`] (ADR-022) : identité du
/// souvenir + nature de la mutation, jamais le contenu (le client rappelle
/// `recall`/`stats` par `id` s'il veut le détail).
#[derive(Serialize)]
struct MemoryEventPayload {
    agent_id: String,
    kind: &'static str,
    layer: &'static str,
    id: String,
}

impl From<&MemoryEvent> for MemoryEventPayload {
    fn from(ev: &MemoryEvent) -> Self {
        Self {
            agent_id: ev.agent_id.clone(),
            kind: match ev.kind {
                MemoryEventKind::Remembered => "remembered",
                MemoryEventKind::Invalidated => "invalidated",
                MemoryEventKind::Forgotten => "forgotten",
                MemoryEventKind::Consolidated => "consolidated",
                // `MemoryEventKind` est `#[non_exhaustive]` : un genre futur
                // atterrit ici plutôt que de casser la compilation.
                _ => "unknown",
            },
            layer: ev.layer.table(),
            id: ev.id.clone(),
        }
    }
}

/// Boucle de relais : consomme [`basemyai::MemorySubscription`] et pousse
/// chaque événement au client MCP via `notifications/message`. S'arrête
/// proprement dès que l'envoi échoue (session/transport fermé côté client) —
/// aucune tâche de fond ne survit à la déconnexion. `mem` est conservé vivant
/// pour la durée de la tâche : le canal `broadcast` de `Memory` (donc les
/// événements) ne disparaît pas tant qu'un abonné actif existe.
async fn relay_memory_events(mem: Arc<Memory>, agent_id: String, layer: Option<MemoryLayer>, peer: Peer<RoleServer>) {
    let mut subscription = mem.watch(&agent_id, layer);
    while let Some(event) = subscription.recv().await {
        let payload = MemoryEventPayload::from(&event);
        let data = serde_json::to_value(&payload).unwrap_or(serde_json::Value::Null);
        let param = LoggingMessageNotificationParam::new(LoggingLevel::Info, data).with_logger(MEMORY_EVENT_LOGGER);
        let notification = ServerNotification::LoggingMessageNotification(LoggingMessageNotification::new(param));
        if peer.send_notification(notification).await.is_err() {
            // Transport fermé (client déconnecté) : plus personne à qui parler.
            break;
        }
    }
}

/// `true` si le client MCP a annoncé la capability `sampling` à l'initialisation.
/// Claude Code (juin 2026) ne la supporte pas ; le sampling est par ailleurs
/// déprécié dans le protocole (SEP-2577) — d'où la politique à niveaux d'ADR-018.
fn client_supports_sampling(peer: &Peer<RoleServer>) -> bool {
    peer.peer_info()
        .map(|info| info.capabilities.sampling.is_some())
        .unwrap_or(false)
}

/// Définition des outils MCP. La macro génère `Self::tool_router()` et le JSON
/// Schema de chaque outil à partir des structs `*Params`.
#[tool_router]
impl McpServer {
    /// Mémorise un texte.
    #[tool(
        description = "Store a memory in a layer (short_term|episodic|procedural|semantic) for an agent. Returns the new memory id.",
        annotations(
            title = "Remember",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub async fn remember(&self, Parameters(p): Parameters<RememberParams>) -> Result<Json<RememberResult>, ErrorData> {
        let t0 = Instant::now();
        let agent_id = p.agent_id.clone();
        let out = self.remember_impl(p).await;
        emit_audit("remember", &agent_id, outcome(&out), elapsed_ms(t0));
        Ok(Json(out?))
    }

    /// Recall sémantique temporel.
    #[tool(
        description = "Recall memories semantically similar to a query, scoped to an agent and still temporally valid.",
        annotations(title = "Recall", read_only_hint = true, open_world_hint = false)
    )]
    pub async fn recall(&self, Parameters(p): Parameters<RecallParams>) -> Result<Json<RecallResult>, ErrorData> {
        let t0 = Instant::now();
        let agent_id = p.agent_id.clone();
        let out = self.recall_impl(p).await;
        emit_audit("recall", &agent_id, outcome(&out), elapsed_ms(t0));
        Ok(Json(out?))
    }

    /// Recall hybride (vecteur + BM25, fusion RRF).
    #[tool(
        description = "Hybrid recall: fuse semantic vector similarity with BM25 keyword ranking (RRF), scoped to an agent and temporally valid. Best when the query contains exact terms (IDs, acronyms, proper nouns) as well as meaning.",
        annotations(title = "Recall (hybrid)", read_only_hint = true, open_world_hint = false)
    )]
    pub async fn recall_hybrid(
        &self,
        Parameters(p): Parameters<RecallParams>,
    ) -> Result<Json<RecallResult>, ErrorData> {
        let t0 = Instant::now();
        let agent_id = p.agent_id.clone();
        let out = self.recall_hybrid_impl(p).await;
        emit_audit("recall_hybrid", &agent_id, outcome(&out), elapsed_ms(t0));
        Ok(Json(out?))
    }

    /// Traversée du graphe entités/relations.
    #[tool(
        description = "Traverse the entity/relation graph from a starting entity, scoped to an agent.",
        annotations(title = "Recall graph", read_only_hint = true, open_world_hint = false)
    )]
    pub async fn recall_graph(
        &self,
        Parameters(p): Parameters<RecallGraphParams>,
    ) -> Result<Json<RecallGraphResult>, ErrorData> {
        let t0 = Instant::now();
        let agent_id = p.agent_id.clone();
        let out = self.recall_graph_impl(p).await;
        emit_audit("recall_graph", &agent_id, outcome(&out), elapsed_ms(t0));
        Ok(Json(out?))
    }

    /// Compile un recall hybride en contexte borné et traçable, sans LLM.
    #[tool(
        description = "Compile a hybrid recall into a bounded, cited context ready for a model prompt — deterministic, no LLM in the loop. Prefer this over raw recall when you need a token-budgeted, ranked, deduplicated context rather than a plain list of memories.",
        annotations(title = "Compile context", read_only_hint = true, open_world_hint = false)
    )]
    pub async fn compile_context(
        &self,
        Parameters(p): Parameters<CompileContextParams>,
    ) -> Result<Json<CompileContextResult>, ErrorData> {
        let t0 = Instant::now();
        let agent_id = p.agent_id.clone();
        let out = self.compile_context_impl(p).await;
        emit_audit("compile_context", &agent_id, outcome(&out), elapsed_ms(t0));
        Ok(Json(out?))
    }

    /// Invalide (soft-delete) un souvenir.
    #[tool(
        description = "Invalidate (soft-delete) a memory by id; it stops appearing in future recalls.",
        annotations(
            title = "Invalidate",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub async fn invalidate(
        &self,
        Parameters(p): Parameters<InvalidateParams>,
    ) -> Result<Json<InvalidateResult>, ErrorData> {
        let t0 = Instant::now();
        let agent_id = p.agent_id.clone();
        let out = self.invalidate_impl(p).await;
        emit_audit("invalidate", &agent_id, outcome(&out), elapsed_ms(t0));
        Ok(Json(out?))
    }

    /// Statistiques par couche.
    #[tool(
        description = "Return counts of valid memories per layer for an agent.",
        annotations(title = "Stats", read_only_hint = true, open_world_hint = false)
    )]
    pub async fn stats(&self, Parameters(p): Parameters<StatsParams>) -> Result<Json<StatsResult>, ErrorData> {
        let t0 = Instant::now();
        let agent_id = p.agent_id.clone();
        let out = self.stats_impl(p).await;
        emit_audit("stats", &agent_id, outcome(&out), elapsed_ms(t0));
        Ok(Json(out?))
    }

    /// Consolidation épisodes → faits + graphe, politique à niveaux (ADR-018).
    #[tool(
        description = "Consolidate an agent's recent episodes into durable facts and a relation graph. \
             Picks an LLM in this order: (1) MCP sampling if your client supports it; (2) a local LLM \
             (Ollama/LM Studio/AnythingLLM) if one is running. If NEITHER is available, it returns \
             status=\"extraction_required\" with the raw `episodes` and `instructions`: in that case YOU \
             extract the facts/entities/relations and call `consolidate_apply` with agent_id and the three \
             arrays. Call this periodically to distill episodic memory into semantic knowledge.",
        annotations(
            title = "Consolidate",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub async fn consolidate(
        &self,
        Parameters(p): Parameters<ConsolidateParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<ConsolidateResult>, ErrorData> {
        let t0 = Instant::now();
        let agent_id = p.agent_id.clone();
        let out = self.consolidate_impl(p, context.peer).await;
        emit_audit("consolidate", &agent_id, outcome(&out), elapsed_ms(t0));
        Ok(Json(out?))
    }

    /// Applique l'extraction produite par l'agent (consolidation pilotée par l'agent).
    #[tool(
        description = "Persist a consolidation result that YOU extracted (used after `consolidate` returns \
             status=\"extraction_required\"). Provide agent_id plus `facts` (strings), `entities` \
             ({id,kind,label}) and `relations` ({src,relation,dst} referencing entity ids). Upserts the \
             graph and promotes facts to the semantic layer; idempotent (existing facts are skipped).",
        annotations(
            title = "Consolidate (apply)",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub async fn consolidate_apply(
        &self,
        Parameters(p): Parameters<ConsolidateApplyParams>,
    ) -> Result<Json<ConsolidateResult>, ErrorData> {
        let t0 = Instant::now();
        let agent_id = p.agent_id.clone();
        let out = self.consolidate_apply_impl(p).await;
        emit_audit("consolidate_apply", &agent_id, outcome(&out), elapsed_ms(t0));
        Ok(Json(out?))
    }

    /// Démarre le relais des événements mémoire en temps réel (ADR-022).
    #[tool(
        description = "Start receiving live memory-change notifications for an agent on this MCP session. \
             Returns immediately; from then on, every remember/invalidate/forget/consolidate for this agent_id \
             (optionally filtered to one layer) arrives asynchronously as a `notifications/message` \
             (logger=\"basemyai.memory\", data={agent_id,kind,layer,id}). The notification carries only the \
             changed memory's id and kind, never its content — call `recall`/`stats` if you need details. \
             Stops automatically when this session disconnects.",
        annotations(title = "Watch", read_only_hint = true, open_world_hint = false)
    )]
    pub async fn watch(
        &self,
        Parameters(p): Parameters<WatchParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<Json<WatchResult>, ErrorData> {
        let t0 = Instant::now();
        let agent_id = p.agent_id.clone();
        let out = self.watch_impl(p, context.peer).await;
        emit_audit("watch", &agent_id, outcome(&out), elapsed_ms(t0));
        Ok(Json(out?))
    }
}

/// Instructions exposées dans `get_info` et la resource `basemyai://instructions`.
const INSTRUCTIONS: &str = "\
BaseMyAI — moteur de mémoire local pour agents IA (100% local, chiffré au repos).\n\
Outils : remember (mémorise), recall (sémantique), recall_hybrid (vecteur+BM25 fusionnés),\n\
recall_graph (graphe entités), compile_context (contexte borné et tracé, prêt pour un prompt),\n\
invalidate (soft-delete), stats (compteurs par couche),\n\
consolidate (distille les épisodes en faits+graphe), consolidate_apply (persiste une extraction),\n\
watch (démarre le relais temps réel des changements mémoire en notifications/message).\n\
Consolidation (ADR-018) : consolidate tente un LLM côté serveur (sampling si supporté, sinon LLM local) ;\n\
s'il n'y en a pas, il renvoie status=\"extraction_required\" et c'est TOI qui extrais puis appelles\n\
consolidate_apply. Le prompt /consolidate_memory pilote ce flux de bout en bout.\n\
Chaque appel exige `agent_id` (isolation stricte). Couches : short_term, episodic, procedural, semantic.";

/// Documentation des couches, exposée via la resource `basemyai://layers`.
const LAYERS_DOC: &str = "\
# Couches mémoire\n\
- short_term : contexte de session (TTL court)\n\
- episodic   : ce qui s'est passé et quand\n\
- procedural : procédures/compétences apprises\n\
- semantic   : faits recherchables vectoriellement\n";

/// `ServerHandler` : la macro `#[tool_handler]` ajoute le routage des outils
/// (call_tool/list_tools/get_tool) ; on fournit `get_info`, les Resources et les
/// Prompts manuellement — la macro ne les écrase pas (elle ne remplit que les
/// méthodes absentes).
#[tool_handler(router = self.tool_router)]
impl ServerHandler for McpServer {
    #[allow(deprecated)] // SEP-2577 : `enable_logging` déprécié ; requis pour notifications/message (ADR-022).
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_prompts()
                .enable_resources()
                // Requis pour `notifications/message` (ADR-022, relais `watch`) :
                // annonce au client qu'il peut recevoir des notifications de log.
                .enable_logging()
                .build(),
        )
        .with_instructions(INSTRUCTIONS)
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let mut instructions = RawResource::new("basemyai://instructions", "instructions");
        instructions.description = Some("How to use the BaseMyAI memory tools.".to_string());
        instructions.mime_type = Some("text/markdown".to_string());

        let mut layers = RawResource::new("basemyai://layers", "memory-layers");
        layers.description = Some("The four memory layers and their semantics.".to_string());
        layers.mime_type = Some("text/markdown".to_string());

        Ok(ListResourcesResult::with_all_items(vec![
            instructions.no_annotation(),
            layers.no_annotation(),
        ]))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, ErrorData> {
        let text = match request.uri.as_str() {
            "basemyai://instructions" => INSTRUCTIONS,
            "basemyai://layers" => LAYERS_DOC,
            other => {
                return Err(ErrorData::resource_not_found(
                    format!("unknown resource: {other}"),
                    None,
                ));
            }
        };
        Ok(ReadResourceResult::new(vec![ResourceContents::TextResourceContents {
            uri: request.uri,
            mime_type: Some("text/markdown".to_string()),
            text: text.to_string(),
            meta: None,
        }]))
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        let mut agent_arg = PromptArgument::new("agent_id");
        agent_arg.required = Some(true);
        agent_arg.description = Some("Agent whose memory to summarize.".to_string());

        let mut focus_arg = PromptArgument::new("focus");
        focus_arg.required = Some(false);
        focus_arg.description = Some("Optional topic to focus the summary on.".to_string());

        let prompt = Prompt::new(
            "summarize_agent_memory",
            Some("Recall and summarize an agent's most relevant memories."),
            Some(vec![agent_arg, focus_arg]),
        );

        let mut consolidate_arg = PromptArgument::new("agent_id");
        consolidate_arg.required = Some(true);
        consolidate_arg.description = Some("Agent whose episodes to consolidate.".to_string());
        let consolidate_prompt = Prompt::new(
            "consolidate_memory",
            Some(
                "Agent-driven consolidation: extract facts/entities/relations from the agent's episodes, then call consolidate_apply.",
            ),
            Some(vec![consolidate_arg]),
        );

        Ok(ListPromptsResult::with_all_items(vec![prompt, consolidate_prompt]))
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResult, ErrorData> {
        match request.name.as_str() {
            "summarize_agent_memory" => {
                let args = request.arguments.unwrap_or_default();
                let agent_id = args.get("agent_id").and_then(|v| v.as_str()).unwrap_or_default();
                if agent_id.is_empty() {
                    return Err(ErrorData::invalid_params("agent_id is required", None));
                }
                let focus = args.get("focus").and_then(|v| v.as_str()).unwrap_or_default();
                let text = if focus.is_empty() {
                    format!(
                        "Using the `recall` tool with agent_id=\"{agent_id}\", retrieve the most relevant \
                         memories, then write a concise summary grounded ONLY in them."
                    )
                } else {
                    format!(
                        "Using the `recall_hybrid` tool with agent_id=\"{agent_id}\" and query=\"{focus}\", \
                         retrieve the most relevant memories, then write a concise summary grounded ONLY in them."
                    )
                };
                Ok(GetPromptResult::new(vec![PromptMessage::new_text(
                    PromptMessageRole::User,
                    text,
                )]))
            }
            "consolidate_memory" => {
                let args = request.arguments.unwrap_or_default();
                let agent_id = args.get("agent_id").and_then(|v| v.as_str()).unwrap_or_default();
                if agent_id.is_empty() {
                    return Err(ErrorData::invalid_params("agent_id is required", None));
                }
                let mem = self.memory_for(agent_id).await.map_err(ErrorData::from)?;
                let input = basemyai::consolidation_prompt(&mem)
                    .await
                    .map_err(|e| ErrorData::from(McpError::from(e)))?;
                let text = match input {
                    None => format!("Agent \"{agent_id}\" has no episodes to consolidate."),
                    Some(input) => format!(
                        "{}\n\nWhen you have the JSON extraction, call the `consolidate_apply` tool with \
                         agent_id=\"{agent_id}\" and the `facts`, `entities` and `relations` arrays.",
                        input.prompt
                    ),
                };
                Ok(GetPromptResult::new(vec![PromptMessage::new_text(
                    PromptMessageRole::User,
                    text,
                )]))
            }
            other => Err(ErrorData::invalid_params(format!("unknown prompt: {other}"), None)),
        }
    }
}

fn recall_item_from_record(r: basemyai::Record) -> RecallItem {
    let score = r.similarity();
    let trust = r.trust().as_str().to_string();
    RecallItem {
        id: r.id,
        text: r.text,
        layer: r.layer.table().to_string(),
        score,
        source: r.source,
        trust,
    }
}

/// Durée écoulée en millisecondes, saturée.
fn elapsed_ms(t0: Instant) -> u64 {
    u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Issue d'audit dérivée d'un résultat, sans en exposer le contenu.
fn outcome<T>(r: &Result<T, McpError>) -> Outcome {
    if r.is_ok() { Outcome::Ok } else { Outcome::Error }
}
