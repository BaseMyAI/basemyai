// SPDX-License-Identifier: BUSL-1.1
//! Endpoints organisés en tranches verticales par domaine métier. Chaque
//! domaine expose un unique `router() -> Router<AppState>`, assemblé par
//! `server::router`.

pub mod agents;
pub mod context;
pub mod events;
pub mod graph;
pub mod health;
pub mod maintenance;
pub mod memories;
