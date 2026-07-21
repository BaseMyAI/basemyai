// SPDX-License-Identifier: BUSL-1.1
//! Types partagés par plusieurs endpoints `memories` : le DTO de souvenir et
//! la réponse de recall commune à `recall`/`recall_hybrid`.

use serde::Serialize;

/// Un souvenir retourné par `recall`/`recall_hybrid`. DTO REST indépendant du
/// `basemyai::Record` interne — un renommage de champ côté `basemyai` ne doit
/// jamais changer silencieusement ce JSON.
#[derive(Serialize)]
pub(crate) struct MemoryDto {
    pub id: String,
    pub text: String,
    pub layer: String,
    pub score: f32,
    pub source: String,
    pub trust: String,
}

impl MemoryDto {
    pub(crate) fn from_vector(r: basemyai::Record) -> Self {
        let trust = r.trust().as_str().to_string();
        let score = r.similarity();
        Self {
            id: r.id,
            text: r.text,
            layer: r.layer.table().to_string(),
            score,
            source: r.source,
            trust,
        }
    }

    /// Comme [`Self::from_vector`], mais `score` porte le score RRF fusionné
    /// (`recall_hybrid`), pas la similarité cosinus.
    pub(crate) fn from_hybrid(r: basemyai::Record) -> Self {
        let trust = r.trust().as_str().to_string();
        Self {
            id: r.id,
            text: r.text,
            layer: r.layer.table().to_string(),
            score: r.score,
            source: r.source,
            trust,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct RecallResponse {
    pub results: Vec<MemoryDto>,
    pub truncated: bool,
}

/// Temps Unix courant (secondes, UTC). `0` si l'horloge est antérieure à l'epoch.
pub(super) fn now_unix() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}
