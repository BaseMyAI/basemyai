// SPDX-License-Identifier: BUSL-1.1
//! # basemyai-rest
//!
//! Sidecar **HTTP/JSON** exposant le moteur de mémoire [`basemyai`] aux langages
//! sans binding Rust natif (Python, Go, Ruby, PHP, …). 100 % local, auth Bearer.
//!
//! Architecture en tranches verticales : `endpoints/<domaine>/<action>.rs` —
//! voir `README.md` pour l'arborescence complète. Conforme à
//! `crates/basemyai-rest/openapi.yaml`. Routes sous `/v1` ; `/health/*` sans
//! auth. Voir [`server::build_router`] pour monter le routeur autour d'un
//! [`context::AppState`], ou [`server::bootstrap::build_state`] pour la
//! construction de production complète.

pub mod config;
pub mod context;
pub mod endpoints;
pub mod http;
pub mod provider;
pub mod server;

pub use config::{AgentPolicy, ApiKey, RuntimeConfig, StartupConfig};
pub use context::AppState;
pub use http::RestError;
#[cfg(feature = "test-util")]
pub use provider::InMemoryProvider;
pub use provider::{FileProvider, MemoryProvider};
pub use server::build_router as build_app;
