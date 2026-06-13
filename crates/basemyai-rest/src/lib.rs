//! # basemyai-rest
//!
//! Sidecar **HTTP/JSON** exposant le moteur de mémoire [`basemyai`] aux langages
//! sans binding Rust natif (Python, Go, Ruby, PHP, …). 100 % local, auth Bearer.
//!
//! Conforme à `analayse/openapi-sidecar.yaml`. Routes sous `/v1` ; `/health`
//! sans auth. Voir [`build_app`] pour monter le routeur autour d'un [`AppState`].

mod config;
mod error;
mod provider;
mod routes;
mod state;

pub use config::Config;
pub use error::RestError;
pub use provider::MemoryProvider;
pub use routes::build_app;
pub use state::AppState;

#[cfg(feature = "crypto")]
pub use provider::EncryptedFileProvider;
#[cfg(feature = "test-util")]
pub use provider::InMemoryProvider;
