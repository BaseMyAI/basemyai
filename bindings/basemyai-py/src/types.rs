// SPDX-License-Identifier: BUSL-1.1
//! Types de données exposés à Python : [`Record`], [`AgentStats`], [`Entity`]
//! et bundle du Context Engine.
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
    /// Tag wire de provenance (`user`, `consolidation`, `import`, …).
    pub source: String,
    /// Provenance typée (ADR-036).
    pub trust: String,
    /// Début inclusif de la fenêtre de validité (timestamp Unix UTC).
    pub valid_from: i64,
    /// Fin exclusive de la fenêtre de validité, ou `None`.
    pub valid_until: Option<i64>,
}

impl From<basemyai::Record> for Record {
    fn from(r: basemyai::Record) -> Self {
        Self::from_vector(r)
    }
}

impl Record {
    /// Recall vectoriel : `score` = similarité cosinus normalisée.
    pub(crate) fn from_vector(r: basemyai::Record) -> Self {
        let score = r.similarity();
        let trust = r.trust().as_str().to_string();
        let validity = r.validity;
        Self {
            id: r.id,
            text: r.text,
            layer: r.layer.table().to_string(),
            score,
            source: r.source,
            trust,
            valid_from: validity.valid_from,
            valid_until: validity.valid_until,
        }
    }

    /// Recall hybride : `score` = score RRF fusionné (pas une similarité).
    pub(crate) fn from_hybrid(r: basemyai::Record) -> Self {
        let trust = r.trust().as_str().to_string();
        let validity = r.validity;
        Self {
            id: r.id,
            text: r.text,
            layer: r.layer.table().to_string(),
            score: r.score,
            source: r.source,
            trust,
            valid_from: validity.valid_from,
            valid_until: validity.valid_until,
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

/// Item sélectionné par le Context Engine.
#[derive(Clone)]
#[pyclass(frozen, get_all, skip_from_py_object)]
pub(crate) struct ContextItem {
    pub text: String,
    pub source_memory_ids: Vec<String>,
    pub layer: String,
    pub trust: String,
    pub valid_from: i64,
    pub valid_until: Option<i64>,
    pub temporal_status: String,
    pub retrieval_score: f32,
    pub retrieval_rank: usize,
    pub estimated_tokens: usize,
    pub utility_score: f64,
    pub value_per_token: f64,
    pub freshness_score: f64,
}

impl From<basemyai::ContextItem> for ContextItem {
    fn from(item: basemyai::ContextItem) -> Self {
        Self {
            text: item.text,
            source_memory_ids: item.source_memory_ids,
            layer: item.layer.table().to_string(),
            trust: item.trust.as_str().to_string(),
            valid_from: item.validity.valid_from,
            valid_until: item.validity.valid_until,
            temporal_status: temporal_status(item.temporal_status).to_string(),
            retrieval_score: item.retrieval_score,
            retrieval_rank: item.retrieval_rank,
            estimated_tokens: item.estimated_tokens,
            utility_score: item.utility_score,
            value_per_token: item.value_per_token,
            freshness_score: item.freshness_score,
        }
    }
}

/// Section sémantique d'un contexte compilé.
#[derive(Clone)]
#[pyclass(frozen, get_all, skip_from_py_object)]
pub(crate) struct ContextSection {
    pub kind: String,
    pub items: Vec<ContextItem>,
}

impl From<basemyai::ContextSection> for ContextSection {
    fn from(section: basemyai::ContextSection) -> Self {
        Self {
            kind: section_kind(section.kind).to_string(),
            items: section.items.into_iter().map(ContextItem::from).collect(),
        }
    }
}

/// Citation entre le rendu et un souvenir persisté.
#[derive(Clone)]
#[pyclass(frozen, get_all, skip_from_py_object)]
pub(crate) struct ContextCitation {
    pub memory_id: String,
    pub section: String,
}

impl From<basemyai::ContextCitation> for ContextCitation {
    fn from(citation: basemyai::ContextCitation) -> Self {
        Self {
            memory_id: citation.memory_id,
            section: section_kind(citation.section).to_string(),
        }
    }
}

/// Candidat écarté lorsque `explain=True`.
#[derive(Clone)]
#[pyclass(frozen, get_all, skip_from_py_object)]
pub(crate) struct ExcludedMemory {
    pub memory_id: String,
    pub reason: String,
    pub temporal_status: String,
}

impl From<basemyai::ExcludedMemory> for ExcludedMemory {
    fn from(excluded: basemyai::ExcludedMemory) -> Self {
        Self {
            memory_id: excluded.memory_id,
            reason: exclusion_reason(excluded.reason).to_string(),
            temporal_status: temporal_status(excluded.temporal_status).to_string(),
        }
    }
}

/// Trace de déduplication exacte.
#[derive(Clone)]
#[pyclass(frozen, get_all, skip_from_py_object)]
pub(crate) struct MergedMemory {
    pub memory_id: String,
    pub representative_memory_id: String,
}

impl From<basemyai::MergedMemory> for MergedMemory {
    fn from(merged: basemyai::MergedMemory) -> Self {
        Self {
            memory_id: merged.memory_id,
            representative_memory_id: merged.representative_memory_id,
        }
    }
}

/// Résultat structuré et rendu de `Memory.compile_context`.
#[pyclass(frozen, get_all)]
pub(crate) struct ContextBundle {
    pub sections: Vec<ContextSection>,
    pub rendered: String,
    pub estimated_tokens: usize,
    pub compiled_at: i64,
    pub total_utility: f64,
    pub citations: Vec<ContextCitation>,
    pub merged: Vec<MergedMemory>,
    pub excluded: Vec<ExcludedMemory>,
}

impl From<basemyai::ContextBundle> for ContextBundle {
    fn from(bundle: basemyai::ContextBundle) -> Self {
        Self {
            sections: bundle.sections.into_iter().map(ContextSection::from).collect(),
            rendered: bundle.rendered,
            estimated_tokens: bundle.estimated_tokens,
            compiled_at: bundle.compiled_at,
            total_utility: bundle.total_utility,
            citations: bundle.citations.into_iter().map(ContextCitation::from).collect(),
            merged: bundle.merged.into_iter().map(MergedMemory::from).collect(),
            excluded: bundle.excluded.into_iter().map(ExcludedMemory::from).collect(),
        }
    }
}

pub(crate) fn parse_source_policy(value: &str) -> Result<basemyai::ContextSourcePolicy, String> {
    match value {
        "allow_all" => Ok(basemyai::ContextSourcePolicy::AllowAll),
        "exclude_imported" => Ok(basemyai::ContextSourcePolicy::ExcludeImported),
        "user_and_consolidation_only" => Ok(basemyai::ContextSourcePolicy::UserAndConsolidationOnly),
        _ => Err(format!(
            "source_policy must be 'allow_all', 'exclude_imported', or \
             'user_and_consolidation_only', got {value:?}"
        )),
    }
}

fn section_kind(kind: basemyai::ContextSectionKind) -> &'static str {
    match kind {
        basemyai::ContextSectionKind::WorkingContext => "working_context",
        basemyai::ContextSectionKind::CurrentFacts => "current_facts",
        basemyai::ContextSectionKind::Procedures => "procedures",
        basemyai::ContextSectionKind::RecentEvents => "recent_events",
        _ => "unknown",
    }
}

fn temporal_status(status: basemyai::ContextTemporalStatus) -> &'static str {
    match status {
        basemyai::ContextTemporalStatus::Current => "current",
        basemyai::ContextTemporalStatus::Scheduled => "scheduled",
        basemyai::ContextTemporalStatus::Expired => "expired",
        _ => "unknown",
    }
}

fn exclusion_reason(reason: basemyai::ExclusionReason) -> &'static str {
    match reason {
        basemyai::ExclusionReason::SourceFiltered => "source_filtered",
        basemyai::ExclusionReason::NotCurrentlyValid => "not_currently_valid",
        basemyai::ExclusionReason::TokenBudget => "token_budget",
        _ => "unknown",
    }
}
