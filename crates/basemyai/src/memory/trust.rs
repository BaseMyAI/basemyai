// SPDX-License-Identifier: BUSL-1.1
//! Provenance typée des souvenirs (audit memory poisoning, ADR-036).
//!
//! `TrustLevel` décrit **d'où vient** un enregistrement, pas s'il est sûr :
//! un contenu `User` peut être hostile ; l'intégrateur filtre avant de faire
//! confiance au texte rappelé.

/// Tag wire `source` pour un souvenir mémorisé directement par l'agent.
pub const SOURCE_USER: &str = "user";
/// Tag wire pour un fait promu par consolidation (pipeline LLM, ADR-018).
pub const SOURCE_CONSOLIDATION: &str = "consolidation";
/// Tag wire pour un souvenir réimporté depuis un export JSONL (ADR-036).
pub const SOURCE_IMPORT: &str = "import";

/// Provenance d'un souvenir — enum stable pour filtrage et affichage.
///
/// Les valeurs inconnues du wire (`source` libre en base) mappent vers
/// [`TrustLevel::Unknown`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TrustLevel {
    /// `remember` / surfaces d'écriture directes.
    User,
    /// Faits promus par `consolidate` / `consolidate_apply`.
    Consolidation,
    /// Import JSONL (`import_jsonl*`).
    Import,
    /// Tag `source` non reconnu (forward-compat, migrations tierces).
    Unknown,
}

impl TrustLevel {
    /// Interprète le champ wire `source` persisté en base.
    #[must_use]
    pub fn from_source(source: &str) -> Self {
        match source {
            SOURCE_USER => Self::User,
            SOURCE_CONSOLIDATION => Self::Consolidation,
            SOURCE_IMPORT => Self::Import,
            _ => Self::Unknown,
        }
    }

    /// Tag wire canonique pour persistance / export.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => SOURCE_USER,
            Self::Consolidation => SOURCE_CONSOLIDATION,
            Self::Import => SOURCE_IMPORT,
            Self::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for TrustLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_known_sources() {
        for level in [TrustLevel::User, TrustLevel::Consolidation, TrustLevel::Import] {
            assert_eq!(TrustLevel::from_source(level.as_str()), level);
        }
    }

    #[test]
    fn unknown_source_maps_to_unknown() {
        assert_eq!(TrustLevel::from_source("spoofed-admin"), TrustLevel::Unknown);
    }
}
