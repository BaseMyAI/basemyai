// SPDX-License-Identifier: BUSL-1.1
//! RAG temporel (ADR-005). Chaque mémoire porte une fenêtre de validité ; le
//! recall ne retourne que ce qui est **pertinent ET encore valide**. Le
//! filtrage lui-même vit dans le moteur de stockage
//! ([`crate::storage::NativeMemoryStore`]) — ce module ne porte que le
//! concept `Validity`, indépendant du backend.

/// Fenêtre de validité d'une mémoire. `valid_until = None` => valide jusqu'à
/// invalidation explicite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
