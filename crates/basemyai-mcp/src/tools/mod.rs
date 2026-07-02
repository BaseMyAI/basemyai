//! Schémas d'entrée/sortie des outils MCP (structs `*Params` / `*Result`),
//! exposés au client via le JSON Schema généré par les macros `#[tool]` (schemars).
//! La logique vit dans [`crate::server`].

mod consolidate;
mod invalidate;
mod recall;
mod recall_graph;
mod remember;
mod stats;
mod watch;

pub use consolidate::{
    ApplyEntity, ApplyRelation, ConsolidateApplyParams, ConsolidateParams, ConsolidateResult, ConsolidateStatus,
};
pub use invalidate::{InvalidateParams, InvalidateResult};
pub use recall::{RecallItem, RecallParams, RecallResult};
pub use recall_graph::{EntityItem, RecallGraphParams, RecallGraphResult};
pub use remember::{RememberParams, RememberResult};
pub use stats::{StatsParams, StatsResult};
pub use watch::{WatchParams, WatchResult};

use basemyai::MemoryLayer;

use crate::error::{McpError, Result};

/// Bornes de validation des entrées, alignées sur celles du sidecar REST
/// (`crates/basemyai-rest/src/routes.rs`) et sur `openapi-sidecar.yaml`.
pub(crate) const MAX_AGENT_ID_LEN: usize = 128;
pub(crate) const MAX_TEXT_LEN: usize = 65_536;
pub(crate) const MAX_QUERY_LEN: usize = 4096;
pub(crate) const MIN_K: usize = 1;
pub(crate) const MAX_K: usize = 100;
pub(crate) const MIN_DEPTH: u32 = 1;
pub(crate) const MAX_DEPTH: u32 = 10;

/// Valide `agent_id` : non vide et borné à [`MAX_AGENT_ID_LEN`] caractères.
///
/// # Errors
/// [`McpError::Validation`] si la borne est dépassée.
pub(crate) fn validate_agent_id(agent_id: &str) -> Result<()> {
    if agent_id.is_empty() || agent_id.chars().count() > MAX_AGENT_ID_LEN {
        return Err(McpError::Validation(format!(
            "agent_id must be 1..={MAX_AGENT_ID_LEN} characters"
        )));
    }
    Ok(())
}

/// Valide `text` (`remember`) : non vide et borné à [`MAX_TEXT_LEN`] caractères.
///
/// # Errors
/// [`McpError::Validation`] si la borne est dépassée.
pub(crate) fn validate_text(text: &str) -> Result<()> {
    if text.is_empty() || text.chars().count() > MAX_TEXT_LEN {
        return Err(McpError::Validation(format!(
            "text must be 1..={MAX_TEXT_LEN} characters"
        )));
    }
    Ok(())
}

/// Valide `query` (`recall`/`recall_hybrid`) : non vide et borné à [`MAX_QUERY_LEN`] caractères.
///
/// # Errors
/// [`McpError::Validation`] si la borne est dépassée.
pub(crate) fn validate_query(query: &str) -> Result<()> {
    if query.is_empty() || query.chars().count() > MAX_QUERY_LEN {
        return Err(McpError::Validation(format!(
            "query must be 1..={MAX_QUERY_LEN} characters"
        )));
    }
    Ok(())
}

/// Valide `k` (`recall`/`recall_hybrid`) : borné à [`MIN_K`]..=[`MAX_K`].
///
/// # Errors
/// [`McpError::Validation`] si la borne est dépassée.
pub(crate) fn validate_k(k: usize) -> Result<()> {
    if !(MIN_K..=MAX_K).contains(&k) {
        return Err(McpError::Validation(format!("k must be {MIN_K}..={MAX_K}")));
    }
    Ok(())
}

/// Valide `max_depth` (`recall_graph`) : borné à [`MIN_DEPTH`]..=[`MAX_DEPTH`].
///
/// # Errors
/// [`McpError::Validation`] si la borne est dépassée.
pub(crate) fn validate_max_depth(max_depth: u32) -> Result<()> {
    if !(MIN_DEPTH..=MAX_DEPTH).contains(&max_depth) {
        return Err(McpError::Validation(format!(
            "max_depth must be {MIN_DEPTH}..={MAX_DEPTH}"
        )));
    }
    Ok(())
}

/// Valide `start` (`recall_graph`) : non vide.
///
/// # Errors
/// [`McpError::Validation`] si vide.
pub(crate) fn validate_start(start: &str) -> Result<()> {
    if start.is_empty() {
        return Err(McpError::Validation("start must not be empty".to_string()));
    }
    Ok(())
}

/// Schéma JSON d'un entier non négatif **sans `format`**.
///
/// schemars émet `format: "uint"` / `"uint32"` pour `usize`/`u32`, formats que la
/// spec JSON Schema ne définit pas — les clients MCP (Claude Code) loguent alors
/// `unknown format "uint" ignored`. On force un schéma standard via `schema_with`.
pub(crate) fn count_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({ "type": "integer", "minimum": 0 })
}

/// Parse un nom de couche fourni par le client en [`MemoryLayer`].
///
/// # Errors
/// [`McpError::InvalidLayer`] si le nom n'est pas une couche connue.
pub(crate) fn parse_layer(name: &str) -> Result<MemoryLayer> {
    MemoryLayer::from_table(name).map_err(|_| McpError::InvalidLayer(name.to_string()))
}

/// Tronque une liste sérialisable pour qu'elle tienne sous `max_bytes` une fois
/// en JSON (best-effort : on retire les derniers éléments, déjà les moins
/// pertinents puisque triés par score). Retourne `(éléments_conservés, tronqué)`.
pub(crate) fn truncate_to_fit<T: serde::Serialize>(mut items: Vec<T>, max_bytes: usize) -> (Vec<T>, bool) {
    let mut truncated = false;
    while !items.is_empty() {
        match serde_json::to_vec(&items) {
            Ok(bytes) if bytes.len() <= max_bytes => break,
            _ => {
                items.pop();
                truncated = true;
            }
        }
    }
    (items, truncated)
}
