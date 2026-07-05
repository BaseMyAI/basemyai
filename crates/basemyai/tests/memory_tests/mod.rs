//! Scaffold de tests **déclaratifs multi-backend** (N2,
//! `docs/TODO-NATIVE-ENGINE.md`, rationale : `docs/PLAN-NATIVE-ENGINE.md`
//! §3.2). Un [`Scenario`] décrit une séquence d'opérations mémoire
//! (remember/invalidate/graphe…) et les postconditions attendues, scopée à un
//! seul `agent`. [`run_scenario`] les rejoue contre **n'importe quelle**
//! implémentation de [`MemoryStore`] — la borne est générique
//! (`S: MemoryStore`), aucune implémentation concrète (`LibsqlMemoryStore` ou
//! une future `Native`) n'apparaît dans ce fichier.
//!
//! Aujourd'hui, seule `LibsqlMemoryStore` existe réellement (le second backend
//! natif dépend de N3/N4, non commencés) : ce module ne peut donc PAS encore
//! diffuser deux implémentations entre elles. Il pose le harnais — scénarios +
//! runner paramétré — pour que brancher un second backend soit mécanique :
//! implémenter `MemoryStore`, écrire une factory async, ajouter une ligne
//! `backend_suite!` dans `../memory_tests.rs`. Voir ce fichier pour
//! l'enregistrement des backends.

pub(crate) mod scenarios;

use basemyai::storage::MemoryStore;
use basemyai::temporal::Validity;
use basemyai::{AgentId, MemoryLayer};
use basemyai_core::Metric;

/// Vecteur déterministe à la dimension du schéma (même recette que
/// `storage_contract.rs`) : une graine identique donne un vecteur identique,
/// deux graines différentes donnent des vecteurs non colinéaires.
#[must_use]
pub(crate) fn vec_for(seed: u8) -> Vec<f32> {
    let dim = basemyai::EMBEDDING_DIM;
    let mut v = vec![0.0_f32; dim];
    v[usize::from(seed) % dim] = 1.0;
    v[0] += 0.001; // évite le vecteur nul même quand seed % dim == 0
    v
}

/// Une étape rejouée dans l'ordre par [`run_scenario`]. Les variantes
/// `Expect*` n'ont aucun effet de bord : elles interrogent le store et
/// comparent au résultat attendu, en panic-ant avec le nom du scénario et
/// l'étape en cause si ça ne correspond pas.
pub(crate) enum Step {
    /// `MemoryStore::put_memory`.
    Remember {
        id: &'static str,
        layer: MemoryLayer,
        text: &'static str,
        vector_seed: u8,
        validity: Validity,
        source: &'static str,
    },
    /// `MemoryStore::invalidate`.
    Invalidate { id: &'static str, at: i64 },
    /// `MemoryStore::forget`.
    Forget { id: &'static str },
    /// `MemoryStore::graph_upsert_entity`.
    GraphEntity {
        id: &'static str,
        kind: &'static str,
        label: &'static str,
        validity: Validity,
    },
    /// `MemoryStore::graph_upsert_edge`.
    GraphEdge {
        src: &'static str,
        relation: &'static str,
        dst: &'static str,
        weight: f64,
        now: i64,
    },
    /// Postcondition sur `MemoryStore::recall_vector` : les ids retournés
    /// doivent correspondre exactement (ordre compris) à `expect_ids`.
    ExpectRecallVector {
        label: &'static str,
        query_seed: u8,
        k: usize,
        layer: Option<MemoryLayer>,
        now: i64,
        expect_ids: &'static [&'static str],
    },
    /// Postcondition sur `MemoryStore::graph_traverse`.
    ExpectGraphTraverse {
        label: &'static str,
        start: &'static str,
        max_depth: u32,
        now: i64,
        expect_ids: &'static [&'static str],
    },
    /// Postcondition sur `MemoryStore::agent_stats` (total toutes couches).
    ExpectAgentStats {
        label: &'static str,
        now: i64,
        expect_total: usize,
    },
}

/// Un scénario : une séquence d'étapes, scopée à un seul `agent` (isolation
/// ADR-006 — deux scénarios différents doivent utiliser des `agent` distincts
/// s'ils partagent un store, même si en pratique chaque backend enregistré
/// via `backend_suite!` ouvre un store frais par scénario).
pub(crate) struct Scenario {
    pub name: &'static str,
    pub agent: &'static str,
    pub steps: Vec<Step>,
}

fn step_ctx(scenario: &str, i: usize, what: &str) -> String {
    format!("[{scenario}] étape {i} ({what})")
}

/// Rejoue `scenario` contre `store`. **C'est ici que la parenthèse
/// multi-backend se prouve** : la seule contrainte sur `store` est le trait
/// [`MemoryStore`], jamais un type concret — brancher un second backend ne
/// touche pas cette fonction.
///
/// # Panics
/// Panique (message préfixé par `[nom du scénario] étape N (...)`) si un
/// appel au store échoue, ou si une postcondition `Expect*` ne correspond pas.
pub(crate) async fn run_scenario<S: MemoryStore>(store: &S, scenario: &Scenario) {
    let agent = AgentId::new(scenario.agent).unwrap_or_else(|| panic!("[{}] agent id invalide", scenario.name));

    for (i, step) in scenario.steps.iter().enumerate() {
        match step {
            Step::Remember {
                id,
                layer,
                text,
                vector_seed,
                validity,
                source,
            } => {
                store
                    .put_memory(id, &agent, *layer, text, *validity, &vec_for(*vector_seed), source)
                    .await
                    .unwrap_or_else(|e| panic!("{}: put_memory a échoué: {e}", step_ctx(scenario.name, i, "remember")));
            }
            Step::Invalidate { id, at } => {
                store.invalidate(&agent, id, *at).await.unwrap_or_else(|e| {
                    panic!("{}: invalidate a échoué: {e}", step_ctx(scenario.name, i, "invalidate"))
                });
            }
            Step::Forget { id } => {
                store
                    .forget(&agent, id)
                    .await
                    .unwrap_or_else(|e| panic!("{}: forget a échoué: {e}", step_ctx(scenario.name, i, "forget")));
            }
            Step::GraphEntity {
                id,
                kind,
                label,
                validity,
            } => {
                store
                    .graph_upsert_entity(&agent, id, kind, label, *validity)
                    .await
                    .unwrap_or_else(|e| {
                        panic!(
                            "{}: graph_upsert_entity a échoué: {e}",
                            step_ctx(scenario.name, i, "graph_entity")
                        )
                    });
            }
            Step::GraphEdge {
                src,
                relation,
                dst,
                weight,
                now,
            } => {
                store
                    .graph_upsert_edge(&agent, src, relation, dst, *weight, *now)
                    .await
                    .unwrap_or_else(|e| {
                        panic!(
                            "{}: graph_upsert_edge a échoué: {e}",
                            step_ctx(scenario.name, i, "graph_edge")
                        )
                    });
            }
            Step::ExpectRecallVector {
                label,
                query_seed,
                k,
                layer,
                now,
                expect_ids,
            } => {
                let ctx = step_ctx(scenario.name, i, label);
                let got = store
                    .recall_vector(&agent, &vec_for(*query_seed), *k, *layer, Metric::Cosine, *now)
                    .await
                    .unwrap_or_else(|e| panic!("{ctx}: recall_vector a échoué: {e}"));
                let got_ids: Vec<&str> = got.iter().map(|r| r.id.as_str()).collect();
                assert_eq!(got_ids, *expect_ids, "{ctx}: recall_vector ids inattendus");
            }
            Step::ExpectGraphTraverse {
                label,
                start,
                max_depth,
                now,
                expect_ids,
            } => {
                let ctx = step_ctx(scenario.name, i, label);
                let got = store
                    .graph_traverse(&agent, start, *max_depth, *now)
                    .await
                    .unwrap_or_else(|e| panic!("{ctx}: graph_traverse a échoué: {e}"));
                let got_ids: Vec<&str> = got.iter().map(|r| r.id.as_str()).collect();
                assert_eq!(got_ids, *expect_ids, "{ctx}: graph_traverse ids inattendus");
            }
            Step::ExpectAgentStats {
                label,
                now,
                expect_total,
            } => {
                let ctx = step_ctx(scenario.name, i, label);
                let stats = store
                    .agent_stats(&agent, *now)
                    .await
                    .unwrap_or_else(|e| panic!("{ctx}: agent_stats a échoué: {e}"));
                assert_eq!(stats.total(), *expect_total, "{ctx}: total inattendu");
            }
        }
    }
}
