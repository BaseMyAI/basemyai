// SPDX-License-Identifier: BUSL-1.1
//! Validation stricte des champs d'entrée, centralisée pour ne pas la
//! dupliquer par endpoint. Chaque fonction correspond à une contrainte
//! documentée dans `openapi.yaml`.

use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequest, Request};
use axum::http::StatusCode;
use basemyai::Validity;
use serde::de::DeserializeOwned;

use crate::http::error::RestError;

/// Remplace `axum::Json` comme extracteur de **corps de requête** : un JSON
/// malformé ou un `Content-Type` incorrect passe par [`RestError`] (donc par
/// l'enveloppe d'erreur stable `{"error": {...}}`) plutôt que par la réponse
/// texte brute par défaut d'Axum — y compris le statut `413` d'un corps trop
/// gros (`JsonRejection::BytesRejection`, propagé par `RequestBodyLimitLayer`),
/// que la conversion naïve en `BadRequest`/400 écrasait auparavant. Nommé
/// différemment d'`axum::Json` (qui reste utilisé tel quel pour les réponses,
/// dont la sérialisation ne peut pas échouer) pour ne jamais les confondre
/// dans un handler.
pub struct JsonBody<T>(pub T);

impl<S, T> FromRequest<S> for JsonBody<T>
where
    axum::Json<T>: FromRequest<S, Rejection = JsonRejection>,
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = RestError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match axum::Json::<T>::from_request(req, state).await {
            Ok(axum::Json(value)) => Ok(JsonBody(value)),
            Err(rejection) if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE => Err(RestError::PayloadTooLarge),
            Err(rejection) => Err(RestError::BadRequest(rejection.to_string())),
        }
    }
}

pub const MAX_AGENT_ID_LEN: usize = 128;
pub const MAX_QUERY_LEN: usize = 4096;
pub const MAX_TEXT_LEN: usize = 65_536;
pub const MIN_K: usize = 1;
pub const MAX_K: usize = 100;
pub const MIN_GRAPH_DEPTH: u32 = 1;
pub const MAX_GRAPH_DEPTH: u32 = 10;
/// Plafond d'un lot `remember_batch` (au-delà, une seule passe d'embedding
/// deviendrait une opération de minutes, pas de secondes).
pub const MAX_BATCH_LEN: usize = 500;
/// Plafond d'un import JSONL en octets (`import_jsonl_with_options` charge
/// tout le texte en mémoire avant la première écriture, fail-fast).
pub const MAX_IMPORT_BYTES: usize = 16 * 1024 * 1024;

/// Valide `agent_id` : non vide et borné à [`MAX_AGENT_ID_LEN`] caractères.
///
/// # Errors
/// [`RestError::Validation`] si la borne est dépassée.
pub fn validate_agent_id(agent_id: &str) -> Result<(), RestError> {
    if agent_id.is_empty() || agent_id.chars().count() > MAX_AGENT_ID_LEN {
        return Err(RestError::InvalidAgent);
    }
    Ok(())
}

/// Valide `query` (`recall`/`recall_hybrid`/`compile_context`) : non vide et
/// borné à [`MAX_QUERY_LEN`] caractères.
///
/// # Errors
/// [`RestError::Validation`] si la borne est dépassée.
pub fn validate_query(query: &str) -> Result<(), RestError> {
    if query.is_empty() || query.chars().count() > MAX_QUERY_LEN {
        return Err(RestError::Validation(format!(
            "query must be 1..={MAX_QUERY_LEN} characters"
        )));
    }
    Ok(())
}

/// Valide `text` (`remember`) : non vide et borné à [`MAX_TEXT_LEN`] caractères.
/// La limite exacte (en octets, pas en caractères) reste appliquée côté
/// `basemyai` ([`basemyai::MemoryError::TextTooLong`]) ; cette borne rejette
/// tôt le cas évident sans payer l'embedding.
///
/// # Errors
/// [`RestError::Validation`] si la borne est dépassée.
pub fn validate_text(text: &str) -> Result<(), RestError> {
    if text.is_empty() || text.chars().count() > MAX_TEXT_LEN {
        return Err(RestError::Validation(format!(
            "text must be 1..={MAX_TEXT_LEN} characters"
        )));
    }
    Ok(())
}

/// Valide un lot de textes (`remember_batch`) : non vide, chaque élément
/// valide, borné à [`MAX_BATCH_LEN`] éléments.
///
/// # Errors
/// [`RestError::Validation`] si le lot est vide, trop grand, ou contient un
/// texte invalide.
pub fn validate_batch(texts: &[String]) -> Result<(), RestError> {
    if texts.is_empty() {
        return Err(RestError::Validation("texts must not be empty".to_string()));
    }
    if texts.len() > MAX_BATCH_LEN {
        return Err(RestError::Validation(format!(
            "texts must contain at most {MAX_BATCH_LEN} items, got {}",
            texts.len()
        )));
    }
    for text in texts {
        validate_text(text)?;
    }
    Ok(())
}

/// Valide `k` (`recall`/`recall_hybrid`) : borné à [`MIN_K`]..=[`MAX_K`].
///
/// # Errors
/// [`RestError::Validation`] si la borne est dépassée.
pub fn validate_k(k: usize) -> Result<(), RestError> {
    if !(MIN_K..=MAX_K).contains(&k) {
        return Err(RestError::Validation(format!("k must be {MIN_K}..={MAX_K}")));
    }
    Ok(())
}

/// Valide `max_depth` (`recall_graph`/`graph traverse`) : borné à
/// [`MIN_GRAPH_DEPTH`]..=[`MAX_GRAPH_DEPTH`].
///
/// # Errors
/// [`RestError::Validation`] si la borne est dépassée.
pub fn validate_graph_depth(max_depth: u32) -> Result<(), RestError> {
    if !(MIN_GRAPH_DEPTH..=MAX_GRAPH_DEPTH).contains(&max_depth) {
        return Err(RestError::Validation(format!(
            "max_depth must be {MIN_GRAPH_DEPTH}..={MAX_GRAPH_DEPTH}"
        )));
    }
    Ok(())
}

/// Valide qu'un identifiant de nœud de graphe (`start`, `id` d'entité) n'est
/// pas vide.
///
/// # Errors
/// [`RestError::Validation`] si vide.
pub fn validate_non_empty(field: &'static str, value: &str) -> Result<(), RestError> {
    if value.is_empty() {
        return Err(RestError::Validation(format!("{field} must not be empty")));
    }
    Ok(())
}

/// Valide une importance (`remember` avec importance explicite) : finie.
/// La borne définitive (négatif accepté, `NaN`/infini rejetés) est de toute
/// façon appliquée par `basemyai` — cette validation ne fait que rejeter tôt
/// avec un message REST-friendly plutôt que de payer l'embedding d'abord.
///
/// # Errors
/// [`RestError::Validation`] si non finie.
pub fn validate_importance(value: f64) -> Result<(), RestError> {
    if !value.is_finite() {
        return Err(RestError::Validation(format!(
            "importance must be a finite number, got {value}"
        )));
    }
    Ok(())
}

/// Valide qu'une fenêtre de validité explicite est cohérente :
/// `valid_until`, si fourni, doit être strictement supérieur à `valid_from`.
///
/// # Errors
/// [`RestError::Validation`] si `valid_until <= valid_from`.
pub fn validate_validity(validity: &Validity) -> Result<(), RestError> {
    if let Some(until) = validity.valid_until
        && until <= validity.valid_from
    {
        return Err(RestError::Validation(format!(
            "valid_until ({until}) must be greater than valid_from ({})",
            validity.valid_from
        )));
    }
    Ok(())
}

/// Valide la taille d'un import JSONL (`agents:import`) avant tout parsing —
/// `import_jsonl_with_options` charge tout le texte en mémoire.
///
/// # Errors
/// [`RestError::Validation`] si le contenu dépasse [`MAX_IMPORT_BYTES`].
pub fn validate_import_size(jsonl: &str) -> Result<(), RestError> {
    if jsonl.len() > MAX_IMPORT_BYTES {
        return Err(RestError::Validation(format!(
            "import payload must be at most {MAX_IMPORT_BYTES} bytes, got {}",
            jsonl.len()
        )));
    }
    Ok(())
}
