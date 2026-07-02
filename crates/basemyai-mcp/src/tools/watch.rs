//! Outil `watch` : démarre le relais des événements mémoire d'un agent vers
//! ce client MCP, via des notifications `notifications/message` (ADR-022,
//! seconde vague — voir `docs/TODO.md` §M6.2).

use rmcp::schemars;
use serde::{Deserialize, Serialize};

/// Paramètres de `watch`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct WatchParams {
    /// Identifiant de l'agent (tenant) dont on veut suivre la mémoire.
    pub agent_id: String,
    /// Couche optionnelle (`short_term`/`episodic`/`procedural`/`semantic`).
    /// Absente : tous les genres d'événements de l'agent sont relayés.
    #[serde(default)]
    pub layer: Option<String>,
}

/// Résultat de `watch`. L'outil renvoie **immédiatement** : les événements
/// arrivent ensuite de façon asynchrone, en notifications, pour la durée de
/// vie de la session MCP (ou jusqu'à déconnexion).
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct WatchResult {
    /// `true` une fois le relais démarré côté serveur.
    pub watching: bool,
}
