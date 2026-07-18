// SPDX-License-Identifier: BUSL-1.1
//! # basemyai
//!
//! Moteur de **mémoire pour agents IA**, posé sur [`basemyai_core`].
//!
//! Ajoute au socle agnostique la *sémantique* mémoire : les 4 couches
//! ([`MemoryLayer`]), le RAG temporel ([`temporal`]), l'isolation par
//! [`AgentId`], le chiffrement obligatoire, et le setup hardware-aware
//! (module `provision`).
//!
//! Ces concepts vivent **ici**, jamais dans `basemyai-core` (ADR-001).

// Scaffold : les corps réels arrivent à la phase d'implémentation.
#![allow(dead_code)]

mod cognition;
pub mod config;
mod context;
mod error;
pub mod maintenance;
mod memory;
pub mod provision;
mod retrieval;
pub mod storage;
pub mod temporal;

pub use basemyai_core::Metric;
pub use cognition::{
    ConsolidationInput, ConsolidationReport, ExtractedEntity, ExtractedRelation, Extraction, Graph, LlmInference,
    MAX_CONSOLIDATION_ENTITIES, MAX_CONSOLIDATION_FACTS, MAX_CONSOLIDATION_RELATIONS, Reached, apply_extraction,
    consolidate, consolidation_prompt, parse_extraction, validate_extraction_bounds,
};
pub use config::ConfigDefaults;
pub use context::{
    ApproximateTokenEstimator, ContextBundle, ContextCitation, ContextItem, ContextRequest, ContextSection,
    ContextSectionKind, ContextSourcePolicy, ContextTemporalStatus, ExcludedMemory, ExclusionReason,
    MAX_CONTEXT_CANDIDATES, MergedMemory, TokenEstimator,
};
pub use error::{MemoryError, Result};
pub use maintenance::{
    AdaptiveForgettingPolicy, AdaptiveForgettingTask, ConsolidationTask, ExpiredGcReport, ExpiredMemoryGcTask,
    ForgettingReport,
};
#[cfg(feature = "test-util")]
pub use memory::HashEmbedder;
pub use memory::{
    AgentId, AgentStats, ConversationTurn, ImportReport, MAX_TEXT_LEN, Memory, MemoryEvent, MemoryEventKind,
    MemoryLayer, MemorySubscription, RecallOptions, Record, SOURCE_CONSOLIDATION, SOURCE_IMPORT, SOURCE_USER,
    TrustLevel,
};
pub use storage::BMAI_FORMAT_VERSION;

/// Dimension des embeddings du baseline (`all-MiniLM-L6-v2`) — modèle unique
/// en V1 (CLAUDE.md).
pub const EMBEDDING_DIM: usize = 384;
pub use provision::{
    AnythingLlmBackend, BASELINE_DIM, BASELINE_MODEL_ID, BackendKind, GpuInfo, HardwareProfile, KNOWN_MODELS,
    KnownModel, LlmOption, LlmProvision, ModelProvision, OllamaBackend, OpenAiCompatBackend, anythingllm_from_env,
    best_llm_option, choose_llm, detect_hardware, detect_llm_options, propose_models_to_install, provision,
    provision_with_progress,
};
pub use retrieval::{Fused, RRF_K, Ranking, rrf_fuse};
pub use temporal::Validity;

/// Temps Unix courant (secondes, UTC). `0` si l'horloge est antérieure à l'epoch.
#[must_use]
pub(crate) fn now_unix() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}
