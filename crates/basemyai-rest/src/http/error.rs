// SPDX-License-Identifier: BUSL-1.1
//! Modèle d'erreur HTTP unique et stable. Toutes les couches (extraction,
//! validation, provider, mémoire) convergent vers [`RestError`], qui se
//! sérialise en `{"error":{"code","message","request_id","details"}}`.
//!
//! Aucun message bas niveau (SQL, chemin local, crypto, backtrace) n'atteint
//! le client : les variantes non mappées explicitement tombent dans
//! `INTERNAL_ERROR` et sont logguées avec leur `request_id` côté serveur.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// Erreur du sidecar, traduite en réponse HTTP stable.
///
/// `Display`/`Error` viennent de `thiserror` (requis pour `?` vers
/// `Box<dyn Error>` dans le binaire et pour le logging) ; le mapping HTTP
/// stable ([`RestError::parts`]) reste séparé de ces messages.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RestError {
    /// Jeton Bearer absent ou invalide.
    #[error("missing or invalid Bearer token")]
    Unauthorized,
    /// `agent_id` vide / hors contrainte.
    #[error("a valid agent_id is required")]
    InvalidAgent,
    /// Erreur propagée depuis la couche mémoire (`basemyai`).
    #[error(transparent)]
    Memory(#[from] basemyai::MemoryError),
    /// Configuration invalide (démarrage).
    #[error("invalid REST configuration: {0}")]
    Config(String),
    /// Entrée hors des bornes documentées par l'OpenAPI (`agent_id`, `text`,
    /// `query`, `k`, `max_depth`, `importance`, temporalité...).
    #[error("validation error: {0}")]
    Validation(String),
    /// Quota d'appels dépassé pour cet agent (fenêtre glissante).
    #[error("rate limit exceeded")]
    RateLimited,
    /// Extraction Axum (JSON malformé, `Content-Type` incorrect, query
    /// invalide) — mappée depuis `JsonRejection`/`QueryRejection` par les
    /// extracteurs de `crate::http::extract`.
    #[error("bad request: {0}")]
    BadRequest(String),
    /// Corps de requête au-delà de `RuntimeConfig::max_body_bytes`, détecté
    /// par `RequestBodyLimitLayer` et remonté via `JsonRejection::BytesRejection`.
    #[error("request body exceeds the configured size limit")]
    PayloadTooLarge,
}

/// Détail d'erreur sérialisé. `details` reste optionnel — la plupart des
/// erreurs n'ont besoin que d'un `code`/`message`.
#[derive(Serialize)]
struct ErrorDetail {
    code: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    request_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<serde_json::Value>,
}

#[derive(Serialize)]
struct ErrorBody {
    error: ErrorDetail,
}

impl RestError {
    /// `(statut HTTP, code stable, message, details optionnels)` pour cette erreur.
    ///
    /// Les codes sont stables et documentés dans `openapi.yaml` — ne jamais
    /// les dériver du message `Display`, qui peut changer de formulation.
    fn parts(&self) -> (StatusCode, &'static str, String, Option<serde_json::Value>) {
        use basemyai::MemoryError as M;
        use basemyai_core::CoreError;
        match self {
            Self::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "Missing or invalid Bearer token.".to_string(),
                None,
            ),
            Self::InvalidAgent => (
                StatusCode::BAD_REQUEST,
                "invalid_agent_id",
                "A valid agent_id is required.".to_string(),
                None,
            ),
            Self::Config(message) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_error",
                message.clone(),
                None,
            ),
            Self::Validation(message) => (StatusCode::BAD_REQUEST, "invalid_request", message.clone(), None),
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, "invalid_request", message.clone(), None),
            Self::PayloadTooLarge => (
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload_too_large",
                "Request body exceeds the configured size limit.".to_string(),
                None,
            ),
            Self::RateLimited => (
                StatusCode::TOO_MANY_REQUESTS,
                "rate_limited",
                "Rate limit exceeded for this agent.".to_string(),
                None,
            ),
            Self::Memory(e) => match e {
                M::MissingAgent => (StatusCode::BAD_REQUEST, "invalid_agent_id", e.to_string(), None),
                M::UnknownLayer(_) => (StatusCode::BAD_REQUEST, "invalid_layer", e.to_string(), None),
                M::Extraction(msg) => (StatusCode::BAD_REQUEST, "invalid_request", msg.clone(), None),
                M::InvalidGcPageSize => (StatusCode::BAD_REQUEST, "invalid_request", e.to_string(), None),
                M::InvalidContextTokenBudget | M::InvalidContextCandidateLimit { .. } => {
                    (StatusCode::BAD_REQUEST, "invalid_request", e.to_string(), None)
                }
                M::InvalidImportance { value } => (
                    StatusCode::BAD_REQUEST,
                    "invalid_importance",
                    e.to_string(),
                    Some(serde_json::json!({ "field": "importance", "value": value })),
                ),
                M::TextTooLong { .. } => (StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large", e.to_string(), None),
                M::Porting(msg) => (StatusCode::UNPROCESSABLE_ENTITY, "invalid_request", msg.clone(), None),
                M::EmbeddingMetadata(msg) => (StatusCode::INTERNAL_SERVER_ERROR, "internal_error", msg.clone(), None),
                M::EmbeddingModelMismatch { .. } => (StatusCode::CONFLICT, "conflict", e.to_string(), None),
                M::Inference(_) => {
                    tracing::error!(error = %e, "inference failure in REST handler");
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "internal_error",
                        "internal error".to_string(),
                        None,
                    )
                }
                M::EncryptionRequired => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "encryption is mandatory".to_string(),
                    None,
                ),
                M::Core(CoreError::EncryptionKeyRequired) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "encryption key required".to_string(),
                    None,
                ),
                M::Core(CoreError::WrongEncryptionKey) => (
                    StatusCode::FORBIDDEN,
                    "wrong_encryption_key",
                    "wrong encryption key".to_string(),
                    None,
                ),
                M::Core(CoreError::CorruptEncryptionMetadata) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "corrupt encryption metadata".to_string(),
                    None,
                ),
                M::Core(CoreError::StoreLocked) => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "store_locked",
                    "store is locked by another writer".to_string(),
                    None,
                ),
                M::Core(CoreError::CorruptStoreGenerationMetadata) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "corrupt store generation metadata".to_string(),
                    None,
                ),
                M::Core(CoreError::Encryption) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "encryption operation failed".to_string(),
                    None,
                ),
                M::Core(CoreError::PlaintextStoreEncryptedKeySupplied) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "plaintext store cannot be encrypted in place".to_string(),
                    None,
                ),
                _ => {
                    tracing::error!(error = %e, "internal error in REST handler");
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "internal_error",
                        "internal error".to_string(),
                        None,
                    )
                }
            },
        }
    }

    /// Attache un `field` de détail (ex. `{"field": "importance"}") sans
    /// changer le code/statut déjà déterminé par [`Self::parts`].
    #[must_use]
    pub fn with_field(self, field: &'static str) -> RestErrorWithDetails {
        RestErrorWithDetails {
            error: self,
            details: Some(serde_json::json!({ "field": field })),
        }
    }
}

/// Enveloppe une [`RestError`] avec des `details` structurés (ex. le champ
/// JSON fautif). Séparée de `RestError` pour ne pas alourdir chaque variante
/// d'un `Option<Value>` alors que la plupart des erreurs n'en ont pas besoin.
pub struct RestErrorWithDetails {
    error: RestError,
    details: Option<serde_json::Value>,
}

impl IntoResponse for RestErrorWithDetails {
    fn into_response(self) -> Response {
        render(&self.error, self.details, None)
    }
}

impl IntoResponse for RestError {
    fn into_response(self) -> Response {
        render(&self, None, None)
    }
}

fn render(err: &RestError, details: Option<serde_json::Value>, request_id: Option<String>) -> Response {
    let (status, code, message, parts_details) = err.parts();
    (
        status,
        Json(ErrorBody {
            error: ErrorDetail {
                code,
                message,
                request_id,
                details: details.or(parts_details),
            },
        }),
    )
        .into_response()
}

/// Injecte `request_id` dans un corps d'erreur JSON déjà construit. Utilisé
/// par le middleware async `http::middleware::request_id` (qui, lui, a accès
/// à `response.into_body()` en contexte `.await`) — cette fonction reste pure
/// et synchrone pour rester testable sans exécuteur tokio.
pub(crate) fn inject_request_id(mut value: serde_json::Value, request_id: &str) -> serde_json::Value {
    if let Some(error) = value.get_mut("error").and_then(|e| e.as_object_mut()) {
        error.insert("request_id".to_string(), serde_json::json!(request_id));
    }
    value
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use basemyai::MemoryError;
    use basemyai_core::CoreError;

    use super::RestError;

    #[test]
    fn wrong_encryption_key_maps_to_stable_rest_code() {
        let err = RestError::Memory(MemoryError::Core(CoreError::WrongEncryptionKey));
        let (status, code, _, _) = err.parts();
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(code, "wrong_encryption_key");
    }

    #[test]
    fn store_lock_maps_to_retryable_rest_code() {
        let err = RestError::Memory(MemoryError::Core(CoreError::StoreLocked));
        let (status, code, _, _) = err.parts();
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(code, "store_locked");
    }

    #[test]
    fn validation_error_carries_optional_field_detail() {
        let with_field = RestError::Validation("importance must be finite".to_string()).with_field("importance");
        assert_eq!(with_field.details, Some(serde_json::json!({ "field": "importance" })));
    }
}
