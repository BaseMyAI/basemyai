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
mod error;
pub mod maintenance;
mod memory;
pub mod provision;
mod retrieval;
pub mod temporal;

pub use basemyai_core::Metric;
pub use cognition::{
    ConsolidationInput, ConsolidationReport, ExtractedEntity, ExtractedRelation, Extraction, Graph, LlmInference,
    Reached, apply_extraction, consolidate, consolidation_prompt, parse_extraction,
};
pub use error::{MemoryError, Result};
pub use maintenance::{AdaptiveForgetting, ConsolidationTask, ExpiredMemoryGc};
#[cfg(feature = "test-util")]
pub use memory::HashEmbedder;
pub use memory::schema::{BMAI_FORMAT_VERSION, EMBEDDING_DIM, schema};
pub use memory::{AgentId, AgentStats, ImportReport, MAX_TEXT_LEN, Memory, MemoryLayer, Record};
pub use provision::{
    AnythingLlmBackend, BASELINE_DIM, BASELINE_MODEL_ID, BackendKind, HardwareProfile, KNOWN_MODELS, KnownModel,
    LlmOption, LlmProvision, ModelProvision, OllamaBackend, OpenAiCompatBackend, anythingllm_from_env, best_llm_option,
    choose_llm, detect_hardware, detect_llm_options, propose_models_to_install, provision, provision_with_progress,
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
