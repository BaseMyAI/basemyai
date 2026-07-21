// SPDX-License-Identifier: BUSL-1.1
//! Construction de l'état applicatif de production : charge la config,
//! construit le provider (store + embedder), assemble l'[`AppState`].
//! Extrait de `main.rs` pour rester testable et pour que `main.rs` reste
//! une simple séquence d'appels (§8 — main.rs minimal).

use std::sync::Arc;

use crate::config::{self, StartupConfig};
use crate::context::{AppState, MemoryRegistry};
use crate::http::RestError;
use crate::provider;

/// Charge `StartupConfig`/`RuntimeConfig`, les valide, construit le provider
/// de production et retourne un [`AppState`] prêt à servir.
///
/// # Errors
/// [`RestError::Config`] si la configuration est invalide ou le provider ne
/// peut être construit (clé introuvable, modèle non provisionné, store
/// inouvrable).
#[cfg(feature = "embed")]
pub async fn build_state() -> Result<(StartupConfig, AppState), RestError> {
    let (startup, runtime) = config::load()?;
    config::validate(&startup, &runtime)?;

    let file_provider = provider::build(&startup)
        .await
        .map_err(|e| RestError::Config(e.to_string()))?;

    let registry = MemoryRegistry::new(Arc::new(file_provider), runtime.agent_policy.clone());
    let state = AppState::new(Arc::new(registry), Arc::new(runtime));
    Ok((startup, state))
}
