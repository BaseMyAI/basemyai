// SPDX-License-Identifier: BUSL-1.1
//! Contexte applicatif : état partagé ([`AppState`]), résolution de mémoire
//! ([`MemoryRegistry`]) et contexte de requête ([`RequestContext`]).

mod app_state;
mod memory_registry;
mod request_context;

pub use app_state::AppState;
pub use memory_registry::MemoryRegistry;
pub use request_context::RequestContext;
