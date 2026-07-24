// SPDX-License-Identifier: BUSL-1.1
//! Petits alias de réponse partagés entre endpoints. Volontairement minimal :
//! la plupart des handlers renvoient directement `Json<T>` ou `StatusCode`,
//! ce module n'existe que pour les deux formes répétées dans plusieurs domaines.

use axum::Json;
use axum::http::StatusCode;

use crate::http::error::RestError;

/// Alias du type de retour standard d'un handler : `T` en JSON, ou une
/// [`RestError`] déjà mappée sur son code/statut stable.
pub type ApiResult<T> = Result<Json<T>, RestError>;

/// Réponse `201 Created` + corps JSON — création d'un souvenir/entité.
pub fn created<T: serde::Serialize>(body: T) -> (StatusCode, Json<T>) {
    (StatusCode::CREATED, Json(body))
}
