//! Tests contractuels : forme JSON stable, `Content-Type`, codes HTTP, codes
//! d'erreur, compatibilité des routes existantes. Pas de scénario métier
//! multi-étapes ici — voir `tests/integration.rs` pour ça.
#![cfg(feature = "test-util")]

#[path = "support/mod.rs"]
mod support;

#[path = "contract/errors.rs"]
mod errors;
#[path = "contract/graph.rs"]
mod graph;
#[path = "contract/health.rs"]
mod health;
#[path = "contract/memories.rs"]
mod memories;
