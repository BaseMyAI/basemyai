// SPDX-License-Identifier: BUSL-1.1
//! Retrieval multi-signal par **Reciprocal Rank Fusion (RRF)** (VISION §4.4 /
//! §5.3). Au lieu du cosine pur, un souvenir peut remonter parce qu'il est
//! sémantiquement proche *et/ou* relié à l'entité courante *et/ou* récent
//! *et/ou* important. Chaque signal produit un **classement** ; la RRF les
//! agrège sans avoir à calibrer des poids hétérogènes.
//!
//! Pur mécanisme de classement (aucune I/O) : facile à tester, déterministe.
//! C'est `basemyai` (le *sens*) qui décide quels signaux fournir.

/// Classement ordonné (meilleur d'abord) produit par un signal nommé.
#[derive(Debug, Clone)]
pub struct Ranking {
    /// Nom du signal (`"vector"`, `"graph"`, `"recency"`, `"importance"`…).
    pub signal: String,
    /// Identifiants, du plus pertinent au moins pertinent pour ce signal.
    pub ids: Vec<String>,
}

/// Une entrée du classement fusionné, avec la provenance des signaux qui l'ont
/// fait remonter (traçabilité, VISION §5.4).
#[derive(Debug, Clone, PartialEq)]
pub struct Fused {
    pub id: String,
    pub score: f64,
    /// Signaux ayant contribué, dans l'ordre de leur première occurrence.
    pub contributions: Vec<String>,
}

/// Constante d'amortissement RRF standard (k = 60, Cormack et al.).
pub const RRF_K: f64 = 60.0;

/// Fusionne plusieurs classements par Reciprocal Rank Fusion : le score d'un id
/// est `Σ 1 / (k + rang)` sur tous les signaux où il apparaît (rang 0-indexé).
/// Retourne les ids triés par score décroissant.
#[must_use]
pub fn rrf_fuse(rankings: &[Ranking], k: f64) -> Vec<Fused> {
    use std::collections::HashMap;

    // Accumulateur par id : score cumulé + signaux contributeurs (ordre de
    // première apparition, sans doublon).
    let mut acc: HashMap<&str, (f64, Vec<String>)> = HashMap::new();
    // Préserve l'ordre stable d'insertion des ids (utile pour un tri total
    // reproductible avant le tri final par score).
    let mut ordre: Vec<&str> = Vec::new();

    for ranking in rankings {
        // Un classement aux ids vides est ignoré sans erreur.
        for (rang, id) in ranking.ids.iter().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let contribution = 1.0 / (k + rang as f64);

            let entree = acc.entry(id.as_str()).or_insert_with(|| {
                ordre.push(id.as_str());
                (0.0, Vec::new())
            });
            entree.0 += contribution;
            // N'ajoute le nom du signal qu'à sa première contribution pour cet
            // id (l'ordre de première apparition est donc respecté).
            if !entree.1.contains(&ranking.signal) {
                entree.1.push(ranking.signal.clone());
            }
        }
    }

    let mut fused: Vec<Fused> = ordre
        .into_iter()
        .map(|id| {
            let (score, contributions) = acc.remove(id).unwrap_or((0.0, Vec::new()));
            Fused {
                id: id.to_string(),
                score,
                contributions,
            }
        })
        .collect();

    // Tri par score décroissant ; départage déterministe par id croissant
    // (lexicographique). `total_cmp` garantit un ordre total même si un NaN
    // venait à apparaître — ici impossible, mais le tri reste sûr.
    fused.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.id.cmp(&b.id)));

    fused
}
