// SPDX-License-Identifier: BUSL-1.1
//! Outil `invalidate` : soft-delete d'un souvenir (fixe `valid_until = now`).

use rmcp::schemars;
use serde::{Deserialize, Serialize};

/// Paramètres de `invalidate`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct InvalidateParams {
    /// Identifiant de l'agent (tenant) propriétaire du souvenir.
    pub agent_id: String,
    /// UUID du souvenir à invalider.
    pub id: String,
}

/// Résultat de `invalidate`.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct InvalidateResult {
    /// `true` une fois l'invalidation appliquée (idempotent).
    pub invalidated: bool,
}
