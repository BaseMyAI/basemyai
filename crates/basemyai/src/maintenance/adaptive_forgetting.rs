// SPDX-License-Identifier: BUSL-1.1
//! Oubli adaptatif (VISION §5.2, ADR-012 §4), porté sur le moteur natif par
//! ADR-037 : plus de `ROW_NUMBER() OVER (PARTITION BY ...)` SQL — un scan
//! applicatif complet de l'agent (`MemoryStore::scan_for_forgetting`), une
//! sélection pure en Rust ([`select_victims`]), puis une éviction ligne par
//! ligne via [`crate::Memory::forget`] (réutilise l'atomicité souvenir+FTS et
//! l'émission d'événement déjà garanties par ce chemin).
//!
//! Score de rétention (inchangé depuis ADR-012) :
//!
//! ```text
//! score = importance + H / (H + max(0, now - last_access))
//! ```
//!
//! `H` = [`AdaptiveForgettingPolicy::recency_half_life_secs`]. Decay
//! **hyperbolique**, pas exponentielle : `0.5^(age/H)` sous-déborde à `0.0`
//! en flottant dès que `age` atteint quelques centaines de demi-vies avec des
//! timestamps Unix réels, rendant tous les souvenirs anciens indiscernables.
//! `H / (H + age)` reste dans `(0, 1]`, strictement décroissante en `age`,
//! distinguable à toute échelle réelle.

use std::sync::Arc;

use basemyai_core::{MaintenanceTask, Result as CoreResult};

use crate::Memory;
use crate::storage::ForgetCandidate;

/// Politique d'oubli adaptatif, enregistrée dans le `MaintenanceWorker`
/// (une instance par agent — la tâche est auto-suffisante, ADR-032/033).
#[derive(Debug, Clone, Copy)]
pub struct AdaptiveForgettingPolicy {
    /// Nombre maximum de souvenirs conservés pour cet agent ; les moins bien
    /// notés au-delà sont physiquement évincés.
    pub capacity: usize,
    /// Demi-vie de la récence en secondes (`H` dans la formule de score).
    pub recency_half_life_secs: i64,
}

/// Rapport d'une passe d'oubli adaptatif.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ForgettingReport {
    /// Nombre de souvenirs scannés pour cet agent.
    pub scanned: usize,
    /// Nombre de souvenirs physiquement évincés.
    pub evicted: usize,
}

/// Score de rétention d'un souvenir à l'instant `now` (ADR-012 §4, formule
/// inchangée). `half_life_secs <= 0` est traité comme `1` (une demi-vie nulle
/// ou négative n'a pas de sens physique ; éviter une division par zéro plutôt
/// que paniquer).
fn retention_score(importance: f64, half_life_secs: i64, last_access: i64, now: i64) -> f64 {
    let half_life = half_life_secs.max(1) as f64;
    #[allow(clippy::cast_precision_loss)]
    let age = now.saturating_sub(last_access).max(0) as f64;
    importance + half_life / (half_life + age)
}

/// Sélectionne les ids à évincer : trie `candidates` par score de rétention
/// décroissant (id croissant départage les ex æquo — même règle qu'ADR-012),
/// renvoie tout ce qui dépasse `policy.capacity`.
///
/// Fonction **pure** (pas d'I/O, pas d'horloge lue en interne — `now` est un
/// paramètre) : testable exhaustivement sans moteur ouvert ni horloge réelle.
/// TODO: benchmarker la complexité de tri `O(n log n)` vs. un algorithme de sélection `O(n)` (QuickSelect) pour des centaines de milliers de souvenirs.
#[must_use]
pub(crate) fn select_victims(
    candidates: &[ForgetCandidate],
    now: i64,
    policy: AdaptiveForgettingPolicy,
) -> Vec<String> {
    if candidates.len() <= policy.capacity {
        return Vec::new();
    }
    let mut ranked: Vec<&ForgetCandidate> = candidates.iter().collect();
    ranked.sort_by(|a, b| {
        let score_a = retention_score(a.importance, policy.recency_half_life_secs, a.last_access, now);
        let score_b = retention_score(b.importance, policy.recency_half_life_secs, b.last_access, now);
        score_b
            .partial_cmp(&score_a)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.cmp(&b.id))
    });
    ranked.into_iter().skip(policy.capacity).map(|c| c.id.clone()).collect()
}

/// Tâche de fond d'oubli adaptatif, injectable dans le `MaintenanceWorker`
/// agnostique du core. Auto-suffisante : possède sa propre [`Memory`] et sa
/// politique (même pattern que `ConsolidationTask`).
pub struct AdaptiveForgettingTask {
    memory: Arc<Memory>,
    policy: AdaptiveForgettingPolicy,
}

impl AdaptiveForgettingTask {
    /// Construit la tâche à partir d'une mémoire partagée et d'une politique.
    #[must_use]
    pub fn new(memory: Arc<Memory>, policy: AdaptiveForgettingPolicy) -> Self {
        Self { memory, policy }
    }
}

#[async_trait::async_trait]
impl MaintenanceTask for AdaptiveForgettingTask {
    fn name(&self) -> &str {
        "adaptive-forgetting"
    }

    /// Lance une passe d'oubli adaptatif. Mappe [`crate::MemoryError`] vers
    /// [`basemyai_core::CoreError::Storage`] pour satisfaire l'interface du
    /// core.
    async fn run(&self) -> CoreResult<()> {
        self.memory
            .adaptive_forget(self.policy)
            .await
            .map(|_| ())
            .map_err(|e| basemyai_core::CoreError::Storage(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(id: &str, importance: f64, last_access: i64) -> ForgetCandidate {
        ForgetCandidate {
            id: id.to_string(),
            importance,
            last_access,
        }
    }

    #[test]
    fn under_capacity_evicts_nothing() {
        let candidates = vec![candidate("a", 1.0, 0), candidate("b", 1.0, 0)];
        let policy = AdaptiveForgettingPolicy {
            capacity: 5,
            recency_half_life_secs: 3600,
        };
        assert!(select_victims(&candidates, 100, policy).is_empty());
    }

    #[test]
    fn at_capacity_evicts_nothing() {
        let candidates = vec![candidate("a", 1.0, 0), candidate("b", 1.0, 0)];
        let policy = AdaptiveForgettingPolicy {
            capacity: 2,
            recency_half_life_secs: 3600,
        };
        assert!(select_victims(&candidates, 100, policy).is_empty());
    }

    #[test]
    fn evicts_least_important_beyond_capacity() {
        // Même récence (last_access identique) : seule l'importance départage.
        let candidates = vec![
            candidate("m1", 0.1, 1_000),
            candidate("m2", 0.9, 1_000),
            candidate("m3", 0.5, 1_000),
            candidate("m4", 0.7, 1_000),
            candidate("m5", 0.3, 1_000),
        ];
        let policy = AdaptiveForgettingPolicy {
            capacity: 3,
            recency_half_life_secs: 86_400,
        };
        let mut evicted = select_victims(&candidates, 1_000, policy);
        evicted.sort();
        // Les 3 plus importants (m2, m4, m3) survivent ; m1 et m5 partent.
        assert_eq!(evicted, vec!["m1".to_string(), "m5".to_string()]);
    }

    #[test]
    fn recency_breaks_ties_at_equal_importance() {
        let old = candidate("old", 0.5, 0);
        let recent = candidate("recent", 0.5, 10_000);
        let policy = AdaptiveForgettingPolicy {
            capacity: 1,
            recency_half_life_secs: 3_600,
        };
        // now proche de `recent` : "recent" a une récence ~1, "old" ~0.
        let evicted = select_victims(&[old, recent], 10_000, policy);
        assert_eq!(
            evicted,
            vec!["old".to_string()],
            "à importance égale, le souvenir au last_access le plus ancien doit être évincé"
        );
    }

    #[test]
    fn ties_break_by_ascending_id() {
        // Importance et last_access identiques : score identique ; l'id
        // croissant départage (même règle qu'ADR-012).
        let candidates = vec![candidate("z", 1.0, 0), candidate("a", 1.0, 0), candidate("m", 1.0, 0)];
        let policy = AdaptiveForgettingPolicy {
            capacity: 1,
            recency_half_life_secs: 3_600,
        };
        // "a" gagne le départage (id le plus petit), "m" et "z" sont évincés.
        let mut evicted = select_victims(&candidates, 0, policy);
        evicted.sort();
        assert_eq!(evicted, vec!["m".to_string(), "z".to_string()]);
    }

    #[test]
    fn zero_capacity_evicts_everything() {
        let candidates = vec![candidate("a", 1.0, 0)];
        let policy = AdaptiveForgettingPolicy {
            capacity: 0,
            recency_half_life_secs: 3_600,
        };
        assert_eq!(select_victims(&candidates, 0, policy), vec!["a".to_string()]);
    }

    #[test]
    fn non_positive_half_life_does_not_panic() {
        let candidates = vec![candidate("a", 1.0, 0), candidate("b", 1.0, 0), candidate("c", 1.0, 0)];
        let policy = AdaptiveForgettingPolicy {
            capacity: 1,
            recency_half_life_secs: 0,
        };
        // Ne doit pas paniquer (division par zéro) ; le résultat exact
        // importe peu ici, seule l'absence de panique est vérifiée.
        let evicted = select_victims(&candidates, 100, policy);
        assert_eq!(evicted.len(), 2);
    }
}
