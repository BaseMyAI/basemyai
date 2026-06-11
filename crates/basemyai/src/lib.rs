//! # basemyai
//!
//! Moteur de **mémoire pour agents IA**, posé sur [`basemyai_core`].
//!
//! Ajoute au socle agnostique la *sémantique* mémoire : les 4 couches
//! ([`MemoryLayer`]), le RAG temporel ([`temporal`]), l'isolation par
//! [`AgentId`], le chiffrement obligatoire, et le setup hardware-aware
//! ([`setup`]).
//!
//! Ces concepts vivent **ici**, jamais dans `basemyai-core` (ADR-001).

// Scaffold : les corps réels arrivent à la phase d'implémentation.
#![allow(dead_code)]

mod consolidation;
mod error;
mod forgetting;
mod graph;
mod inference;
mod isolation;
pub mod llm_provision;
pub mod maintenance;
mod memory;
mod retrieval;
mod schema;
pub mod setup;
pub mod temporal;

pub use consolidation::{ConsolidationReport, consolidate};
pub use error::{MemoryError, Result};
pub use forgetting::AdaptiveForgetting;
pub use graph::{Graph, Reached};
pub use inference::LlmInference;
pub use isolation::AgentId;
pub use llm_provision::{KnownModel, LlmOption, LlmProvision, OllamaBackend, KNOWN_MODELS, best_llm_option, choose_llm, detect_llm_options, propose_models_to_install};
pub use maintenance::ExpiredMemoryGc;
pub use memory::{Memory, MemoryLayer, Record};
pub use retrieval::{Fused, Ranking, RRF_K, rrf_fuse};
pub use schema::{EMBEDDING_DIM, schema};

/// Temps Unix courant (secondes, UTC). `0` si l'horloge est antérieure à l'epoch.
#[must_use]
pub(crate) fn now_unix() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}
