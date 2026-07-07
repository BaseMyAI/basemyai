// SPDX-License-Identifier: BUSL-1.1
//! RAG temporel (ADR-005). Chaque mémoire porte une fenêtre de validité ; le
//! recall ne retourne que ce qui est **pertinent ET encore valide**.
//!
//! Le filtre temporel s'exprime via le [`basemyai_core::Filter`]
//! paramétré du core — le core ne sait pas que le filtre concerne le temps.

use basemyai_core::{Filter, Value};

/// Fenêtre de validité d'une mémoire. `valid_until = None` => valide jusqu'à
/// invalidation explicite.
#[derive(Debug, Clone, Copy)]
pub struct Validity {
    /// Timestamp Unix (UTC) de début de validité.
    pub valid_from: i64,
    /// Timestamp Unix (UTC) de fin, exclusif. `None` = sans expiration.
    pub valid_until: Option<i64>,
}

impl Validity {
    /// Valide à partir de `from`, sans expiration.
    #[must_use]
    pub fn since(from: i64) -> Self {
        Self {
            valid_from: from,
            valid_until: None,
        }
    }

    /// `true` si la validité couvre l'instant `now` (Unix UTC).
    #[must_use]
    pub fn is_valid_at(&self, now: i64) -> bool {
        self.valid_from <= now && self.valid_until.is_none_or(|until| now < until)
    }
}

/// Construit le fragment de filtre temporel paramétré passé au KNN du core :
/// `valid_from <= ? AND (valid_until IS NULL OR valid_until > ?)`.
#[must_use]
pub fn temporal_filter(now: i64) -> Filter {
    Filter::new(
        "valid_from <= ? AND (valid_until IS NULL OR valid_until > ?)",
        vec![Value::Integer(now), Value::Integer(now)],
    )
}
