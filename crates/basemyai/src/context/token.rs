// SPDX-License-Identifier: BUSL-1.1
//! Estimation locale et injectable du cout en tokens.

/// Estimateur du cout d'un texte pour la fenetre de contexte d'un modele.
///
/// Le resultat est une estimation, pas un comptage garanti par un tokenizer de
/// fournisseur. Un consommateur peut injecter son propre estimateur.
pub trait TokenEstimator: Send + Sync {
    /// Estime le nombre de tokens necessaires pour `text`.
    fn estimate(&self, text: &str) -> usize;
}

/// Estimateur local approximatif base sur la taille UTF-8.
#[derive(Debug, Clone, Copy, Default)]
pub struct ApproximateTokenEstimator;

impl TokenEstimator for ApproximateTokenEstimator {
    fn estimate(&self, text: &str) -> usize {
        if text.is_empty() {
            0
        } else {
            text.len().div_ceil(3).max(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approximate_estimator_handles_empty_and_multibyte_text() {
        let estimator = ApproximateTokenEstimator;
        assert_eq!(estimator.estimate(""), 0);
        assert!(estimator.estimate("memoire locale") > 0);
        assert!(estimator.estimate("memoire locale") >= estimator.estimate("memoire"));
    }
}
