//! # basemyai-core
//!
//! Socle embarqué **agnostique métier** de l'écosystème BaseMyAI.
//!
//! Il fournit des *primitives* — store libSQL async, recherche vectorielle
//! **native** (libSQL), embeddings in-process (Candle), chiffrement (libSQL),
//! boucle de maintenance — et **jamais** de concept métier.
//!
//! Règle d'agnosticité (cf. `ADR.md` ADR-001) : ce crate ne connaît ni
//! `agent_id`, ni `valid_from`/`valid_until`, ni les couches mémoire, ni les
//! `Symbol`/`Edge` de ForgeMyAI. Il expose un *mécanisme* ; le consommateur
//! fournit le *sens* (filtre SQL paramétré, tâches de maintenance injectées).
//!
//! Consommateurs : `basemyai` (sémantique mémoire) et ForgeMyAI (crate Rust natif).

// Scaffold : les corps réels arrivent à la phase d'implémentation.
#![allow(dead_code)]

mod embed;
mod error;
mod maintenance;
mod store;
mod vector;

pub use embed::{Device, Embedder};
/// Embedder Candle réel (BERT/`all-MiniLM-L6-v2`) — gated par la feature `embed`.
#[cfg(feature = "embed")]
pub use embed::CandleEmbedder;
pub use error::{CoreError, Result};
pub use maintenance::{MaintenanceTask, MaintenanceWorker};
pub use store::{EncryptionKey, Migration, Store};
pub use vector::{Filter, Neighbor, Value};

/// Ré-export : les consommateurs déclarent leur schéma et leurs requêtes via
/// l'API libSQL exposée par [`Store::connect`].
pub use libsql;
