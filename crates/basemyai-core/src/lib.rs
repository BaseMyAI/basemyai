// SPDX-License-Identifier: BUSL-1.1
//! # basemyai-core
//!
//! Socle embarqué **agnostique métier** de l'écosystème BaseMyAI.
//!
//! Il fournit des *primitives* — moteur de stockage natif (ADR-024/025,
//! `basemyai-engine`), embeddings in-process (Candle), chiffrement au repos
//! (ADR-030), boucle de maintenance — et **jamais** de concept métier. libSQL
//! (ADR-011) a été le backend V1 ; **retiré du workspace par ADR-033** :
//! le moteur natif est l'unique backend.
//!
//! Règle d'agnosticité (cf. `ADR.md` ADR-001) : ce crate ne connaît ni
//! `agent_id`, ni `valid_from`/`valid_until`, ni les couches mémoire, ni les
//! `Symbol`/`Edge` d'un consommateur code. Il expose un *mécanisme* ; le
//! consommateur fournit le *sens* (tâches de maintenance injectées, sémantique
//! côté `basemyai::storage::NativeMemoryStore`).
//!
//! Consommateur principal : `basemyai` (sémantique mémoire). Le core est aussi
//! importable par des crates Rust tiers qui apportent leur propre sémantique.

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
/// (ADR-024/ADR-025/ADR-033), unique backend du workspace. Ne fournit pas
/// `MemoryStore` : voir
/// `basemyai::storage::NativeMemoryStore` pour l'implémentation sémantique.
pub use storage::NativeEngine;
pub use storage::{
    DOCKER_SECRET_PATH, EncryptionKey, EngineCapabilities, EngineKind, KeyResolveError, KeySource, Metric, ResolvedKey,
    StorageEngine, key_source_label,
};
