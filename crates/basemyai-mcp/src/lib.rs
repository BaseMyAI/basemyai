// SPDX-License-Identifier: BUSL-1.1
//! # basemyai-mcp
//!
//! Serveur **MCP (Model Context Protocol)** exposant le moteur de mémoire
//! [`basemyai`] aux agents IA, via deux transports : `stdio` (intégration locale)
//! et `http` (Streamable HTTP, auth Bearer).
//!
//! Outils : `remember`, `recall`, `recall_hybrid`, `recall_graph`, `invalidate`,
//! `stats`, `consolidate` + `consolidate_apply`. La consolidation suit une
//! **politique à niveaux** (ADR-018, supersède ADR-017) : sampling MCP si le
//! client le supporte → LLM local (Ollama/LM Studio/AnythingLLM) → sinon
//! l'extraction est **déléguée à l'agent appelant** (qui a déjà un LLM) puis
//! persistée via `consolidate_apply`. Aucun LLM externe imposé.
//! Multi-agent : une [`basemyai::Memory`] par `agent_id`, isolée au niveau SQL
//! (ADR-006). L'audit ne logue **jamais** de contenu mémoire.
//!
//! Voir `docs/research/mcp-blueprint.md` pour le design détaillé.

mod audit;
mod config;
mod error;
mod provider;
mod sampling;
mod server;
mod tools;

#[cfg(feature = "http")]
mod auth;
#[cfg(any(feature = "stdio", feature = "http"))]
mod transport;

pub use audit::Outcome;
pub use config::Config;
pub use error::{McpError, Result};
pub use provider::MemoryProvider;
pub use sampling::SamplingBackend;
pub use server::McpServer;
pub use tools::{
    ApplyEntity, ApplyRelation, CompileContextParams, CompileContextResult, ConsolidateApplyParams, ConsolidateParams,
    ConsolidateResult, ConsolidateStatus, EntityItem, InvalidateParams, InvalidateResult, RecallGraphParams,
    RecallGraphResult, RecallItem, RecallParams, RecallResult, RememberParams, RememberResult, StatsParams,
    StatsResult, WatchParams, WatchResult,
};

pub use provider::FileProvider;
#[cfg(feature = "test-util")]
pub use provider::InMemoryProvider;

#[cfg(feature = "http")]
pub use transport::run_http;
#[cfg(feature = "stdio")]
pub use transport::run_stdio;
