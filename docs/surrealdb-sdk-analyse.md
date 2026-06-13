# surrealdb (src/) — The Rust SDK

This is the **public, stable** client crate (`surrealdb` on crates.io). One API
— `Surreal<C>` — drives every engine: in-memory, on-disk (RocksDB/SurrealKV),
distributed (TiKV), browser (IndexedDB), and remote (WS/HTTP). The trick is that
all of these sit behind a single channel-based router, so the *method* code
never knows which engine it talks to.

Sources : `analayse/surrealdb/surrealdb/src/`

`lib.rs` defines the client handle; the four pillars are `conn/`, `engine/`,
`method/`, and `opt/`.

## The big picture

```
   user code
      │  db.create("user").content(..).await
      ▼
 method/*  builder structs  ── impl IntoFuture ──► Command
      │
      ▼
 conn::Router ── async_channel ──► Route{ RequestData{command, session_id}, response: Sender }
      │                                   │
      │   (one receiver lives inside the engine task)
      ▼                                   ▼
 engine/{local|remote|any}  ── executes Command against core / socket ──► Vec<QueryResult>
      │
      ▼
 response Sender ──► Router::recv_* ──► R: SurrealValue
```

The client is just a typed front-end that **serialises method calls into
`Command`s and posts them down a channel**; an engine task on the other end does
the real work and replies on a per-request `oneshot`-style bounded channel.

## `Surreal<C>` — the handle (`lib.rs`)

```rust
pub struct Surreal<C: Connection> {
    inner: Arc<Inner>,        // router (OnceLock) + waiter + session_clone
    session_id: Uuid,
    engine: PhantomData<C>,   // compile-time engine selection
}
```

- **`C: Connection`** is a type-state parameter. `Surreal<Db>` (local),
  `Surreal<Client>` (remote), `Surreal<Any>` (runtime-chosen). Enabling/omitting
  a Cargo feature makes a wrong engine a **compile error**, not a runtime one.
- **`inner: Arc<Inner>`** — clones share one router. `Router` lives in a
  `OnceLock`, so a `Surreal` can be created *before* it connects (`Surreal::init()`)
  and wired later (`connect().await`).
- **`session_id: Uuid`** + `SessionClone` — every clone of the handle mints a new
  session id and notifies the engine (`clone_session`), so server-side session
  state can fan out and be reclaimed on `Drop`. This is why `Clone` and `Drop`
  are hand-written.
- **`Connect<C, R>`** is the `IntoFuture` returned by `new`/`connect`; awaiting it
  resolves the endpoint, calls `Client::connect`, and (for remote) runs
  `verify_server_version` against `SUPPORTED_VERSIONS` (`">=3.0.0-alpha.1, <4.0.0"`),
  gracefully skipping the check if the server denies the `version` RPC.

## `conn/` — the router and the command set

- **`conn::Sealed`** — the *real* engine trait (sealed; `Connection` is the public
  marker that requires it). One method: `connect(endpoint, capacity, session_clone)
  -> BoxFuture<Result<Surreal<Self>>>`. Each engine implements this.
- **`Router`** (`conn/mod.rs`) — holds `Sender<Route>`, the negotiated `Config`,
  and a `HashSet<ExtraFeatures>` (`Backup`, `LiveQueries`) for capability checks.
  - `send_command` builds a `Route { RequestData{command, session_id}, response }`
    with a fresh bounded(1) reply channel and posts it.
  - `recv_value` / `recv_results` await the reply; the `execute_*` family adapts
    the raw `Value` into the caller's `R: SurrealValue` (`execute`, `execute_opt`,
    `execute_vec`, `execute_unit`, `execute_value`, `execute_query`). The
    single/array-unwrapping quirks (record-id ops returning a 1-element array)
    are normalised here, in one place.
- **`Command`** (`conn/cmd.rs`) — the closed enum of everything the client can
  ask: `Use`, `Signup`/`Signin`/`Authenticate`/`Refresh`/`Invalidate`,
  `Begin`/`Commit`/`Rollback`, `Query{txn, query, variables}`,
  `Export*`/`Import*` (file/bytes/ML), `Set`/`Unset`, `SubscribeLive`/`Kill`,
  `Attach`/`Detach`, `Health`, `Version`. **This enum is the actual wire between
  client and engine** — every method ultimately constructs one variant.

## `engine/` — local, remote, any

```
engine/
  local/   { mod, native, wasm }   — embedded core Datastore (kv-mem/rocksdb/tikv/…)
  remote/
    ws/    { mod, native, wasm }   — WebSocket client, auto-reconnect
    http/  { mod, native, wasm }   — HTTP client
  any/     { mod, native, wasm }   — Surreal<Any>: engine chosen at runtime by scheme
  tasks.rs                         — periodic background tasks (IntervalStream)
```

- Each engine spawns a task that **owns the `Receiver<Route>`** and loops:
  pull a `Route`, execute its `Command`, send the result back. That loop is the
  counterpart to `Router::send_command`.
- `native` vs `wasm` split per engine isolates `tokio` (native) from
  `wasm-bindgen`/`wasmtimer` (browser). `IntervalStream` in `engine/mod.rs`
  abstracts `tokio::time::Interval` vs `wasmtimer` so background tasks compile on
  both.
- **`Surreal<Any>`** (`engine/any/`) decouples the binary from the engine: the
  scheme in the endpoint string (`memory`, `rocksdb://`, `ws://`, …) picks the
  engine at runtime, so you develop on in-memory and deploy on RocksDB without a
  code change — at the cost of a runtime error if the feature wasn't compiled in.

## `method/` — one builder per verb

Every database verb is its own module + struct (`create.rs → Create`,
`select.rs → Select`, `query.rs → Query`, `live.rs → Stream`, …) re-exported
from `method/mod.rs`. The shape is uniform:

- A builder struct captures arguments fluently (`.content(..)`, `.range(..)`,
  `.with_stats()`), parameterised by type-state markers (`Live`, `Model`,
  `ExportConfig`, `Relation`).
- It implements **`IntoFuture`** (returning `BoxFuture<'a, Result<R>>`), so the
  call site reads as `db.create(..).content(..).await` — the future is built
  lazily and only runs on `.await`.
- On poll it constructs the matching `Command` and calls one of `Router::execute_*`.

`BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + Sync + 'a>>` is the
common return type. `Stats`/`WithStats` carry per-statement timing for
`query.with_stats()`.

## `opt/` — connection options & resources

- `endpoint/` — one module per scheme (`mem`, `rocksdb`, `surrealkv`, `tikv`,
  `indxdb`, `http`, `ws`) building an `Endpoint`; `IntoEndpoint` makes
  `Surreal::new::<Ws>("…")` ergonomic. `EndpointKind::is_remote()` gates the
  version check.
- `auth.rs` — `Credentials`/`Jwt`/`Token`, the root/NS/DB/record auth levels.
- `capabilities.rs` — what the embedded engine is allowed to do.
- `resource.rs` — `Resource`/`IntoResource` (table, record-id, range) — the
  targets of `select`/`update`/…
- `config.rs`, `tls.rs`, `websocket.rs`, `query.rs`, `export.rs` — per-connection
  config, TLS, WS tuning, query variables, export shaping.

## Cross-cutting conventions

- **Sealed traits** (`conn::Sealed`) keep the engine set closed while
  `Connection` stays the public bound — users name `Surreal<Db>` but cannot add
  an engine.
- **`#[allow(dead_code, reason = "...")]`** is pervasive because feature/`cfg`
  combinations (`ml`, `protocol-*`, `kv-*`, `wasm`) leave some fields/variants
  unused per build — the reasons are documented inline.
- **Errors** are the shared `surrealdb_types::Error` re-exported as
  `crate::Error`, with a crate `Result<T>` alias.

## Reading order

1. `src/lib.rs` — `Surreal<C>`, `Connect`, `Inner`, session/clone semantics.
2. `src/conn/cmd.rs` — the `Command` enum (the real client↔engine contract).
3. `src/conn/mod.rs` — `Router`, `Sealed`, the `execute_*` adapters.
4. `src/engine/any/mod.rs` — the module docs explain the whole
   local/remote/any philosophy better than anything else.
5. `src/method/create.rs` (or `select.rs`) — the canonical builder-`IntoFuture`-`Command` shape.
6. `src/opt/endpoint/mod.rs` — how a scheme string becomes a typed `Endpoint`.

## Patterns worth stealing (for les bindings/surfaces SDK de BaseMyAI)

- **Type-state engine selection** (`Surreal<C>` + sealed `Connection`) — compile-time
  guarantee that an enabled backend is used; misuse fails to compile.
- **Channel/actor router** (`Router` + `Command` + per-request reply channel) —
  decouples the public API from execution; one method set, many backends.
- **`Command` as an explicit enum** — the entire client capability is a single,
  reviewable closed type rather than scattered trait methods.
- **`IntoFuture` builders** — fluent, lazy, allocation-deferred call sites.
- **`Any` engine** — runtime backend choice by scheme string, decoupling deploy
  target from source.
- **native/wasm `cfg` split per engine** — one logical engine, two runtime
  flavours, no `tokio`-in-wasm leakage.
