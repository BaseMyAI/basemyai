//! Types de données exposés à Python : [`Record`], [`AgentStats`], [`Entity`].
//! Couches mémoire représentées en `str` (jamais en `int`), conformément à la
//! table de mapping cross-language.

use pyo3::prelude::*;

/// Un souvenir retourné par `recall`.
#[pyclass(frozen, get_all)]
pub struct Record {
    /// UUID du souvenir.
    pub id: String,
    /// Contenu mémorisé.
    pub text: String,
    /// Couche mémoire (`short_term` | `episodic` | `procedural` | `semantic`).
    pub layer: String,
    /// Similarité cosinus normalisée dans `[0, 1]` (`1` = identique).
    pub score: f32,
}

impl From<basemyai::Record> for Record {
    fn from(r: basemyai::Record) -> Self {
        let score = r.similarity();
        Self {
            id: r.id,
            text: r.text,
            layer: r.layer.table().to_string(),
            score,
        }
    }
}

#[pymethods]
impl Record {
    fn __repr__(&self) -> String {
        format!("Record(id={:?}, layer={:?}, score={})", self.id, self.layer, self.score)
    }
}

/// Statistiques d'un agent, par couche.
#[pyclass(frozen, get_all)]
pub struct AgentStats {
    pub short_term: usize,
    pub episodic: usize,
    pub procedural: usize,
    pub semantic: usize,
    pub total: usize,
}

impl From<basemyai::AgentStats> for AgentStats {
    fn from(s: basemyai::AgentStats) -> Self {
        Self {
            short_term: s.short_term,
            episodic: s.episodic,
            procedural: s.procedural,
            semantic: s.semantic,
            total: s.total(),
        }
    }
}

#[pymethods]
impl AgentStats {
    fn __repr__(&self) -> String {
        format!(
            "AgentStats(short_term={}, episodic={}, procedural={}, semantic={}, total={})",
            self.short_term, self.episodic, self.procedural, self.semantic, self.total
        )
    }
}

/// Une entité atteinte par une traversée du graphe.
#[pyclass(frozen, get_all)]
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

#[pymethods]
impl Entity {
    fn __repr__(&self) -> String {
        format!(
            "Entity(id={:?}, kind={:?}, label={:?}, depth={})",
            self.id, self.kind, self.label, self.depth
        )
    }
}

/// Un événement mémoire poussé par `Memory.watch()` (ADR-022, seconde vague).
/// Payload minimal : identité du souvenir + nature de la mutation, jamais le
/// contenu (rappeler `recall`/`stats` par `id` pour le détail).
#[pyclass(frozen, get_all)]
pub struct WatchEvent {
    /// Agent propriétaire (toujours celui de la `Memory` qui a émis l'itérateur).
    pub agent_id: String,
    /// `"remembered"` | `"invalidated"` | `"forgotten"` | `"consolidated"`.
    pub kind: String,
    /// Couche mémoire (`short_term` | `episodic` | `procedural` | `semantic`).
    pub layer: String,
    /// UUID du souvenir/fait affecté.
    pub id: String,
}

impl From<basemyai::MemoryEvent> for WatchEvent {
    fn from(ev: basemyai::MemoryEvent) -> Self {
        let kind = match ev.kind {
            basemyai::MemoryEventKind::Remembered => "remembered",
            basemyai::MemoryEventKind::Invalidated => "invalidated",
            basemyai::MemoryEventKind::Forgotten => "forgotten",
            basemyai::MemoryEventKind::Consolidated => "consolidated",
            // `MemoryEventKind` est `#[non_exhaustive]` : un genre futur atterrit
            // ici plutôt que de casser la compilation.
            _ => "unknown",
        };
        Self {
            agent_id: ev.agent_id,
            kind: kind.to_string(),
            layer: ev.layer.table().to_string(),
            id: ev.id,
        }
    }
}

#[pymethods]
impl WatchEvent {
    fn __repr__(&self) -> String {
        format!(
            "WatchEvent(kind={:?}, layer={:?}, id={:?})",
            self.kind, self.layer, self.id
        )
    }
}
