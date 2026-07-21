//! Helpers partagés par `tests/contract.rs`, `tests/integration.rs` et
//! `tests/security.rs`. Ce module n'est jamais compilé comme cible de test à
//! part entière (convention `mod.rs`) — chacun des trois l'inclut via
//! `#[path = "support/mod.rs"] mod support;`.
//!
//! Chaque binaire de test n'utilise qu'un sous-ensemble de ces helpers (ex.
//! `overlong_text` ne sert qu'à `security`) : le reste est légitimement
//! "unused" du point de vue d'un binaire donné, d'où l'`allow` global plutôt
//! que du bruit `#[cfg(test)]` par fonction.
#![allow(dead_code)]

pub(crate) mod app;
pub(crate) mod client;
pub(crate) mod fixtures;
