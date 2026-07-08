// SPDX-License-Identifier: BUSL-1.1
//! # basemyai-rest
//!
//! Sidecar **HTTP/JSON** exposant le moteur de mémoire [`basemyai`] aux langages
//! sans binding Rust natif (Python, Go, Ruby, PHP, …). 100 % local, auth Bearer.
//!
//! Conforme à `crates/basemyai-rest/openapi.yaml`. Routes sous `/v1` ; `/health`
//! sans auth. Voir [`build_app`] pour monter le routeur autour d'un [`AppState`].

mod config;
mod error;
mod provider;
mod routes;
mod state;

pub use config::{AgentPolicy, Config};
pub use error::RestError;
pub use provider::MemoryProvider;
pub use routes::build_app;
pub use state::AppState;

pub use provider::FileProvider;
#[cfg(feature = "test-util")]
pub use provider::InMemoryProvider;
