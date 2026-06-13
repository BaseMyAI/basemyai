//! Erreurs HTTP du sidecar : enveloppe métier → réponse JSON `{error:{code,message}}`
//! avec le statut adéquat, conforme à `ErrorResponse` de la spec OpenAPI.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// Erreur du sidecar, traduite en réponse HTTP.
///
/// `Display`/`Error` viennent de `thiserror` (requis pour `?` vers
/// `Box<dyn Error>` dans le binaire et pour le logging) ; le mapping HTTP
/// stable (`parts()`) reste séparé de ces messages.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RestError {
    /// Jeton Bearer absent ou invalide.
    #[error("missing or invalid Bearer token")]
    Unauthorized,
    /// `agent_id` vide / hors contrainte.
    #[error("a valid agent_id is required")]
    InvalidAgent,
    /// Erreur propagée depuis la couche mémoire.
    #[error(transparent)]
    Memory(#[from] basemyai::MemoryError),
}

#[derive(Serialize)]
struct ErrorDetail {
    code: &'static str,
    message: String,
}

#[derive(Serialize)]
struct ErrorBody {
    error: ErrorDetail,
}

impl RestError {
    /// `(statut HTTP, code stable, message)` pour cette erreur.
    fn parts(&self) -> (StatusCode, &'static str, String) {
        use basemyai::MemoryError as M;
        use basemyai_core::CoreError;
        match self {
            Self::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "UNAUTHORIZED",
                "Missing or invalid Bearer token.".to_string(),
            ),
            Self::InvalidAgent => (
                StatusCode::BAD_REQUEST,
                "MISSING_AGENT",
                "A valid agent_id is required.".to_string(),
            ),
            Self::Memory(e) => match e {
                M::MissingAgent => (StatusCode::BAD_REQUEST, "MISSING_AGENT", e.to_string()),
                M::UnknownLayer(_) => (StatusCode::BAD_REQUEST, "UNKNOWN_LAYER", e.to_string()),
                M::EncryptionRequired => (StatusCode::INTERNAL_SERVER_ERROR, "ENCRYPTION_REQUIRED", e.to_string()),
                M::Core(CoreError::Encryption) => {
                    (StatusCode::INTERNAL_SERVER_ERROR, "ENCRYPTION_REQUIRED", e.to_string())
                }
                _ => (StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL_ERROR", e.to_string()),
            },
        }
    }
}

impl IntoResponse for RestError {
    fn into_response(self) -> Response {
        let (status, code, message) = self.parts();
        (
            status,
            Json(ErrorBody {
                error: ErrorDetail { code, message },
            }),
        )
            .into_response()
    }
}
