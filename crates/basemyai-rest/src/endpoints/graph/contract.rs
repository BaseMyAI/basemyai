// SPDX-License-Identifier: BUSL-1.1
//! Types partagés par les endpoints `graph`.

use serde::Serialize;

/// Une entité atteinte par une traversée ou un recall filtré par graphe.
#[derive(Serialize)]
pub(crate) struct EntityDto {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub depth: u32,
}

impl From<basemyai::Reached> for EntityDto {
    fn from(r: basemyai::Reached) -> Self {
        Self {
            id: r.id,
            kind: r.kind,
            label: r.label,
            depth: r.depth,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct GraphResponse {
    pub nodes: Vec<EntityDto>,
    pub truncated: bool,
}
