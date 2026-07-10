// SPDX-License-Identifier: BUSL-1.1
//! Oubli adaptatif (VISION §5.2, ADR-012 §4), porté sur le moteur natif par
//! ADR-037 : plus de `ROW_NUMBER() OVER (PARTITION BY ...)` SQL — un scan
//! applicatif complet de l'agent (`MemoryStore::scan_for_forgetting`), une
//! sélection pure en Rust ([`select_victims`]), puis une éviction ligne par
//! ligne (une transaction moteur par victime, jamais un `DELETE` de masse —
//! voir [`scan_and_select`] pour pourquoi les deux points d'entrée ci-dessous
//! partagent cette étape mais divergent sur l'éviction elle-même).
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
//!
//! Deux points d'entrée, une seule sélection ([`scan_and_select`]) :
//! - [`crate::Memory::adaptive_forget`] évince via [`crate::Memory::forget`]
//!   (émission d'événement `Forgotten`, cf. `MemorySubscription`/ADR-022) —
//!   le chemin programmatique/`MaintenanceTask` ([`AdaptiveForgettingTask`]).
//! - [`run`] évince directement via [`crate::storage::MemoryStore::forget`],
//!   sans passer par un [`crate::Memory`] complet (donc sans charger
//!   l'embedder Candle) — le chemin CLI, qui n'a besoin d'aucun embedding
//!   pour une opération purement temporelle/de capacité, et supporte le
//!   dry-run (aucune éviction, juste le rapport).

use std::sync::Arc;

use basemyai_core::{MaintenanceTask, Result as CoreResult};

use crate::storage::{ForgetCandidate, MemoryStore};
use crate::{AgentId, Memory, Result, now_unix};

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
/// que paniquer). `last_access` dans le futur (horloge système en recul,
/// import adversarial) sature l'âge à `0` plutôt que de produire un âge
/// négatif — un souvenir "d'avenir" est traité comme parfaitement récent,
/// jamais comme un signal qui gonflerait artificiellement son score au-delà
/// de `importance + 1`. `importance` non finie (`NaN`/`±inf` — non atteignable
/// via l'API publique aujourd'hui, mais un import ADR-036 rejoue des valeurs
/// arbitraires depuis un fichier JSONL non fiable) est ramenée à `0.0` :
/// laisser passer un `NaN` romprait l'ordre total exigé par [`select_victims`]
/// (`NaN.partial_cmp` renvoie toujours `None`), ce qui rendrait la sélection
/// non déterministe pour *tous* les candidats comparés au souvenir corrompu,
/// pas seulement pour lui.
fn retention_score(importance: f64, half_life_secs: i64, last_access: i64, now: i64) -> f64 {
    let half_life = half_life_secs.max(1) as f64;
    #[allow(clippy::cast_precision_loss)]
    let age = now.saturating_sub(last_access).max(0) as f64;
    let importance = if importance.is_finite() { importance } else { 0.0 };
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

/// Scanne et sélectionne les victimes d'une passe d'oubli adaptatif, sans les
/// évincer — l'étape partagée par [`crate::Memory::adaptive_forget`] (éviction
/// via `Memory::forget`, événementielle) et [`run`] (éviction directe sur le
/// store, sans `Memory`). `now` est calculé une seule fois ici et réutilisé
/// pour le scan **et** le score : un scan et un score évalués à des instants
/// différents pourraient exclure/inclure un souvenir de façon incohérente
/// entre les deux passes.
///
/// Renvoie `(scanned, victim_ids)` — `scanned` est la population **active**
/// vue par le scan (ADR-038 : les invalidés/expirés en sont déjà exclus par
/// [`crate::storage::MemoryStore::scan_for_forgetting`]), jamais le total
/// brut de la table.
///
/// # Errors
/// Propage les erreurs de stockage du scan.
pub(crate) async fn scan_and_select(
    store: &Arc<dyn MemoryStore>,
    agent: &AgentId,
    policy: AdaptiveForgettingPolicy,
) -> Result<(usize, Vec<String>)> {
    let now = now_unix();
    let candidates = store.scan_for_forgetting(agent, now).await?;
    let scanned = candidates.len();
    let victims = select_victims(&candidates, now, policy);
    Ok((scanned, victims))
}

/// Passe d'oubli adaptatif **sans `Memory`** : opère directement sur
/// [`MemoryStore`], donc sans charger l'embedder Candle — le chemin CLI
/// (`basemyai forget-adaptive`), qui n'a besoin d'aucun embedding pour une
/// opération purement temporelle/de capacité (miroir de la façon dont
/// `list`/`forget`/`invalidate`/`purge` évitent déjà `open_memory`).
///
/// `dry_run = true` calcule et renvoie le rapport (ce qui **serait** évincé)
/// sans évincer quoi que ce soit — aucune mutation, aucun appel à
/// [`MemoryStore::forget`].
///
/// N'émet aucun [`crate::MemoryEvent`] (contrairement à
/// [`crate::Memory::adaptive_forget`]) : un processus CLI one-shot n'a pas
/// d'abonné à qui les envoyer.
///
/// # Errors
/// Propage les erreurs de stockage (scan ou éviction).
pub async fn run(
    store: &Arc<dyn MemoryStore>,
    agent: &AgentId,
    policy: AdaptiveForgettingPolicy,
    dry_run: bool,
) -> Result<ForgettingReport> {
    let (scanned, victims) = scan_and_select(store, agent, policy).await?;
    let evicted = victims.len();
    if !dry_run {
        for id in &victims {
            store.forget(agent, id).await?;
        }
    }
    Ok(ForgettingReport { scanned, evicted })
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

    #[test]
    fn no_candidates_evicts_nothing() {
        let policy = AdaptiveForgettingPolicy {
            capacity: 0,
            recency_half_life_secs: 3_600,
        };
        assert!(select_victims(&[], 0, policy).is_empty());
    }

    #[test]
    fn future_last_access_does_not_panic_or_produce_negative_age() {
        // `last_access` postérieur à `now` (horloge en recul, import
        // adversarial) : l'âge doit saturer à 0, jamais devenir négatif
        // (ce qui gonflerait le score au-delà de la plage attendue).
        let future = candidate("future", 0.5, 1_000_000);
        let present = candidate("present", 0.5, 0);
        let policy = AdaptiveForgettingPolicy {
            capacity: 1,
            recency_half_life_secs: 3_600,
        };
        // now = 0, très antérieur à `future.last_access` : l'âge de
        // `future` sature à 0 (score maximal), donc "present" est évincé.
        let evicted = select_victims(&[future, present], 0, policy);
        assert_eq!(evicted, vec!["present".to_string()]);
    }

    #[test]
    fn non_finite_importance_does_not_break_total_order() {
        // NaN/±inf ne sont pas atteignables via l'API publique aujourd'hui
        // (`importance` par défaut = 1.0, ADR-037), mais un import (ADR-036)
        // rejoue des valeurs arbitraires depuis un JSONL non fiable : la
        // sélection doit rester déterministe et ne jamais paniquer.
        let candidates = vec![
            candidate("nan", f64::NAN, 0),
            candidate("pos_inf", f64::INFINITY, 0),
            candidate("neg_inf", f64::NEG_INFINITY, 0),
            candidate("normal", 1.0, 0),
        ];
        let policy = AdaptiveForgettingPolicy {
            capacity: 1,
            recency_half_life_secs: 3_600,
        };
        // Deux appels doivent produire le même résultat (déterminisme) —
        // un NaN qui romprait l'ordre total ferait varier le résultat d'un
        // tri à l'autre.
        let mut first = select_victims(&candidates, 100, policy);
        let mut second = select_victims(&candidates, 100, policy);
        first.sort();
        second.sort();
        assert_eq!(first, second);
        assert_eq!(first.len(), 3, "capacité 1 sur 4 candidats : 3 évincés");
        // `normal` (importance finie 1.0, la plus élevée après sanitisation
        // des non-finies à 0.0) doit survivre.
        assert!(!first.contains(&"normal".to_string()));
    }

    #[test]
    fn out_of_range_importance_is_ordered_but_never_panics() {
        // Importance négative ou très grande (hors la plage [0,1] "documentée"
        // mais jamais validée à l'écriture) reste un simple facteur additif :
        // pas de clamp requis, juste un ordre total stable.
        let candidates = vec![
            candidate("negative", -100.0, 0),
            candidate("huge", 1e300, 0),
            candidate("normal", 1.0, 0),
        ];
        let policy = AdaptiveForgettingPolicy {
            capacity: 2,
            recency_half_life_secs: 3_600,
        };
        let evicted = select_victims(&candidates, 100, policy);
        assert_eq!(
            evicted,
            vec!["negative".to_string()],
            "importance négative doit perdre face à `huge` et `normal`"
        );
    }
}
