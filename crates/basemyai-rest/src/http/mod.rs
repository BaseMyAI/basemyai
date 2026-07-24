// SPDX-License-Identifier: BUSL-1.1
//! Couche HTTP transverse : modèle d'erreur, validation, middlewares.
//! Rien ici ne connaît `basemyai` au-delà du mapping d'erreurs
//! ([`error::RestError::Memory`]) — c'est la seule couture avec le domaine.

pub mod error;
pub mod extract;
pub mod middleware;
pub mod pagination;
pub mod response;

pub use error::RestError;
