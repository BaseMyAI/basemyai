// SPDX-License-Identifier: BUSL-1.1
//! Cycle de vie du serveur : construction de l'état ([`bootstrap`]),
//! assemblage du routeur ([`router`]), arrêt gracieux ([`shutdown`]),
//! télémétrie ([`telemetry`]).

#[cfg(feature = "embed")]
pub mod bootstrap;
pub mod router;
pub mod shutdown;
pub mod telemetry;

pub use router::build as build_router;
