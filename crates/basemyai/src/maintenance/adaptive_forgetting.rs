// SPDX-License-Identifier: BUSL-1.1
//! Oubli adaptatif (VISION §5.2, ADR-012 §4), porté sur le moteur natif par
//! ADR-037 puis **borné en mémoire** par ADR-041 §7.3 : plus de scan complet
//! matérialisé — deux passes paginées sur
//! [`crate::storage::MemoryStore::scan_for_forgetting`] :
//!
//! 1. **Sélection** ([`select_survivors`]) : scan par pages, tas borné à
//!    `capacity` ([`SurvivorSelector`]) qui ne retient que les `capacity`
//!    meilleurs scores de rétention — mémoire `O(capacity + page)`, calcul
//!    `O(n log capacity)`. Le résultat est l'ensemble des **survivants**,
//!    jamais la liste des victimes (elle, est `O(n - capacity)`, non bornée).
//! 2. **Éviction** ([`next_victim_page`]) : re-scan par pages au même `now`,
//!    éviction de tout candidat hors de l'ensemble des survivants, par lots
//!    atomiques bornés ([`crate::storage::MemoryStore::forget_many`],
//!    ADR-041 §7.4) — jamais une transaction moteur par victime, jamais un
//!    lot géant non plus.
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
//! Deux points d'entrée, une seule sélection ([`select_survivors`]) :
//! - [`crate::Memory::adaptive_forget`] évince par lots bornés en émettant
//!   les événements `Forgotten` après commit (cf.
//!   `MemorySubscription`/ADR-022) — le chemin
//!   programmatique/`MaintenanceTask` ([`AdaptiveForgettingTask`]).
//! - [`run`] évince directement via
//!   [`crate::storage::MemoryStore::forget_many`], sans passer par un
//!   [`crate::Memory`] complet (donc sans charger l'embedder Candle) — le
//!   chemin CLI, qui n'a besoin d'aucun embedding pour une opération
//!   purement temporelle/de capacité, et supporte le dry-run (aucune
//!   éviction, juste le rapport).
//!
//! Fenêtre entre les deux passes : le prédicat de population est **gelé** au
//! `now` de la passe 1 (un souvenir inséré ensuite avec la validité par
//! défaut a `valid_from > now`, donc n'entre jamais dans la passe 2). Seul
//! un souvenir inséré entre les passes avec un `valid_from` explicitement
//! antidaté peut être évincé sans avoir été scoré — fenêtre de la durée
//! d'une passe, assumée pour une politique de capacité (documenté ADR-041).

use std::collections::{BinaryHeap, HashSet};
use std::sync::Arc;

use basemyai_core::{MaintenanceTask, Result as CoreResult};

use crate::storage::{ForgetCandidate, MemoryStore};
use crate::{AgentId, Memory, Result, now_unix};

/// Taille de page des deux passes (sélection et éviction) — interne : le
/// contrat public reste « mémoire bornée », pas une taille précise.
pub(crate) const SCAN_PAGE_SIZE: usize = 512;

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
/// de `importance + 1`. `importance` non finie (`NaN`/`±inf` — rejetée par
/// l'API publique depuis ADR-041 §7.1, mais un import ADR-036 rejoue des
/// valeurs arbitraires depuis un fichier JSONL non fiable) est ramenée à
/// `0.0` : laisser passer un `NaN` romprait l'ordre total exigé par
/// [`SurvivorSelector`] (`NaN.partial_cmp` renvoie toujours `None`), ce qui
/// rendrait la sélection non déterministe pour *tous* les candidats comparés
/// au souvenir corrompu, pas seulement pour lui.
fn retention_score(importance: f64, half_life_secs: i64, last_access: i64, now: i64) -> f64 {
    let half_life = half_life_secs.max(1) as f64;
    #[allow(clippy::cast_precision_loss)]
    let age = now.saturating_sub(last_access).max(0) as f64;
    let importance = if importance.is_finite() { importance } else { 0.0 };
    importance + half_life / (half_life + age)
}

/// Une entrée du tas de sélection : le rang de rétention total d'un candidat.
/// L'`Ord` est inversé exprès — « plus grand » == « plus faible » (score plus
/// bas ; à score égal, id plus grand — l'id croissant survit, même règle de
/// départage qu'ADR-012) — pour que le sommet du `BinaryHeap` (max-heap) soit
/// toujours le survivant le plus faible, celui qu'un meilleur challenger
/// remplace en `O(log capacity)`.
struct Ranked {
    score: f64,
    id: String,
}

impl PartialEq for Ranked {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == std::cmp::Ordering::Equal
    }
}
impl Eq for Ranked {}
impl PartialOrd for Ranked {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Ranked {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // `retention_score` ne produit jamais de NaN (importance sanitisée),
        // le `unwrap_or(Equal)` est une ceinture, pas un chemin réel.
        other
            .score
            .partial_cmp(&self.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| self.id.cmp(&other.id))
    }
}

/// Sélectionne les `capacity` meilleurs candidats vus, en mémoire
/// `O(capacity)` quelle que soit la taille de la population offerte
/// (ADR-041 §7.3). Pur (pas d'I/O, pas d'horloge — `now` est un paramètre de
/// construction) : testable exhaustivement sans moteur ouvert.
///
/// L'ordre d'offre est sans effet sur le résultat : le rang (score
/// décroissant, id croissant en départage) est un ordre total sur des ids
/// uniques, donc l'ensemble des `capacity` meilleurs est unique.
pub(crate) struct SurvivorSelector {
    capacity: usize,
    half_life_secs: i64,
    now: i64,
    heap: BinaryHeap<Ranked>,
}

impl SurvivorSelector {
    pub(crate) fn new(policy: AdaptiveForgettingPolicy, now: i64) -> Self {
        Self {
            capacity: policy.capacity,
            half_life_secs: policy.recency_half_life_secs,
            now,
            heap: BinaryHeap::with_capacity(policy.capacity.saturating_add(1).min(4096)),
        }
    }

    /// Offre un candidat à la sélection. Ne clone son id que s'il entre
    /// réellement dans le tas (le cas perdant — le plus fréquent au-delà de
    /// `capacity` — ne coûte qu'un score et une comparaison).
    pub(crate) fn offer(&mut self, candidate: &ForgetCandidate) {
        if self.capacity == 0 {
            return;
        }
        let score = retention_score(
            candidate.importance,
            self.half_life_secs,
            candidate.last_access,
            self.now,
        );
        if self.heap.len() < self.capacity {
            self.heap.push(Ranked {
                score,
                id: candidate.id.clone(),
            });
            return;
        }
        if let Some(mut weakest) = self.heap.peek_mut() {
            // Le challenger est plus fort ssi son score est plus haut, ou —
            // à score égal — son id plus petit (l'id croissant survit).
            let challenger_is_stronger = match score.partial_cmp(&weakest.score) {
                Some(std::cmp::Ordering::Greater) => true,
                Some(std::cmp::Ordering::Equal) => candidate.id < weakest.id,
                Some(std::cmp::Ordering::Less) | None => false,
            };
            if challenger_is_stronger {
                *weakest = Ranked {
                    score,
                    id: candidate.id.clone(),
                };
            }
        }
    }

    pub(crate) fn into_survivors(self) -> HashSet<String> {
        self.heap.into_iter().map(|r| r.id).collect()
    }
}

/// Résultat de la passe 1 ([`select_survivors`]).
pub(crate) struct BoundedSelection {
    /// Population **active** vue par le scan (ADR-038 : les invalidés/
    /// expirés en sont déjà exclus par `scan_for_forgetting`), jamais le
    /// total brut de la table.
    pub(crate) scanned: usize,
    /// L'instant unique de la passe : réutilisé par la passe 2 pour geler le
    /// prédicat de population (un scan et une éviction évalués à des
    /// instants différents pourraient inclure/exclure un souvenir de façon
    /// incohérente entre les deux passes).
    pub(crate) now: i64,
    /// `None` ⇔ population ≤ `capacity` : rien à évincer, passe 2 inutile.
    pub(crate) survivors: Option<HashSet<String>>,
}

/// Passe 1 : scan paginé + sélection bornée des survivants (ADR-041 §7.3).
///
/// # Errors
/// Propage les erreurs de stockage du scan.
pub(crate) async fn select_survivors(
    store: &Arc<dyn MemoryStore>,
    agent: &AgentId,
    policy: AdaptiveForgettingPolicy,
    page_size: usize,
) -> Result<BoundedSelection> {
    let page_size = page_size.max(1);
    let now = now_unix();
    let mut selector = SurvivorSelector::new(policy, now);
    let mut scanned = 0usize;
    let mut cursor: Option<String> = None;
    loop {
        let page = store
            .scan_for_forgetting(agent, now, cursor.as_deref(), page_size)
            .await?;
        scanned += page.len();
        for candidate in &page {
            selector.offer(candidate);
        }
        if page.len() < page_size {
            break;
        }
        cursor = page.last().map(|c| c.id.clone());
    }
    let survivors = (scanned > policy.capacity).then(|| selector.into_survivors());
    Ok(BoundedSelection {
        scanned,
        now,
        survivors,
    })
}

/// Une page de victimes de la passe 2 : les candidats de la page hors de
/// l'ensemble des survivants, plus le curseur brut pour continuer.
pub(crate) struct VictimPage {
    pub(crate) victims: Vec<String>,
    /// Dernier id **candidat** de la page brute — le `after_id` du prochain
    /// appel. Porté par l'id (pas la position) : la page suivante reste
    /// correcte alors même que l'appelant vient d'évincer les victimes de
    /// celle-ci (même argument de curseur que `scan_expired`, ADR-038).
    pub(crate) cursor: Option<String>,
    /// `true` ⇔ la population est épuisée : dernière page.
    pub(crate) exhausted: bool,
}

/// Passe 2, une page à la fois : à l'appelant d'évincer `victims` par son
/// propre chemin (`Memory::forget_batch_with_events` — événementiel — ou
/// [`MemoryStore::forget_many`] direct, ADR-041 §7.4) puis de rappeler avec
/// `cursor`. `now` **doit** être celui de la passe 1 (voir
/// [`BoundedSelection::now`]).
///
/// # Errors
/// Propage les erreurs de stockage du scan.
pub(crate) async fn next_victim_page(
    store: &Arc<dyn MemoryStore>,
    agent: &AgentId,
    now: i64,
    survivors: &HashSet<String>,
    cursor: Option<&str>,
    page_size: usize,
) -> Result<VictimPage> {
    let page_size = page_size.max(1);
    let page = store.scan_for_forgetting(agent, now, cursor, page_size).await?;
    let exhausted = page.len() < page_size;
    let cursor = page.last().map(|c| c.id.clone());
    let victims = page
        .into_iter()
        .map(|c| c.id)
        .filter(|id| !survivors.contains(id))
        .collect();
    Ok(VictimPage {
        victims,
        cursor,
        exhausted,
    })
}

/// Passe d'oubli adaptatif **sans `Memory`** : opère directement sur
/// [`MemoryStore`], donc sans charger l'embedder Candle — le chemin CLI
/// (`basemyai forget-adaptive`), qui n'a besoin d'aucun embedding pour une
/// opération purement temporelle/de capacité (miroir de la façon dont
/// `list`/`forget`/`invalidate`/`purge` évitent déjà `open_memory`).
///
/// `dry_run = true` calcule et renvoie le rapport (ce qui **serait** évincé
/// d'après l'instantané de la passe 1) sans évincer quoi que ce soit —
/// aucune mutation, aucun appel à [`MemoryStore::forget_many`].
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
    let selection = select_survivors(store, agent, policy, SCAN_PAGE_SIZE).await?;
    let Some(survivors) = selection.survivors else {
        return Ok(ForgettingReport {
            scanned: selection.scanned,
            evicted: 0,
        });
    };
    if dry_run {
        return Ok(ForgettingReport {
            scanned: selection.scanned,
            evicted: selection.scanned - survivors.len(),
        });
    }
    let mut evicted = 0usize;
    let mut cursor: Option<String> = None;
    loop {
        let page = next_victim_page(
            store,
            agent,
            selection.now,
            &survivors,
            cursor.as_deref(),
            SCAN_PAGE_SIZE,
        )
        .await?;
        // Éviction par lot borné (ADR-041 §7.4) plutôt qu'une transaction
        // moteur par victime — le curseur est porté par l'id du dernier
        // candidat brut, insensible aux suppressions derrière lui.
        evicted += usize::try_from(
            store
                .forget_many(agent, &page.victims, crate::storage::ForgetBatchOptions::default())
                .await?,
        )
        .unwrap_or(usize::MAX);
        if page.exhausted {
            break;
        }
        cursor = page.cursor;
    }
    Ok(ForgettingReport {
        scanned: selection.scanned,
        evicted,
    })
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

    /// Équivalent test du `select_victims` d'ADR-037, reconstruit sur la
    /// sélection bornée : offre tous les candidats au [`SurvivorSelector`],
    /// renvoie les ids hors survivants. Les assertions de comportement
    /// (départages, sanitisation, capacité) restent donc mot pour mot celles
    /// de la version non bornée.
    fn victims_of(candidates: &[ForgetCandidate], now: i64, policy: AdaptiveForgettingPolicy) -> Vec<String> {
        if candidates.len() <= policy.capacity {
            return Vec::new();
        }
        let mut selector = SurvivorSelector::new(policy, now);
        for c in candidates {
            selector.offer(c);
        }
        let survivors = selector.into_survivors();
        candidates
            .iter()
            .filter(|c| !survivors.contains(&c.id))
            .map(|c| c.id.clone())
            .collect()
    }

    #[test]
    fn under_capacity_evicts_nothing() {
        let candidates = vec![candidate("a", 1.0, 0), candidate("b", 1.0, 0)];
        let policy = AdaptiveForgettingPolicy {
            capacity: 5,
            recency_half_life_secs: 3600,
        };
        assert!(victims_of(&candidates, 100, policy).is_empty());
    }

    #[test]
    fn at_capacity_evicts_nothing() {
        let candidates = vec![candidate("a", 1.0, 0), candidate("b", 1.0, 0)];
        let policy = AdaptiveForgettingPolicy {
            capacity: 2,
            recency_half_life_secs: 3600,
        };
        assert!(victims_of(&candidates, 100, policy).is_empty());
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
        let mut evicted = victims_of(&candidates, 1_000, policy);
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
        let evicted = victims_of(&[old, recent], 10_000, policy);
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
        let mut evicted = victims_of(&candidates, 0, policy);
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
        assert_eq!(victims_of(&candidates, 0, policy), vec!["a".to_string()]);
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
        let evicted = victims_of(&candidates, 100, policy);
        assert_eq!(evicted.len(), 2);
    }

    #[test]
    fn no_candidates_evicts_nothing() {
        let policy = AdaptiveForgettingPolicy {
            capacity: 0,
            recency_half_life_secs: 3_600,
        };
        assert!(victims_of(&[], 0, policy).is_empty());
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
        let evicted = victims_of(&[future, present], 0, policy);
        assert_eq!(evicted, vec!["present".to_string()]);
    }

    #[test]
    fn non_finite_importance_does_not_break_total_order() {
        // NaN/±inf sont rejetés par l'API publique (ADR-041 §7.1), mais un
        // import (ADR-036) rejoue des valeurs arbitraires depuis un JSONL
        // non fiable : la sélection doit rester déterministe et ne jamais
        // paniquer.
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
        let mut first = victims_of(&candidates, 100, policy);
        let mut second = victims_of(&candidates, 100, policy);
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
        let evicted = victims_of(&candidates, 100, policy);
        assert_eq!(
            evicted,
            vec!["negative".to_string()],
            "importance négative doit perdre face à `huge` et `normal`"
        );
    }

    #[test]
    fn selection_is_independent_of_offer_order() {
        // L'ensemble des survivants ne doit dépendre que du rang total,
        // jamais de l'ordre dans lequel les pages arrivent.
        let mut candidates: Vec<ForgetCandidate> = (0..50)
            .map(|i| candidate(&format!("m{i:02}"), f64::from(i % 7), i64::from(i * 13 % 11)))
            .collect();
        let policy = AdaptiveForgettingPolicy {
            capacity: 12,
            recency_half_life_secs: 3_600,
        };
        let mut forward = victims_of(&candidates, 1_000, policy);
        candidates.reverse();
        let mut backward = victims_of(&candidates, 1_000, policy);
        forward.sort();
        backward.sort();
        assert_eq!(forward, backward);
        assert_eq!(forward.len(), 50 - 12);
    }

    /// Stub `MemoryStore` scripté pour tester les deux passes paginées sans
    /// moteur : sert `scan_for_forgetting` fidèlement (tri par id, curseur
    /// exclusif, limite) sur une population fixe, enregistre les `forget`.
    /// Toute autre méthode est hors du chemin testé (`unimplemented!`).
    mod paged {
        use std::sync::Mutex;

        use basemyai_core::Metric;

        use super::*;
        use crate::memory::AgentStats;
        use crate::storage::{ExpiredCandidate, HydratedRecord, ListedRecord, NewMemory};
        use crate::temporal::Validity;
        use crate::{MemoryLayer, Reached, Record};

        pub(super) struct ScriptedStore {
            pub(super) candidates: Mutex<Vec<ForgetCandidate>>,
            pub(super) forgotten: Mutex<Vec<String>>,
        }

        impl ScriptedStore {
            pub(super) fn new(mut candidates: Vec<ForgetCandidate>) -> Self {
                candidates.sort_by(|a, b| a.id.cmp(&b.id));
                Self {
                    candidates: Mutex::new(candidates),
                    forgotten: Mutex::new(Vec::new()),
                }
            }
        }

        #[async_trait::async_trait]
        impl MemoryStore for ScriptedStore {
            async fn scan_for_forgetting(
                &self,
                _agent: &AgentId,
                _now: i64,
                after_id: Option<&str>,
                limit: usize,
            ) -> Result<Vec<ForgetCandidate>> {
                let guard = self.candidates.lock().expect("candidates lock");
                Ok(guard
                    .iter()
                    .filter(|c| after_id.is_none_or(|cursor| c.id.as_str() > cursor))
                    .take(limit)
                    .cloned()
                    .collect())
            }

            async fn forget(&self, _agent: &AgentId, id: &str) -> Result<()> {
                self.candidates.lock().expect("candidates lock").retain(|c| c.id != id);
                self.forgotten.lock().expect("forgotten lock").push(id.to_string());
                Ok(())
            }

            async fn forget_many(
                &self,
                agent: &AgentId,
                ids: &[String],
                _options: crate::storage::ForgetBatchOptions,
            ) -> Result<u64> {
                let mut removed = 0u64;
                for id in ids {
                    let existed = self
                        .candidates
                        .lock()
                        .expect("candidates lock")
                        .iter()
                        .any(|c| &c.id == id);
                    if existed {
                        self.forget(agent, id).await?;
                        removed += 1;
                    }
                }
                Ok(removed)
            }

            // ── hors du chemin testé ────────────────────────────────────
            #[allow(clippy::too_many_arguments)]
            async fn put_memory(
                &self,
                _id: &str,
                _agent: &AgentId,
                _layer: MemoryLayer,
                _text: &str,
                _validity: Validity,
                _vector: &[f32],
                _source: &str,
                _importance: f64,
            ) -> Result<()> {
                unimplemented!()
            }
            async fn put_memory_batch(&self, _agent: &AgentId, _items: &[NewMemory<'_>]) -> Result<()> {
                unimplemented!()
            }
            async fn set_importance(&self, _agent: &AgentId, _id: &str, _importance: f64) -> Result<()> {
                unimplemented!()
            }
            #[allow(clippy::too_many_arguments)]
            async fn recall_vector(
                &self,
                _agent: &AgentId,
                _query: &[f32],
                _k: usize,
                _layer: Option<MemoryLayer>,
                _metric: Metric,
                _now: i64,
                _include_procedural: bool,
            ) -> Result<Vec<Record>> {
                unimplemented!()
            }
            async fn recall_graph_filtered(
                &self,
                _agent: &AgentId,
                _query: &[f32],
                _k: usize,
                _now: i64,
                _include_procedural: bool,
                _include_imported: bool,
            ) -> Result<Vec<Record>> {
                unimplemented!()
            }
            async fn vector_ranking_ids(
                &self,
                _agent: &AgentId,
                _query: &[f32],
                _k: usize,
                _now: i64,
                _include_procedural: bool,
            ) -> Result<Vec<String>> {
                unimplemented!()
            }
            async fn keyword_ranking_ids(
                &self,
                _agent: &AgentId,
                _match_expr: &str,
                _k: usize,
                _now: i64,
                _include_procedural: bool,
            ) -> Result<Vec<String>> {
                unimplemented!()
            }
            async fn hydrate(&self, _agent: &AgentId, _ids: &[String], _now: i64) -> Result<Vec<HydratedRecord>> {
                unimplemented!()
            }
            async fn invalidate(&self, _agent: &AgentId, _id: &str, _now: i64) -> Result<()> {
                unimplemented!()
            }
            async fn purge_agent(&self, _agent: &AgentId) -> Result<()> {
                unimplemented!()
            }
            async fn agent_stats(&self, _agent: &AgentId, _now: i64) -> Result<AgentStats> {
                unimplemented!()
            }
            async fn graph_upsert_entity(
                &self,
                _agent: &AgentId,
                _id: &str,
                _kind: &str,
                _label: &str,
                _validity: Validity,
                _source: basemyai_engine::GraphSource,
            ) -> Result<()> {
                unimplemented!()
            }
            async fn graph_upsert_edge(
                &self,
                _agent: &AgentId,
                _src: &str,
                _relation: &str,
                _dst: &str,
                _weight: f64,
                _now: i64,
                _source: basemyai_engine::GraphSource,
            ) -> Result<()> {
                unimplemented!()
            }
            async fn graph_traverse(
                &self,
                _agent: &AgentId,
                _start: &str,
                _max_depth: u32,
                _now: i64,
            ) -> Result<Vec<Reached>> {
                unimplemented!()
            }
            async fn recent_episodes(&self, _agent: &AgentId, _limit: usize, _now: i64) -> Result<Vec<String>> {
                unimplemented!()
            }
            async fn exact_fact_exists(&self, _agent: &AgentId, _content: &str, _at: i64) -> Result<bool> {
                unimplemented!()
            }
            async fn layer_of(&self, _agent: &AgentId, _id: &str) -> Result<Option<MemoryLayer>> {
                unimplemented!()
            }
            async fn list_memories(
                &self,
                _agent: &AgentId,
                _layer: Option<MemoryLayer>,
                _limit: usize,
                _include_invalid: bool,
                _now: i64,
            ) -> Result<Vec<ListedRecord>> {
                unimplemented!()
            }
            async fn scan_expired(
                &self,
                _agent: &AgentId,
                _now: i64,
                _after_id: Option<&str>,
                _limit: usize,
            ) -> Result<Vec<ExpiredCandidate>> {
                unimplemented!()
            }
        }
    }

    fn test_agent() -> AgentId {
        AgentId::new("agent-test").expect("agent id")
    }

    /// Les deux passes, pilotées avec une page minuscule sur une population
    /// bien plus grande : le résultat doit être identique à la sélection
    /// non paginée, et chaque page de victimes doit rester bornée.
    #[tokio::test]
    async fn paged_two_pass_run_matches_unpaged_selection() {
        let candidates: Vec<ForgetCandidate> = (0..37)
            .map(|i| candidate(&format!("m{i:02}"), f64::from(i % 5), i64::from((i * 17) % 13)))
            .collect();
        let policy = AdaptiveForgettingPolicy {
            capacity: 10,
            recency_half_life_secs: 3_600,
        };
        let expected: HashSet<String> = victims_of(&candidates, now_unix(), policy).into_iter().collect();

        let store: Arc<dyn MemoryStore> = Arc::new(paged::ScriptedStore::new(candidates));
        let page_size = 4usize;
        let selection = select_survivors(&store, &test_agent(), policy, page_size)
            .await
            .expect("select");
        assert_eq!(selection.scanned, 37);
        let survivors = selection.survivors.expect("population > capacité");
        assert_eq!(survivors.len(), 10);

        let mut evicted: Vec<String> = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let page = next_victim_page(
                &store,
                &test_agent(),
                selection.now,
                &survivors,
                cursor.as_deref(),
                page_size,
            )
            .await
            .expect("victim page");
            assert!(page.victims.len() <= page_size, "une page de victimes reste bornée");
            for id in &page.victims {
                store.forget(&test_agent(), id).await.expect("forget");
                evicted.push(id.clone());
            }
            if page.exhausted {
                break;
            }
            cursor = page.cursor;
        }
        assert_eq!(evicted.len(), 37 - 10);
        assert_eq!(evicted.iter().cloned().collect::<HashSet<_>>(), expected);
    }

    /// Le point d'entrée CLI [`run`] bout en bout sur le stub : rapport
    /// exact, éviction effective, et dry-run sans aucune mutation.
    #[tokio::test]
    async fn run_evicts_through_pages_and_dry_run_mutates_nothing() {
        let candidates: Vec<ForgetCandidate> = (0..23)
            .map(|i| candidate(&format!("m{i:02}"), 1.0, i64::from(i)))
            .collect();
        let policy = AdaptiveForgettingPolicy {
            capacity: 5,
            recency_half_life_secs: 3_600,
        };

        let store_impl = Arc::new(paged::ScriptedStore::new(candidates.clone()));
        let store: Arc<dyn MemoryStore> = Arc::clone(&store_impl) as Arc<dyn MemoryStore>;

        let preview = run(&store, &test_agent(), policy, true).await.expect("dry run");
        assert_eq!(
            preview,
            ForgettingReport {
                scanned: 23,
                evicted: 18
            }
        );
        assert!(
            store_impl.forgotten.lock().expect("lock").is_empty(),
            "un dry-run ne doit rien évincer"
        );

        let report = run(&store, &test_agent(), policy, false).await.expect("run");
        assert_eq!(
            report,
            ForgettingReport {
                scanned: 23,
                evicted: 18
            }
        );
        assert_eq!(store_impl.candidates.lock().expect("lock").len(), 5);
    }
}
