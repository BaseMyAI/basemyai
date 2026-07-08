// SPDX-License-Identifier: BUSL-1.1
//! Metric de distance pour le KNN — le seul type de `storage::vector` qui
//! survit à la bascule 100% natif (ADR-032) : `Filter`/`Value`/`Neighbor`
//! étaient des types de construction de requête libSQL, supprimés avec
//! `Store`. `Metric` reste un concept de domaine, indépendant du backend.

/// Métrique de distance pour le KNN.
///
/// L'index natif (`basemyai-engine`, LM-DiskANN) est **cosinus**.
/// [`Metric::Euclidean`]/[`Metric::Hamming`] n'ont pas d'implémentation de
/// re-classement côté natif aujourd'hui (l'ancien chemin de re-classement
/// vivait dans le `Store` libSQL supprimé) — un appelant qui les demande
/// reçoit une erreur franche du backend, jamais un résultat silencieusement
/// faux.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Metric {
    /// Distance cosinus (native). Défaut.
    #[default]
    Cosine,
    /// Distance euclidienne (L2) sur les vecteurs.
    Euclidean,
    /// Distance de Hamming par signe : nombre de dimensions où le signe diffère
    /// (quantification binaire 1 bit/dimension).
    Hamming,
}
