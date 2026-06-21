//! Types de données projetés en objets JS plein (`#[napi(object)]`). Couches en
//! `string`, scores/compteurs en `number`, conformément à la table de mapping.

use napi_derive::napi;

/// Options de production pour ouvrir une mémoire persistée.
#[napi(object)]
pub struct MemoryOpenOptions {
    pub path: String,
    pub agent_id: String,
    pub encryption_key: String,
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
}

impl From<basemyai::Record> for Record {
    fn from(r: basemyai::Record) -> Self {
        let score = f64::from(r.similarity());
        Self {
            id: r.id,
            text: r.text,
            layer: r.layer.table().to_string(),
            score,
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
