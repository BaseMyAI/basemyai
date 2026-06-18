//! Schémas d'entrée/sortie des outils MCP (structs `*Params` / `*Result`),
//! exposés au client via le JSON Schema généré par les macros `#[tool]` (schemars).
//! La logique vit dans [`crate::server`].

mod consolidate;
mod invalidate;
mod recall;
mod recall_graph;
mod remember;
mod stats;

pub use consolidate::{
    ApplyEntity, ApplyRelation, ConsolidateApplyParams, ConsolidateParams, ConsolidateResult, ConsolidateStatus,
};
pub use invalidate::{InvalidateParams, InvalidateResult};
pub use recall::{RecallItem, RecallParams, RecallResult};
pub use recall_graph::{EntityItem, RecallGraphParams, RecallGraphResult};
pub use remember::{RememberParams, RememberResult};
pub use stats::{StatsParams, StatsResult};

use basemyai::MemoryLayer;

use crate::error::{McpError, Result};

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
