// SPDX-License-Identifier: BUSL-1.1
//! Types de données projetés en objets JS plein (`#[napi(object)]`). Couches en
//! `string`, scores/compteurs en `number`, conformément à la table de mapping.

use napi_derive::napi;

use basemyai::MemoryEventKind;

/// Options de production pour ouvrir une mémoire persistée.
#[napi(object)]
pub struct MemoryOpenOptions {
    pub path: String,
    pub agent_id: String,
    pub encryption_key: Option<String>,
    pub model_path: Option<String>,
    pub allow_model_download: Option<bool>,
}

/// Un souvenir retourné par `recall`.
#[napi(object)]
pub struct Record {
    pub id: String,
    pub text: String,
    /// `short_term` | `episodic` | `procedural` | `semantic`.
    pub layer: String,
    /// Similarité cosinus normalisée dans `[0, 1]` (`1` = identique).
    pub score: f64,
    /// Tag wire de provenance.
    pub source: String,
    /// Provenance typée (ADR-036).
    pub trust: String,
}

impl From<basemyai::Record> for Record {
    fn from(r: basemyai::Record) -> Self {
        Self::from_vector(r)
    }
}

impl Record {
    pub(crate) fn from_vector(r: basemyai::Record) -> Self {
        let score = f64::from(r.similarity());
        let trust = r.trust().as_str().to_string();
        Self {
            id: r.id,
            text: r.text,
            layer: r.layer.table().to_string(),
            score,
            source: r.source,
            trust,
        }
    }

    pub(crate) fn from_hybrid(r: basemyai::Record) -> Self {
        let trust = r.trust().as_str().to_string();
        Self {
            id: r.id,
            text: r.text,
            layer: r.layer.table().to_string(),
            score: f64::from(r.score),
            source: r.source,
            trust,
        }
    }
}

/// Statistiques d'un agent, par couche.
#[napi(object)]
pub struct AgentStats {
    pub short_term: u32,
    pub episodic: u32,
    pub procedural: u32,
    pub semantic: u32,
    pub total: u32,
}

impl From<basemyai::AgentStats> for AgentStats {
    fn from(s: basemyai::AgentStats) -> Self {
        Self {
            short_term: clamp_u32(s.short_term),
            episodic: clamp_u32(s.episodic),
            procedural: clamp_u32(s.procedural),
            semantic: clamp_u32(s.semantic),
            total: clamp_u32(s.total()),
        }
    }
}

/// Une entité atteinte par une traversée du graphe.
#[napi(object)]
pub struct Entity {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub depth: u32,
}

impl From<basemyai::Reached> for Entity {
    fn from(r: basemyai::Reached) -> Self {
        Self {
            id: r.id,
            kind: r.kind,
            label: r.label,
            depth: r.depth,
        }
    }
}

fn clamp_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Un événement mémoire poussé à un abonné `watch` (ADR-022, live subscriptions
/// côté binding Node). Ne porte jamais le contenu du souvenir — seulement son
/// identité et la nature de la mutation, comme les payloads MCP/REST
/// équivalents ; l'abonné rappelle `recall`/`stats` par `id` s'il veut le détail.
#[napi(object)]
pub struct MemoryEventPayload {
    pub agent_id: String,
    /// `"remembered"` | `"invalidated"` | `"forgotten"` | `"consolidated"` |
    /// `"unknown"` (genre futur non reconnu — `MemoryEventKind` est `#[non_exhaustive]`).
    pub kind: String,
    /// `short_term` | `episodic` | `procedural` | `semantic`.
    pub layer: String,
    pub id: String,
}

impl From<&basemyai::MemoryEvent> for MemoryEventPayload {
    fn from(ev: &basemyai::MemoryEvent) -> Self {
        let kind = match ev.kind {
            MemoryEventKind::Remembered => "remembered",
            MemoryEventKind::Invalidated => "invalidated",
            MemoryEventKind::Forgotten => "forgotten",
            MemoryEventKind::Consolidated => "consolidated",
            // `MemoryEventKind` est `#[non_exhaustive]` : un genre futur
            // atterrit ici plutôt que de casser la compilation.
            _ => "unknown",
        };
        Self {
            agent_id: ev.agent_id.clone(),
            kind: kind.to_string(),
            layer: ev.layer.table().to_string(),
            id: ev.id.clone(),
        }
    }
}
