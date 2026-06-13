//! Mapping [`basemyai::MemoryError`] → [`napi::Error`].
//!
//! NAPI projette `napi::Error` en `Error` JS dont `.code` reflète le `Status`.
//! Les entrées invalides (agent_id, couche) → `InvalidArg` ; le reste →
//! `GenericFailure`. Des classes d'erreur JS dédiées sont une couche TypeScript
//! optionnelle au-dessus de ces codes.

use napi::{Error, Status};

/// Convertit une erreur mémoire en `napi::Error` typée par `Status`.
pub(crate) fn to_napi(e: basemyai::MemoryError) -> Error {
    use basemyai::MemoryError as E;
    let status = match e {
        E::MissingAgent | E::UnknownLayer(_) => Status::InvalidArg,
        _ => Status::GenericFailure,
    };
    Error::new(status, e.to_string())
}
