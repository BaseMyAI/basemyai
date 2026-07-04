# Blueprint architecture — `basemyai-mcp`

**Date** : 2026-06 | **Statut** : design validé, prêt à implémenter

---

## Bibliothèque MCP choisie : `rmcp` 1.7

Crate officiel `modelcontextprotocol/rust-sdk`. Features nécessaires : `transport-io` (stdio) + `transport-streamable-http-server` (HTTP, basé axum 0.8). La macro `#[tool]` / `#[tool_router]` génère le JSON Schema automatiquement depuis les structs de paramètres (`schemars`). `StreamableHttpService` produit un `tower::Service` monté via `.nest_service("/mcp", service)`.

---

## Layout `src/`

```
crates/basemyai-mcp/src/
├── lib.rs              — re-exports (McpServer, McpError, Config)
├── error.rs            — McpError (thiserror, #[non_exhaustive])
├── config.rs           — Config::from_env() (port, api_key, timeout, max_result_bytes)
├── server.rs           — McpServer + pool Arc<RwLock<HashMap<String, Arc<Memory>>>> + #[tool_router]
├── tools/
│   ├── mod.rs          — re-exports + TruncationMarker
│   ├── remember.rs     — RememberParams, RememberResult
│   ├── recall.rs       — RecallParams, RecallResult, RecallItem (troncation ici)
│   ├── recall_graph.rs — RecallGraphParams, RecallGraphResult, EntityItem
│   ├── invalidate.rs   — InvalidateParams, InvalidateResult
│   └── stats.rs        — StatsParams, StatsResult
├── transport/
│   ├── mod.rs          — fn run_stdio, fn run_http
│   ├── stdio.rs        — détection TTY (IsTerminal) + warn + serve stdio
│   └── http.rs         — axum Router + BearerAuthLayer + StreamableHttpService
├── auth.rs             — BearerAuthLayer + BearerAuthService (subtle::ConstantTimeEq)
└── audit.rs            — emit_audit(tool, agent_id, outcome, time_ms) — jamais le contenu
```

---

## Types Rust clés

### `McpError`

```rust
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum McpError {
    #[error("memory error: {0}")]
    Memory(#[from] basemyai::MemoryError),
    #[error("invalid agent_id: must not be empty")]
    InvalidAgentId,
    #[error("invalid layer: {0}")]
    InvalidLayer(String),
    #[error("config error: {0}")]
    Config(String),
    #[error("auth: missing or invalid Bearer token")]
    Unauthorized,
    #[error("payload truncated at {limit} bytes")]
    PayloadTruncated { limit: usize },
    #[error("transport error: {0}")]
    Transport(String),
}
```

Conversion vers `rmcp::Error` : `impl From<McpError> for rmcp::Error` dans chaque handler.

### `Config`

```rust
pub struct Config {
    pub port: u16,               // défaut 7744, BASEMYAI_MCP_PORT
    pub api_key: Option<String>, // lu dans ~/.basemyai/config.toml [mcp] api_key
    pub timeout_secs: u64,       // défaut 60, BASEMYAI_MCP_TIMEOUT_SECS
    pub max_result_bytes: usize, // défaut 262144 (256 KiB), BASEMYAI_MCP_MAX_RESULT_BYTES
}
```

### `McpServer`

```rust
#[derive(Clone)]  // requis par StreamableHttpService
pub struct McpServer {
    // Pool : un Arc<Memory> par agent_id. Memory est lié à un agent_id à la construction.
    memory_pool: Arc<tokio::sync::RwLock<HashMap<String, Arc<Memory>>>>,
    embedder:    Arc<dyn Embedder + Send + Sync>,  // partagé, Candle lourd
    store_path:  PathBuf,
    enc_key:     EncryptionKey,
    config:      Arc<Config>,
}

#[tool_router]
impl McpServer {
    #[tool(description = "Store a memory in the given layer for an agent.")]
    pub async fn remember(&self, Parameters(p): Parameters<RememberParams>)
        -> core::result::Result<CallToolResult, rmcp::Error> { ... }

    #[tool(description = "Recall memories semantically similar to a query.")]
    pub async fn recall(&self, Parameters(p): Parameters<RecallParams>)
        -> core::result::Result<CallToolResult, rmcp::Error> { ... }

    #[tool(description = "Traverse the entity graph from a starting entity.")]
    pub async fn recall_graph(&self, Parameters(p): Parameters<RecallGraphParams>)
        -> core::result::Result<CallToolResult, rmcp::Error> { ... }

    #[tool(description = "Invalidate (soft-delete) a memory by ID.")]
    pub async fn invalidate(&self, Parameters(p): Parameters<InvalidateParams>)
        -> core::result::Result<CallToolResult, rmcp::Error> { ... }

    #[tool(description = "Return memory counts by layer for an agent.")]
    pub async fn stats(&self, Parameters(p): Parameters<StatsParams>)
        -> core::result::Result<CallToolResult, rmcp::Error> { ... }
}

impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::V_2025_03_26,
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation {
                name: "basemyai-mcp".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
        }
    }
}
```

**Pool multi-agent** : `get_or_create_memory(agent_id) -> Result<Arc<Memory>>` acquiert un `read` lock (agent existant : chemin chaud), ou un `write` lock (création). Jamais de `write` lock tenu à travers un `.await` long.

---

## Façades à ajouter dans `basemyai`

**1. `Memory::remember_with` → retourne `Result<String>` (UUID)**

Actuellement `Result<()>` — modifier pour retourner l'UUID inséré. Non-breaking sémantiquement.

**2. `Memory::graph(&self) -> Graph`**

`pub fn graph(&self) -> Graph` construit `Graph::new(self.store(), self.agent.clone())`. Évite d'exposer `store()` publiquement.

**3. `Memory::open_in_memory(agent_id) -> Result<Self>`**

Path `:memory:` sans chiffrement. Nécessaire pour les tests de spike Python/Node sans CMake.

---

## Audit log

```rust
pub fn emit_audit(tool: &str, agent_id: &str, outcome: Outcome, time_ms: u64) {
    tracing::info!(
        tool = tool, agent_id = agent_id,
        outcome = match outcome { Outcome::Ok => "ok", Outcome::Error => "error" },
        time_ms = time_ms,
        "mcp_audit"
    );
    // NE LOGUE JAMAIS : text, vecteurs, résultats bruts
}
```

Chaque handler enveloppe : `let t0 = Instant::now(); let r = ...; emit_audit("recall", agent_id, outcome, t0.elapsed().as_millis() as u64);`

---

## Transport stdio (`#[cfg(feature = "stdio")]`)

```rust
pub async fn run_stdio(server: McpServer) -> Result<()> {
    if std::io::stdin().is_terminal() {  // stable Rust 1.70+
        tracing::warn!(
            "stdio transport started from a terminal, not a pipe. \
             All tool calls run as operator. No per-call auth."
        );
    }
    let ct = server.serve(stdio()).await?;
    ct.waiting().await?;
    Ok(())
}
```

---

## Transport HTTP (`#[cfg(feature = "http")]`)

```rust
pub async fn run_http(server: McpServer, config: Arc<Config>) -> Result<()> {
    let api_key = config.api_key.clone()
        .ok_or_else(|| McpError::Config("HTTP transport requires an API key".into()))?;

    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        LocalSessionManager::default().into(),
        Default::default(),
    );

    let app = Router::new()
        .nest_service("/mcp", service)
        .layer(BearerAuthLayer::new(api_key))
        .layer(TimeoutLayer::new(Duration::from_secs(config.timeout_secs)));

    let listener = TcpListener::bind(format!("0.0.0.0:{}", config.port)).await?;
    tracing::info!(port = config.port, "basemyai-mcp HTTP listening");
    axum::serve(listener, app).await?;
    Ok(())
}
```

**Auth** : `BearerAuthLayer` compare le token via `subtle::ConstantTimeEq` (anti timing-attack). Retourne `401 {"error":"unauthorized"}` en JSON si absent ou invalide.

---

## Checklist d'implémentation

**Phase 1 — Fondations**
- [ ] Créer `crates/basemyai-mcp/` + `Cargo.toml`
- [ ] Ajouter `"crates/basemyai-mcp"` dans `[workspace] members`
- [ ] Ajouter workspace deps : `axum 0.8`, `tower 0.5`, `tower-http 0.6`, `rmcp 1.7`, `schemars 1.0`, `subtle 2`
- [ ] `error.rs` : `McpError` + `impl From<McpError> for rmcp::Error`
- [ ] `config.rs` : `Config::from_env()`, lecture TOML

**Phase 2 — Serveur et outils**
- [ ] Modifier `basemyai/src/memory/mod.rs` : `remember_with` → `Result<String>` ; `pub fn graph`
- [ ] Ajouter `Memory::open_in_memory` (tests sans crypto)
- [ ] `audit.rs` : `emit_audit`
- [ ] `tools/` : 5 modules (structs params/résultats + logique troncation dans `recall`)
- [ ] `server.rs` : `McpServer` pool + `get_or_create_memory` + `#[tool_router]` + `ServerHandler`

**Phase 3 — Transports**
- [ ] `auth.rs` : `BearerAuthLayer` + `BearerAuthService` (subtle)
- [ ] `transport/stdio.rs` : TTY detection + `run_stdio`
- [ ] `transport/http.rs` : `run_http` avec auth + timeout layers

**Phase 4 — Tests**
- [ ] Tests unitaires : `Config::from_env` (mock env), `BearerAuthLayer` (401/200), troncation recall
- [ ] Test intégration : `McpServer` + `Store::open_in_memory()` + embedder stub → round-trip `remember` → `recall`
- [ ] Gate : `cargo clippy --workspace --all-targets -- -D warnings` passe
- [ ] `grep -rE 'unwrap\(\)|expect\(' crates/basemyai-mcp/src/` → zéro en code lib
