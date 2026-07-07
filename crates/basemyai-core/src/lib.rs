// SPDX-License-Identifier: BUSL-1.1
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
mod storage;

/// Embedder Candle réel (BERT/`all-MiniLM-L6-v2`) — gated par la feature `embed`.
#[cfg(feature = "embed")]
pub use embed::CandleEmbedder;
pub use embed::{Device, Embedder};
pub use error::{CoreError, Result};
pub use maintenance::{MaintenanceTask, MaintenanceWorker};
/// Wrapper capability-only pour le backend natif `basemyai-engine`
/// (ADR-024/ADR-025) — gated par la feature `engine-native`. Ne fournit pas
/// `MemoryStore` : voir `docs/TODO-NATIVE-ENGINE.md` N2.
#[cfg(feature = "engine-native")]
pub use storage::NativeEngine;
pub use storage::{EncryptionKey, EngineCapabilities, EngineKind, Migration, StorageEngine, Store, WriteTxn};
pub use storage::{Filter, Metric, Neighbor, Value};

/// Ré-export : les consommateurs déclarent leur schéma et leurs requêtes via
/// l'API libSQL exposée par [`Store::connect`].
pub use libsql;
