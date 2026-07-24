// SPDX-License-Identifier: BUSL-1.1
//! Initialisation du subscriber `tracing`. Sans cet appel, tous les
//! `tracing::info!`/`tracing::error!` du crate (télémétrie par requête,
//! erreurs internes) sont des no-op silencieux — c'était le cas avant cette
//! restructuration : aucun subscriber n'était jamais installé.
//!
//! Filtre par défaut : `info` pour ce crate, `warn` ailleurs — surchargeable
//! via `RUST_LOG` (convention `tracing-subscriber` standard).

use tracing_subscriber::EnvFilter;

const DEFAULT_FILTER: &str = "basemyai_rest=info,warn";

/// Installe le subscriber global. Idempotent au sens où un second appel
/// échoue silencieusement (déjà installé) plutôt que de paniquer — utile en
/// tests qui construisent plusieurs fois l'app dans le même process.
pub fn init() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}
