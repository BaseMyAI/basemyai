//! Tests de bout en bout du relais de notifications mémoire (`watch`,
//! ADR-022 seconde vague). Monte un vrai serveur MCP et un vrai client MCP
//! reliés par un duplex in-memory — aucun réseau, 100 % déterministe.
//!
//! 1. `watch_delivers_remembered_notification_for_same_agent` : le client
//!    appelle `watch` puis `remember` pour le même agent et reçoit une
//!    notification `notifications/message` (`logger = "basemyai.memory"`)
//!    portant l'id du souvenir créé.
//! 2. `watch_isolates_notifications_from_other_agents` : test adversarial —
//!    une rafale de `remember` au nom d'un **autre** agent ne doit produire
//!    **aucune** notification sur le flux de l'agent observé (ADR-022,
//!    « Attente de test adversarial »).
#![cfg(feature = "test-util")]

use std::sync::Arc;
use std::time::Duration;

use basemyai_mcp::{Config, InMemoryProvider, McpServer};
use rmcp::model::{CallToolRequestParams, LoggingMessageNotificationParam, object};
use rmcp::service::{NotificationContext, RunningService};
use rmcp::{ClientHandler, RoleClient, ServiceExt};
use serde_json::{Value, json};
use tokio::sync::mpsc;

fn call(name: &'static str, args: Value) -> CallToolRequestParams {
    let mut p = CallToolRequestParams::new(name);
    p.arguments = Some(object(args));
    p
}

/// Client MCP de test : capture toutes les notifications `notifications/message`
/// (logging) reçues du serveur dans un canal, pour assertion asynchrone.
#[derive(Clone)]
struct WatchClient {
    events: mpsc::UnboundedSender<LoggingMessageNotificationParam>,
}

impl ClientHandler for WatchClient {
    async fn on_logging_message(&self, params: LoggingMessageNotificationParam, _context: NotificationContext<RoleClient>) {
        let _ = self.events.send(params);
    }
}

async fn shutdown(client: RunningService<RoleClient, WatchClient>, server_task: tokio::task::JoinHandle<()>) {
    client.cancel().await.expect("cancel client");
    let _ = server_task.await;
}

/// Attend une notification dont le payload matche `predicate`, ou `None` au
/// bout de `timeout` (le test échoue explicitement plutôt que de bloquer).
async fn recv_matching(
    rx: &mut mpsc::UnboundedReceiver<LoggingMessageNotificationParam>,
    predicate: impl Fn(&Value) -> bool,
    timeout: Duration,
) -> Option<Value> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some(param)) if predicate(&param.data) => return Some(param.data),
            Ok(Some(_)) => continue,
            Ok(None) | Err(_) => return None,
        }
    }
}

#[tokio::test]
async fn watch_delivers_remembered_notification_for_same_agent() {
    let (server_io, client_io) = tokio::io::duplex(1 << 16);

    let server = McpServer::new(Arc::new(InMemoryProvider::new()), Config::default());
    let server_task = tokio::spawn(async move {
        let running = server.serve(server_io).await.expect("serve server");
        running.waiting().await.expect("server loop");
    });

    let (tx, mut rx) = mpsc::unbounded_channel();
    let client = WatchClient { events: tx }.serve(client_io).await.expect("serve client");

    let watch_result = client
        .call_tool(call("watch", json!({ "agent_id": "a" })))
        .await
        .expect("watch tool");
    assert_ne!(watch_result.is_error, Some(true), "watch ne doit pas échouer");
    let watching = watch_result.structured_content.expect("watch structured content");
    assert_eq!(watching["watching"], true);

    let remember = client
        .call_tool(call(
            "remember",
            json!({ "agent_id": "a", "text": "sse-equivalent fact over mcp", "layer": "semantic" }),
        ))
        .await
        .expect("remember tool");
    let created = remember.structured_content.expect("remember structured content");
    let id = created["id"].as_str().expect("id").to_string();

    let payload = recv_matching(&mut rx, |v| v["kind"] == "remembered", Duration::from_secs(5))
        .await
        .expect("expected a remembered notification for agent a");

    assert_eq!(payload["agent_id"], "a");
    assert_eq!(payload["id"], id);
    assert_eq!(payload["layer"], "semantic");

    shutdown(client, server_task).await;
}

#[tokio::test]
async fn watch_isolates_notifications_from_other_agents() {
    let (server_io, client_io) = tokio::io::duplex(1 << 16);

    let server = McpServer::new(Arc::new(InMemoryProvider::new()), Config::default());
    let server_task = tokio::spawn(async move {
        let running = server.serve(server_io).await.expect("serve server");
        running.waiting().await.expect("server loop");
    });

    let (tx, mut rx) = mpsc::unbounded_channel();
    let client = WatchClient { events: tx }.serve(client_io).await.expect("serve client");

    client
        .call_tool(call("watch", json!({ "agent_id": "a" })))
        .await
        .expect("watch tool");

    // Rafale d'écritures au nom de l'agent B : rien ne doit atteindre A.
    for i in 0..5 {
        client
            .call_tool(call(
                "remember",
                json!({ "agent_id": "b", "text": format!("other agent fact {i}") }),
            ))
            .await
            .expect("remember for b");
    }

    let leaked = recv_matching(&mut rx, |_| true, Duration::from_millis(300)).await;
    assert!(
        leaked.is_none(),
        "agent a's watch must not receive agent b's events, got: {leaked:?}"
    );

    shutdown(client, server_task).await;
}
