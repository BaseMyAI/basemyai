// SPDX-License-Identifier: BUSL-1.1
//! Types de données projetés en objets JS plein (`#[napi(object)]`). Couches en
//! `string`, scores/compteurs en `number`, conformément à la table de mapping.

use napi_derive::napi;

use basemyai::MemoryEventKind;

/// Options de production pour ouvrir une mémoire persistée. Tout est
/// optionnel : `path`/`agentId` retombent sur `~/.basemyai/config.toml` /
/// `BASEMYAI_DB_PATH` / `BASEMYAI_AGENT` (même résolution que la CLI) puis
/// sur un défaut intégré (`./basemyai.bmai`, agent `"default"` — cf.
/// `basemyai::ConfigDefaults`) ; sans `encryptionKey` ni source configurée,
/// une clé est générée et persistée dans `~/.basemyai/key`. Seul le
/// téléchargement du modèle (`allowModelDownload`) reste soumis à
/// consentement explicite — jamais silencieux (ADR-010).
#[napi(object)]
pub struct MemoryOpenOptions {
    pub path: Option<String>,
    pub agent_id: Option<String>,
    pub encryption_key: Option<String>,
    /// `raw` (default for an explicit key) or `passphrase` (Argon2id).
    pub credential_mode: Option<String>,
    pub model_path: Option<String>,
    pub allow_model_download: Option<bool>,
}

/// Un tour de conversation brut (rôle + contenu) à ingérer via
/// `Memory.observe()`. `role` n'est pas validé : libellé libre (`"user"`,
/// `"assistant"`, `"system"`, ...), simplement reporté dans le texte mémorisé.
#[napi(object)]
pub struct ConversationTurn {
    pub role: String,
    pub content: String,
}

impl From<ConversationTurn> for basemyai::ConversationTurn {
    fn from(turn: ConversationTurn) -> Self {
        basemyai::ConversationTurn::new(turn.role, turn.content)
    }
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
    /// Début inclusif de la fenêtre de validité (timestamp Unix UTC).
    pub valid_from: f64,
    /// Fin exclusive de la fenêtre de validité.
    pub valid_until: Option<f64>,
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
        let validity = r.validity;
        Self {
            id: r.id,
            text: r.text,
            layer: r.layer.table().to_string(),
            score,
            source: r.source,
            trust,
            valid_from: validity.valid_from as f64,
            valid_until: validity.valid_until.map(|value| value as f64),
        }
    }

    pub(crate) fn from_hybrid(r: basemyai::Record) -> Self {
        let trust = r.trust().as_str().to_string();
        let validity = r.validity;
        Self {
            id: r.id,
            text: r.text,
            layer: r.layer.table().to_string(),
            score: f64::from(r.score),
            source: r.source,
            trust,
            valid_from: validity.valid_from as f64,
            valid_until: validity.valid_until.map(|value| value as f64),
        }
    }
}

/// Options de compilation d'un contexte borné.
#[napi(object)]
pub struct ContextOptions {
    pub query: String,
    pub token_budget: u32,
    pub candidate_limit: Option<u32>,
    pub include_procedural: Option<bool>,
    /// `allow_all` | `exclude_imported` | `user_and_consolidation_only`.
    pub source_policy: Option<String>,
    pub explain: Option<bool>,
}

/// Item sélectionné par le Context Engine.
#[napi(object)]
pub struct ContextItem {
    pub text: String,
    pub source_memory_ids: Vec<String>,
    pub layer: String,
    pub trust: String,
    pub valid_from: f64,
    pub valid_until: Option<f64>,
    pub temporal_status: String,
    pub retrieval_score: f64,
    pub retrieval_rank: u32,
    pub estimated_tokens: u32,
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
            valid_from: item.validity.valid_from as f64,
            valid_until: item.validity.valid_until.map(|value| value as f64),
            temporal_status: temporal_status(item.temporal_status).to_string(),
            retrieval_score: f64::from(item.retrieval_score),
            retrieval_rank: clamp_u32(item.retrieval_rank),
            estimated_tokens: clamp_u32(item.estimated_tokens),
            utility_score: item.utility_score,
            value_per_token: item.value_per_token,
            freshness_score: item.freshness_score,
        }
    }
}

/// Section sémantique du bundle final.
#[napi(object)]
pub struct ContextSection {
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

/// Citation entre un fragment du bundle et un souvenir persisté.
#[napi(object)]
pub struct ContextCitation {
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

/// Candidat écarté lorsque `explain` est activé.
#[napi(object)]
pub struct ExcludedMemory {
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
#[napi(object)]
pub struct MergedMemory {
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

/// Contexte compilé, rendu et inspectable.
#[napi(object)]
pub struct ContextBundle {
    pub sections: Vec<ContextSection>,
    pub rendered: String,
    pub estimated_tokens: u32,
    pub compiled_at: f64,
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
            estimated_tokens: clamp_u32(bundle.estimated_tokens),
            compiled_at: bundle.compiled_at as f64,
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
            "sourcePolicy must be 'allow_all', 'exclude_imported', or \
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
