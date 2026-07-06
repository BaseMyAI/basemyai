//! Scénarios portés depuis `../storage_contract.rs` (même sémantique, mais
//! exprimés en données rejouables contre n'importe quel [`MemoryStore`] via
//! [`super::run_scenario`]) — couvrent les comportements listés au N2 de
//! `docs/TODO-NATIVE-ENGINE.md` : remember/recall, invalidate, graphe,
//! validité temporelle.

use basemyai::MemoryLayer;
use basemyai::temporal::Validity;

use super::{Scenario, Step};

/// Tous les scénarios enregistrés : chaque backend (`backend_suite!` dans
/// `../memory_tests.rs`) les rejoue intégralement.
#[must_use]
pub(crate) fn all() -> Vec<Scenario> {
    vec![
        remember_recall_roundtrip(),
        invalidate_hides_from_recall(),
        forget_deletes_physically(),
        graph_upsert_and_traverse(),
        temporal_validity_boundary(),
        keyword_ranking_orders_by_relevance_and_truncates(),
        keyword_ranking_respects_temporal_validity_and_forget(),
    ]
}

/// Remember puis recall retrouve exactement l'item mémorisé.
fn remember_recall_roundtrip() -> Scenario {
    Scenario {
        name: "remember_recall_roundtrip",
        agent: "scenario-remember-recall",
        steps: vec![
            Step::Remember {
                id: "m1",
                layer: MemoryLayer::Episodic,
                text: "bonjour",
                vector_seed: 1,
                validity: Validity::since(0),
                source: "user",
            },
            Step::ExpectRecallVector {
                label: "recall après remember",
                query_seed: 1,
                k: 5,
                layer: None,
                now: 0,
                expect_ids: &["m1"],
            },
        ],
    }
}

/// `invalidate` masque le souvenir du recall (à partir de l'instant
/// d'invalidation) sans le supprimer physiquement.
fn invalidate_hides_from_recall() -> Scenario {
    Scenario {
        name: "invalidate_hides_from_recall",
        agent: "scenario-invalidate",
        steps: vec![
            Step::Remember {
                id: "m1",
                layer: MemoryLayer::Episodic,
                text: "x",
                vector_seed: 1,
                validity: Validity::since(0),
                source: "user",
            },
            Step::ExpectRecallVector {
                label: "présent avant invalidation",
                query_seed: 1,
                k: 5,
                layer: None,
                now: 50,
                expect_ids: &["m1"],
            },
            Step::Invalidate { id: "m1", at: 100 },
            Step::ExpectRecallVector {
                label: "absent après invalidation",
                query_seed: 1,
                k: 5,
                layer: None,
                now: 100,
                expect_ids: &[],
            },
        ],
    }
}

/// `forget` supprime physiquement (contrairement à `invalidate`) : le recall
/// ne le retrouve plus, à n'importe quel instant.
fn forget_deletes_physically() -> Scenario {
    Scenario {
        name: "forget_deletes_physically",
        agent: "scenario-forget",
        steps: vec![
            Step::Remember {
                id: "m1",
                layer: MemoryLayer::Episodic,
                text: "x",
                vector_seed: 1,
                validity: Validity::since(0),
                source: "user",
            },
            Step::Forget { id: "m1" },
            Step::ExpectRecallVector {
                label: "absent après forget",
                query_seed: 1,
                k: 5,
                layer: None,
                now: 0,
                expect_ids: &[],
            },
        ],
    }
}

/// Upsert d'entités/arêtes idempotent, traversée multi-sauts.
fn graph_upsert_and_traverse() -> Scenario {
    Scenario {
        name: "graph_upsert_and_traverse",
        agent: "scenario-graph",
        steps: vec![
            Step::GraphEntity {
                id: "alice",
                kind: "person",
                label: "Alice",
                validity: Validity::since(0),
            },
            Step::GraphEntity {
                id: "acme",
                kind: "company",
                label: "Acme",
                validity: Validity::since(0),
            },
            Step::GraphEdge {
                src: "alice",
                relation: "employeur",
                dst: "acme",
                weight: 1.0,
                now: 0,
            },
            // Idempotence : ré-upserter la même entité/arête ne duplique rien.
            Step::GraphEntity {
                id: "alice",
                kind: "person",
                label: "Alice",
                validity: Validity::since(0),
            },
            Step::GraphEdge {
                src: "alice",
                relation: "employeur",
                dst: "acme",
                weight: 1.0,
                now: 0,
            },
            Step::ExpectGraphTraverse {
                label: "traversée depuis alice",
                start: "alice",
                max_depth: 1,
                now: 0,
                expect_ids: &["acme"],
            },
        ],
    }
}

/// Fenêtre de validité temporelle : borne basse inclusive, borne haute
/// (`valid_until`) exclusive.
fn temporal_validity_boundary() -> Scenario {
    Scenario {
        name: "temporal_validity_boundary",
        agent: "scenario-temporal",
        steps: vec![
            Step::Remember {
                id: "bounded",
                layer: MemoryLayer::Semantic,
                text: "fenêtre bornée",
                vector_seed: 7,
                validity: Validity {
                    valid_from: 100,
                    valid_until: Some(200),
                },
                source: "user",
            },
            Step::ExpectRecallVector {
                label: "avant valid_from : absent",
                query_seed: 7,
                k: 5,
                layer: None,
                now: 99,
                expect_ids: &[],
            },
            Step::ExpectRecallVector {
                label: "juste avant valid_until : présent",
                query_seed: 7,
                k: 5,
                layer: None,
                now: 199,
                expect_ids: &["bounded"],
            },
            Step::ExpectRecallVector {
                label: "à valid_until (borne exclusive) : absent",
                query_seed: 7,
                k: 5,
                layer: None,
                now: 200,
                expect_ids: &[],
            },
            Step::ExpectAgentStats {
                label: "stats juste avant expiration",
                now: 199,
                expect_total: 1,
            },
            Step::ExpectAgentStats {
                label: "stats après expiration",
                now: 200,
                expect_total: 0,
            },
        ],
    }
}

/// `keyword_ranking_ids` (ADR-028, BM25 natif) : un terme unique retrouve
/// exactement le souvenir qui le contient, un terme absent ne retourne rien,
/// et un `OR` classe par pertinence. `m1` répète son terme deux fois dans un
/// texte de même longueur que `m2` — à `df`/longueur/`idf` égaux entre les
/// deux termes (chacun n'apparaît que dans un seul des deux souvenirs), le
/// classement par `tf` croissant est une propriété monotone de BM25, robuste
/// à toute implémentation correcte (contrairement à comparer des scores
/// entre agents différents ou provenant de `df`/`idf` distincts, plus
/// sensible aux détails d'implémentation — évité ici volontairement).
fn keyword_ranking_orders_by_relevance_and_truncates() -> Scenario {
    Scenario {
        name: "keyword_ranking_orders_by_relevance_and_truncates",
        agent: "scenario-keyword-relevance",
        steps: vec![
            Step::Remember {
                id: "m1",
                layer: MemoryLayer::Episodic,
                text: "chat chat oiseau jardin",
                vector_seed: 1,
                validity: Validity::since(0),
                source: "user",
            },
            Step::Remember {
                id: "m2",
                layer: MemoryLayer::Episodic,
                text: "chien oiseau jardin arbre",
                vector_seed: 2,
                validity: Validity::since(0),
                source: "user",
            },
            Step::ExpectKeywordRankingIds {
                label: "terme unique retrouve le bon souvenir",
                match_expr: r#""chat""#,
                k: 10,
                now: 0,
                expect_ids: &["m1"],
            },
            Step::ExpectKeywordRankingIds {
                label: "terme absent : vide, jamais une erreur",
                match_expr: r#""dinosaure""#,
                k: 10,
                now: 0,
                expect_ids: &[],
            },
            Step::ExpectKeywordRankingIds {
                label: "OR : tf plus élevé à longueur/idf égaux classe en tête",
                match_expr: r#""chat" OR "chien""#,
                k: 10,
                now: 0,
                expect_ids: &["m1", "m2"],
            },
            Step::ExpectKeywordRankingIds {
                label: "k tronque au(x) meilleur(s) résultat(s)",
                match_expr: r#""chat" OR "chien""#,
                k: 1,
                now: 0,
                expect_ids: &["m1"],
            },
        ],
    }
}

/// `keyword_ranking_ids` respecte la même fenêtre de validité temporelle que
/// `recall_vector` (ADR-005) — porté depuis `temporal_validity_boundary` —
/// et `forget` le supprime physiquement, y compris de l'index full-text.
fn keyword_ranking_respects_temporal_validity_and_forget() -> Scenario {
    Scenario {
        name: "keyword_ranking_respects_temporal_validity_and_forget",
        agent: "scenario-keyword-temporal",
        steps: vec![
            Step::Remember {
                id: "bounded",
                layer: MemoryLayer::Semantic,
                text: "licorne mauve rarissime",
                vector_seed: 7,
                validity: Validity {
                    valid_from: 100,
                    valid_until: Some(200),
                },
                source: "user",
            },
            Step::ExpectKeywordRankingIds {
                label: "avant valid_from : absent",
                match_expr: r#""licorne""#,
                k: 5,
                now: 99,
                expect_ids: &[],
            },
            Step::ExpectKeywordRankingIds {
                label: "dans la fenêtre : présent",
                match_expr: r#""licorne""#,
                k: 5,
                now: 150,
                expect_ids: &["bounded"],
            },
            Step::ExpectKeywordRankingIds {
                label: "juste avant valid_until : présent",
                match_expr: r#""licorne""#,
                k: 5,
                now: 199,
                expect_ids: &["bounded"],
            },
            Step::ExpectKeywordRankingIds {
                label: "à valid_until (borne exclusive) : absent",
                match_expr: r#""licorne""#,
                k: 5,
                now: 200,
                expect_ids: &[],
            },
            Step::Forget { id: "bounded" },
            Step::ExpectKeywordRankingIds {
                label: "après forget : absent même dans la fenêtre",
                match_expr: r#""licorne""#,
                k: 5,
                now: 150,
                expect_ids: &[],
            },
        ],
    }
}
