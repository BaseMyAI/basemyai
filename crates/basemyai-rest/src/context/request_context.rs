// SPDX-License-Identifier: BUSL-1.1
//! Informations de requête centralisées : `request_id`, et le point d'entrée
//! unique validation+résolution d'agent partagé par tous les endpoints.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use std::convert::Infallible;
use std::sync::Arc;

use basemyai::Memory;
use tower_http::request_id::RequestId;

use super::AppState;
use crate::http::error::RestError;
use crate::http::extract::validate_agent_id;

/// Contexte extrait une fois par requête. Aujourd'hui ne porte que
/// `request_id` (posé par `http::middleware::request_id::set_layer`, lu ici
/// pour usage applicatif — le header de réponse reste géré par
/// `PropagateRequestIdLayer`, indépendamment). `store_id`/`tenant_id` ne sont
/// pas ajoutés : le sidecar n'a qu'un store physique aujourd'hui (voir
/// `context::memory_registry`) et aucune notion de tenant au-dessus de
/// l'agent — les ajouter maintenant serait une généralisation vide.
#[derive(Debug, Clone)]
pub struct RequestContext {
    pub request_id: Option<String>,
}

impl<S> FromRequestParts<S> for RequestContext
where
    S: Send + Sync,
{
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let request_id = parts
            .extensions
            .get::<RequestId>()
            .and_then(|id| id.header_value().to_str().ok())
            .map(str::to_string);
        Ok(Self { request_id })
    }
}

impl RequestContext {
    /// Point d'entrée unique validation + résolution d'un `agent_id` :
    /// valide une seule fois (`http::extract::validate_agent_id`), puis
    /// résout la `Memory` via le registre. Tous les endpoints appellent
    /// cette fonction plutôt que de reparser `agent_id` chacun de leur côté.
    ///
    /// # Errors
    /// [`RestError::InvalidAgent`]/[`RestError::Validation`] si `agent_id`
    /// est invalide ; [`RestError::Memory`] si l'ouverture échoue.
    pub async fn require_agent(state: &AppState, agent_id: &str) -> Result<Arc<Memory>, RestError> {
        validate_agent_id(agent_id)?;
        state.memories().resolve(agent_id).await
    }
}
