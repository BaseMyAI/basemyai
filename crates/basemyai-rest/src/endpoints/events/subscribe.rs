// SPDX-License-Identifier: BUSL-1.1
//! `GET /watch` (alias `GET /events`) : flux SSE des événements mémoire d'un
//! agent (ADR-022).

use std::convert::Infallible;

use axum::extract::{Query, State};
use axum::response::IntoResponse;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::stream;
use serde::{Deserialize, Serialize};

use basemyai::{MemoryEvent, MemoryEventKind, MemoryLayer};

use crate::context::{AppState, RequestContext};
use crate::http::error::RestError;

/// `agent_id` requis, `layer` optionnel (mêmes noms que `layer` ailleurs,
/// `from_table`, ex. `"semantic"`). Sans `layer`, tous les événements de
/// l'agent sont relayés.
#[derive(Deserialize)]
pub(super) struct SubscribeQuery {
    agent_id: String,
    #[serde(default)]
    layer: Option<String>,
}

/// Payload SSE minimal (ADR-022) : identité du souvenir + nature de la
/// mutation, jamais le contenu (l'abonné rappelle par `id` s'il le veut).
#[derive(Serialize)]
struct MemoryEventDto {
    agent_id: String,
    kind: &'static str,
    layer: &'static str,
    id: String,
}

impl From<&MemoryEvent> for MemoryEventDto {
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

/// Relaie [`basemyai::Memory::watch`] en SSE, un [`MemoryEventDto`] JSON par
/// ligne `data:`. L'isolation par agent/couche est déjà garantie par
/// `MemorySubscription::recv` (ADR-022) — cette route ne refait aucun
/// filtrage, elle passe `agent_id` tel quel.
///
/// Déconnexion propre, y compris à l'arrêt du serveur
/// (`server::shutdown::signal`) : aucune tâche de fond n'est `spawn`ée, le
/// flux SSE est tiré directement par le corps de réponse axum — quand le
/// client se déconnecte (ou que l'arrêt gracieux cesse de driver le flux),
/// axum abandonne la `MemorySubscription` portée par `stream::unfold`, ce qui
/// désabonne le récepteur `broadcast` via son `Drop`. Un client SSE lent ne
/// tient donc jamais de ressource au-delà de son propre flux, et ne bloque
/// jamais les écritures mémoire (le canal `broadcast` sous-jacent est
/// lossy — un abonné lent perd des événements plutôt que de ralentir
/// `remember`, cf. `basemyai::MemorySubscription`).
pub(super) async fn subscribe(
    State(state): State<AppState>,
    Query(q): Query<SubscribeQuery>,
) -> Result<impl IntoResponse, RestError> {
    let layer = q.layer.as_deref().map(MemoryLayer::from_table).transpose()?;
    let mem = RequestContext::require_agent(&state, &q.agent_id).await?;
    let subscription = mem.watch(&q.agent_id, layer);

    let stream = stream::unfold(subscription, |mut subscription| async move {
        let event = subscription.recv().await?;
        let dto = MemoryEventDto::from(&event);
        let data = serde_json::to_string(&dto).unwrap_or_else(|_| "{}".to_string());
        Some((Ok::<_, Infallible>(Event::default().data(data)), subscription))
    });

    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}
