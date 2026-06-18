# surrealdb-server — HTTP / WebSocket / RPC Server

`surrealdb-server` is the server-side binary crate: it wraps a
`surrealdb_core::kvs::Datastore` in an Axum HTTP/WebSocket stack, a CLI, auth,
observability, and the RPC dispatch shared by both transports. **It is not a
public API** — embedders consume the Rust SDK (`surrealdb` crate) instead. This
file maps the crate for someone extending the network layer or reusing its patterns.

Sources : `analayse/surrealdb/surrealdb/server/src/`

## Top-level module map

| Module | Role |
|--------|------|
| `cli/` | `clap` command tree: `start`, `sql`, `import`/`export`, `ml`, `module`, `fix`, `upgrade`, `validate`, `isready`, `mcp`. Entry for the `surreal` binary. |
| `ntw/` | **Network layer** — Axum router composition, middleware stack, per-endpoint route modules, auth layer, headers, signals. |
| `rpc/` | Transport-agnostic RPC: method dispatch + `format` (content negotiation) shared by `http` and `websocket`. |
| `dbs/` | Datastore bootstrap (open, configure, startup tasks). |
| `gql/` | GraphQL endpoint (feature `graphql`). |
| `observe/` | Metrics/observability tower layers, Prometheus `/metrics`, MCP adapter. |
| `telemetry/` | Tracing/logs/traces/audit-log setup (OpenTelemetry). |
| `cnf/`, `env/`, `tls.rs` | Static config constants, environment capture, TLS PEM loading. |

## Request lifecycle (HTTP)

```
TCP ─► axum_server (TLS optional) ─► ServiceBuilder middleware stack ─► Router ─► handler ─► Datastore
```

The middleware stack (`ntw/mod.rs`, applied outermost-first) is the spine:

1. `catch_panic` — a panicking handler becomes a 500, not a dropped connection.
2. `set_x_request_id(MakeRequestUuid)` + `propagate_x_request_id` — request correlation.
3. `concurrency_limit(NET_MAX_CONCURRENT_REQUESTS)` — global in-flight cap (backpressure).
4. `CompressionLayer` — gzip above 512 bytes, **except** gRPC and images.
5. `AddExtensionLayer(AppState)` — shared `{ client_ip, datastore, metrics_observer }`.
6. `client_ip_middleware` — resolve real client IP per `ClientIp` strategy.
7. `SetSensitiveRequestHeadersLayer` — obfuscate `Authorization`, `Cookie`, … in traces.
8. `TraceLayer` with custom `HttpTraceLayerHooks` — span per request.
9. `HttpMetricsLayer(events_observer)` — network-byte counters.
10. `SurrealAuthLayer` — parse credentials / token into a session (anonymous allowed).
11. server + version headers (suppressible via `no_identification_headers`).
12. `CorsLayer` — methods, allowed headers (incl. `NS`/`DB`/`AUTH_NS`/… and MCP headers), origins.

## Router composition — the `RouterFactory` pattern

The most reusable idea in the crate. Routes are **not** hard-coded into the
server; they are assembled by a trait so editions/embedders can add, remove, or
wrap routes without forking the startup code.

```rust
pub trait RouterFactory: TransactionBuilderFactory {
    fn configure_router(state: Self::RouterState) -> Router<Arc<RpcState>>;
}

impl RouterFactory for CommunityComposer {
    fn configure_router(_: Self::RouterState) -> Router<Arc<RpcState>> {
        Router::new()
            .route("/", get(redirect_to_app))
            .route("/status", get(|| async {}))
            .merge(health::router())
            .merge(export::router()).merge(import::router())
            .merge(rpc::router())          // /rpc — HTTP + WS upgrade
            .merge(version::router()).merge(sync::router()).merge(sql::router())
            .merge(signin::router()).merge(signup::router()).merge(key::router())
            .merge(ml::router()).merge(api::router())
        // + gql::router() / mcp::router() behind features
    }
}
```

Each `ntw/<endpoint>.rs` exposes a `router()` returning a `Router<Arc<RpcState>>`;
the composer `.merge()`s them. `CommunityComposer` is the default; an enterprise
build supplies its own composer.

## Build vs serve — embeddability

`ntw/mod.rs` deliberately splits *constructing* the configured router from
*binding a socket*:

- **`SurrealRouter::build::<F>(opts, ds, notifications, ct, state)`** — applies
  the full middleware stack + state and returns a `SurrealRouter` holding the
  Axum `Router`, the `RpcState`, the datastore, the notification receiver, and a
  `CancellationToken`. No socket touched.
  - `into_router()` hands back the raw `axum::Router` to `.merge()` into the
    embedder's own app.
  - `spawn_notifications()` starts the LIVE-query delivery task on demand.
  - `shutdown()` calls `Datastore::shutdown()` (deregisters the cluster node).
- **`init::<F>(...)`** — the all-in-one `surreal start` path: `build`, install
  `graceful_shutdown` (`CancellationToken` + `axum_server::Handle`), spawn
  notifications, then `bind`/`bind_rustls` and `serve` with
  `into_make_service_with_connect_info::<SocketAddr>()`.

This `build`/`init` split is the embeddability contract — everything an external
app needs without inheriting the CLI's socket and signal handling.

## State sharing — two channels

- **`AppState`** (`{ client_ip, datastore, metrics_observer }`) flows via
  `Extension` — read by middleware and any handler that asks.
- **`RpcState`** flows via `Router::with_state` — it is the live RPC registry:

```rust
pub struct RpcState {
    pub web_sockets: RwLock<HashMap<Uuid, Arc<Websocket>>>,   // connected sockets
    pub live_queries: RwLock<HashMap<Uuid, LiveQueryEntry>>,  // LIVE subscriptions
    pub http: Arc<rpc::http::Http>,                           // persistent HTTP sessions
    pub metrics_observer: Option<Arc<MetricsObserver>>,
}
```

`LiveQueryEntry` records `{ websocket_id, session_id, namespace, database }` so
the notification fan-out can route a change to exactly the subscribed sockets
and keep the active-LIVE gauge balanced on cleanup.

## RPC dispatch — one method set, two transports

`rpc/` is transport-agnostic:

- `rpc/format.rs` — content negotiation (JSON / CBOR / …) from `Accept`/`Content-Type`.
- `rpc/http.rs` — `/rpc` over HTTP with persistent sessions.
- `rpc/websocket.rs` — the same method set over a WS connection, plus LIVE subscriptions.
- `rpc/response.rs` — `DbResponse`/`DbResult` shaping.

LIVE queries: a registered statement adds a `LiveQueryEntry`; the spawned
`notifications(receiver, state, ct)` task consumes `Receiver<Notification>` from
the datastore and pushes each notification to the matching WebSocket.

## Observability

`observe/` installs metrics as **tower layers** (`HttpMetricsLayer`) and a
`/metrics` route merged *before* middleware so it inherits the auth/trace stack
(scraping is anonymous-allowed; the handler itself gates the body on root auth).
The byte-counter observer is sourced from `ds.observer()` so audit composers see
HTTP **and** WS bytes symmetrically even when `SURREAL_METRICS_ENABLED=false`.
`telemetry/` wires OpenTelemetry traces/logs + audit logs.

## Graceful shutdown

Cooperative, three-party: cancel the `CancellationToken` → background tasks and
the notification loop exit → `axum_server::Handle` stops accepting → drain →
`Datastore::shutdown()` deregisters the node. CLI (`surreal start`) does this
automatically; embedders using `build` must drive it themselves.

## Reading order

1. `ntw/mod.rs` — `RouterFactory`, the middleware stack, `SurrealRouter::build` vs `init`. The whole crate's shape is here.
2. `rpc/mod.rs` — `RpcState`, `LiveQueryEntry`, the notification fan-out.
3. `rpc/format.rs` + `rpc/{http,websocket}.rs` — one method set across two transports.
4. `cli/start.rs` — how the binary assembles datastore + router + signals.
5. `observe/` + `telemetry/` — layered, opt-in instrumentation.

## Patterns worth stealing

- **Composer trait over hard-coded routes** (`RouterFactory`) — extensible surface without forking startup.
- **`build` (router) / `init` (serve) split** — makes the server embeddable as a plain Axum sub-router.
- **State via two lanes** — immutable config through `Extension`, the live mutable registry through `with_state`.
- **Cancellation-token graceful shutdown** wired through every spawned task.
- **Instrumentation as removable tower layers**, inert (not just cheap) when disabled.
