// SPDX-License-Identifier: BUSL-1.1
//! Les 4 couches mémoire (ADR-004) et les types de données associés.

use super::trust::TrustLevel;
use crate::temporal::Validity;
use crate::{MemoryError, Result};

/// Les 4 couches mémoire (ADR-004). Chacune a son mode d'accès et sa durée de vie.
///
/// `#[non_exhaustive]` : une couche supplémentaire peut être ajoutée en minor.
/// Les `match` externes doivent inclure un bras `_ =>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum MemoryLayer {
    /// Contexte de travail de la session (TTL court).
    ShortTerm,
    /// Ce qui s'est passé et quand.
    Episodic,
    /// Procédures/compétences apprises.
    Procedural,
    /// Faits recherchables vectoriellement.
    Semantic,
}

impl MemoryLayer {
    /// Nom de couche stocké dans la colonne `layer`.
    #[must_use]
    pub fn table(self) -> &'static str {
        match self {
            Self::ShortTerm => "short_term",
            Self::Episodic => "episodic",
            Self::Procedural => "procedural",
            Self::Semantic => "semantic",
        }
    }

    /// Reconstruit une couche depuis son nom stocké.
    ///
    /// # Errors
    /// [`MemoryError::UnknownLayer`] si le nom ne correspond à aucune couche connue.
    pub fn from_table(name: &str) -> Result<Self> {
        match name {
            "short_term" => Ok(Self::ShortTerm),
            "episodic" => Ok(Self::Episodic),
            "procedural" => Ok(Self::Procedural),
            "semantic" => Ok(Self::Semantic),
            other => Err(MemoryError::UnknownLayer(other.to_string())),
        }
    }
}

/// Une mémoire retournée par `recall`.
///
/// `score` est la **distance cosinus** brute renvoyée par l'index (`0` = identique,
/// croissante = moins pertinent). Les surfaces publiques (SDK, REST) exposent en
/// général la **similarité** via [`Record::similarity`].
#[derive(Debug, Clone)]
pub struct Record {
    pub id: String,
    pub text: String,
    pub layer: MemoryLayer,
    pub score: f32,
    /// Provenance wire du souvenir (`user`, `consolidation`, `import`, …).
    pub source: String,
    /// Fenetre temporelle persistante du souvenir.
    pub validity: Validity,
}

impl Record {
    /// Provenance typée dérivée de [`Self::source`] (ADR-036).
    #[must_use]
    pub fn trust(&self) -> TrustLevel {
        TrustLevel::from_source(&self.source)
    }

    /// Similarité cosinus normalisée dans `[0, 1]` (`1` = identique), dérivée de
    /// la distance brute. C'est la forme exposée par les SDK et le sidecar REST.
    #[must_use]
    pub fn similarity(&self) -> f32 {
        (1.0 - self.score).clamp(0.0, 1.0)
    }
}

/// Statistiques de la mémoire d'un agent (souvenirs valides à l'instant courant).
#[derive(Debug, Clone, Default)]
pub struct AgentStats {
    pub short_term: usize,
    pub episodic: usize,
    pub procedural: usize,
    pub semantic: usize,
}

impl AgentStats {
    /// Nombre total de souvenirs valides.
    #[must_use]
    pub fn total(&self) -> usize {
        self.short_term + self.episodic + self.procedural + self.semantic
    }
}
