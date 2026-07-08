// SPDX-License-Identifier: BUSL-1.1
//! # basemyai (bindings Node.js)
//!
//! Liaison NAPI-RS du moteur de mémoire [`basemyai`]. API asynchrone : chaque
//! opération rend une `Promise`, exécutée sur le runtime tokio interne de NAPI.
//!
//! Les symboles `#[napi]` sont auto-enregistrés ; `index.js`/`index.d.ts` sont
//! générés par `@napi-rs/cli` (`napi build`).

mod errors;
mod memory;
mod types;

pub use memory::Memory;
pub use types::{AgentStats, Entity, MemoryOpenOptions, Record};
