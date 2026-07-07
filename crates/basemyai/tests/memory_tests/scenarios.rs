//! Scénarios portés depuis `../storage_contract.rs` (même sémantique, mais
//! exprimés en données rejouables contre n'importe quel [`MemoryStore`] via
//! [`super::run_scenario`]) — couvrent l'intégralité de la surface de
//! `storage_contract.rs` (N5.3, `docs/TODO-NATIVE-ENGINE.md`) : remember/
//! recall (batch compris), invalidate, forget, purge, stats par couche,
//! hydrate, graphe, épisodes récents, dédup de faits, classement vecteur/
//! mot-clé, validité temporelle et isolation multi-agent.

use basemyai::MemoryLayer;
use basemyai::temporal::Validity;

use super::{BatchItem, Scenario, Step};

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
        recall_vector_isolated_per_agent(),
        recall_vector_excludes_expired_and_future(),
        recall_vector_filters_by_layer(),
        put_memory_batch_is_atomic_and_ordered(),
        put_memory_batch_empty_is_noop(),
        hydrate_preserves_order_and_skips_missing_ids(),
        hydrate_is_scoped_to_agent(),
        purge_agent_removes_memories_and_graph_only_for_that_agent(),
        agent_stats_counts_only_valid_memories_per_layer(),
        vector_and_keyword_ranking_ids_are_isolated(),
        recent_episodes_returns_only_valid_episodic_layer_newest_first(),
        exact_fact_exists_matches_only_semantic_layer_exact_content(),
    ]
}

/// Remember puis recall retrouve exactement l'item mémorisé.
fn remember_recall_roundtrip() -> Scenario {
    Scenario {
        name: "remember_recall_roundtrip",
        agent: "scenario-remember-recall",
        steps: vec![
            Step::Remember {
                agent: None,
                id: "m1",
                layer: MemoryLayer::Episodic,
                text: "bonjour",
                vector_seed: 1,
                validity: Validity::since(0),
                source: "user",
            },
            Step::ExpectRecallVector {
                agent: None,
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
                agent: None,
                id: "m1",
                layer: MemoryLayer::Episodic,
                text: "x",
                vector_seed: 1,
                validity: Validity::since(0),
                source: "user",
            },
            Step::ExpectRecallVector {
                agent: None,
                label: "présent avant invalidation",
                query_seed: 1,
                k: 5,
                layer: None,
                now: 50,
                expect_ids: &["m1"],
            },
            Step::Invalidate { id: "m1", at: 100 },
            Step::ExpectRecallVector {
                agent: None,
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
/// ne le retrouve plus, à n'importe quel instant, et `hydrate` non plus.
fn forget_deletes_physically() -> Scenario {
    Scenario {
        name: "forget_deletes_physically",
        agent: "scenario-forget",
        steps: vec![
            Step::Remember {
                agent: None,
                id: "m1",
                layer: MemoryLayer::Episodic,
                text: "x",
                vector_seed: 1,
                validity: Validity::since(0),
                source: "user",
            },
            Step::Forget { id: "m1" },
            Step::ExpectRecallVector {
                agent: None,
                label: "absent après forget",
                query_seed: 1,
                k: 5,
                layer: None,
                now: 0,
                expect_ids: &[],
            },
            Step::ExpectHydrate {
                agent: None,
                label: "hydrate ne retrouve plus le souvenir supprimé",
                ids: &["m1"],
                now: 0,
                expect_ids: &[],
            },
        ],
    }
}

/// Upsert d'entités/arêtes idempotent, traversée multi-sauts, et scopée à
/// l'agent : un autre agent ne traverse rien depuis `alice`.
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
                agent: None,
                label: "traversée depuis alice",
                start: "alice",
                max_depth: 1,
                now: 0,
                expect_ids: &["acme"],
            },
            Step::ExpectGraphTraverse {
                agent: Some("scenario-graph-other"),
                label: "un autre agent ne voit rien depuis alice",
                start: "alice",
                max_depth: 1,
                now: 0,
                expect_ids: &[],
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
                agent: None,
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
                agent: None,
                label: "avant valid_from : absent",
                query_seed: 7,
                k: 5,
                layer: None,
                now: 99,
                expect_ids: &[],
            },
            Step::ExpectRecallVector {
                agent: None,
                label: "juste avant valid_until : présent",
                query_seed: 7,
                k: 5,
                layer: None,
                now: 199,
                expect_ids: &["bounded"],
            },
            Step::ExpectRecallVector {
                agent: None,
                label: "à valid_until (borne exclusive) : absent",
                query_seed: 7,
                k: 5,
                layer: None,
                now: 200,
                expect_ids: &[],
            },
            Step::ExpectAgentStats {
                agent: None,
                label: "stats juste avant expiration",
                now: 199,
                expect_total: 1,
            },
            Step::ExpectAgentStats {
                agent: None,
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
                agent: None,
                id: "m1",
                layer: MemoryLayer::Episodic,
                text: "chat chat oiseau jardin",
                vector_seed: 1,
                validity: Validity::since(0),
                source: "user",
            },
            Step::Remember {
                agent: None,
                id: "m2",
                layer: MemoryLayer::Episodic,
                text: "chien oiseau jardin arbre",
                vector_seed: 2,
                validity: Validity::since(0),
                source: "user",
            },
            Step::ExpectKeywordRankingIds {
                agent: None,
                label: "terme unique retrouve le bon souvenir",
                match_expr: r#""chat""#,
                k: 10,
                now: 0,
                expect_ids: &["m1"],
            },
            Step::ExpectKeywordRankingIds {
                agent: None,
                label: "terme absent : vide, jamais une erreur",
                match_expr: r#""dinosaure""#,
                k: 10,
                now: 0,
                expect_ids: &[],
            },
            Step::ExpectKeywordRankingIds {
                agent: None,
                label: "OR : tf plus élevé à longueur/idf égaux classe en tête",
                match_expr: r#""chat" OR "chien""#,
                k: 10,
                now: 0,
                expect_ids: &["m1", "m2"],
            },
            Step::ExpectKeywordRankingIds {
                agent: None,
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
                agent: None,
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
                agent: None,
                label: "avant valid_from : absent",
                match_expr: r#""licorne""#,
                k: 5,
                now: 99,
                expect_ids: &[],
            },
            Step::ExpectKeywordRankingIds {
                agent: None,
                label: "dans la fenêtre : présent",
                match_expr: r#""licorne""#,
                k: 5,
                now: 150,
                expect_ids: &["bounded"],
            },
            Step::ExpectKeywordRankingIds {
                agent: None,
                label: "juste avant valid_until : présent",
                match_expr: r#""licorne""#,
                k: 5,
                now: 199,
                expect_ids: &["bounded"],
            },
            Step::ExpectKeywordRankingIds {
                agent: None,
                label: "à valid_until (borne exclusive) : absent",
                match_expr: r#""licorne""#,
                k: 5,
                now: 200,
                expect_ids: &[],
            },
            Step::Forget { id: "bounded" },
            Step::ExpectKeywordRankingIds {
                agent: None,
                label: "après forget : absent même dans la fenêtre",
                match_expr: r#""licorne""#,
                k: 5,
                now: 150,
                expect_ids: &[],
            },
        ],
    }
}

/// `recall_vector` est isolé par agent : deux agents mémorisent au même
/// vecteur, chacun ne retrouve que le sien — porté depuis
/// `recall_vector_is_isolated_per_agent`.
fn recall_vector_isolated_per_agent() -> Scenario {
    Scenario {
        name: "recall_vector_isolated_per_agent",
        agent: "scenario-isolation-b",
        steps: vec![
            Step::Remember {
                agent: Some("scenario-isolation-a"),
                id: "m1",
                layer: MemoryLayer::Episodic,
                text: "secret de A",
                vector_seed: 1,
                validity: Validity::since(0),
                source: "user",
            },
            Step::Remember {
                agent: None, // scenario-isolation-b
                id: "m2",
                layer: MemoryLayer::Episodic,
                text: "secret de B",
                vector_seed: 1,
                validity: Validity::since(0),
                source: "user",
            },
            Step::ExpectRecallVector {
                agent: None,
                label: "B ne voit que son propre souvenir",
                query_seed: 1,
                k: 5,
                layer: None,
                now: 0,
                expect_ids: &["m2"],
            },
        ],
    }
}

/// `recall_vector` exclut un souvenir expiré et un souvenir pas encore
/// valide, avec seulement 3 lignes (bien sous `KNN_OVERSAMPLE`), les trois
/// restent candidates à l'index ANN — porté depuis
/// `recall_vector_excludes_expired_and_not_yet_valid`.
fn recall_vector_excludes_expired_and_future() -> Scenario {
    Scenario {
        name: "recall_vector_excludes_expired_and_future",
        agent: "scenario-expired-future",
        steps: vec![
            Step::Remember {
                agent: None,
                id: "expired",
                layer: MemoryLayer::Episodic,
                text: "périmé",
                vector_seed: 1,
                validity: Validity {
                    valid_from: 900,
                    valid_until: Some(990),
                },
                source: "user",
            },
            Step::Remember {
                agent: None,
                id: "future",
                layer: MemoryLayer::Episodic,
                text: "pas encore",
                vector_seed: 2,
                validity: Validity {
                    valid_from: 1_100,
                    valid_until: None,
                },
                source: "user",
            },
            Step::Remember {
                agent: None,
                id: "live",
                layer: MemoryLayer::Episodic,
                text: "vivant",
                vector_seed: 3,
                validity: Validity::since(999),
                source: "user",
            },
            Step::ExpectRecallVector {
                agent: None,
                label: "seul le souvenir vivant à now=1000",
                query_seed: 3,
                k: 10,
                layer: None,
                now: 1_000,
                expect_ids: &["live"],
            },
        ],
    }
}

/// `recall_vector` filtre par couche — porté depuis
/// `recall_vector_filters_by_layer`.
fn recall_vector_filters_by_layer() -> Scenario {
    Scenario {
        name: "recall_vector_filters_by_layer",
        agent: "scenario-layer-filter",
        steps: vec![
            Step::Remember {
                agent: None,
                id: "ep",
                layer: MemoryLayer::Episodic,
                text: "un épisode",
                vector_seed: 1,
                validity: Validity::since(0),
                source: "user",
            },
            Step::Remember {
                agent: None,
                id: "sem",
                layer: MemoryLayer::Semantic,
                text: "un fait",
                vector_seed: 1,
                validity: Validity::since(0),
                source: "user",
            },
            Step::ExpectRecallVector {
                agent: None,
                label: "recall filtré sur semantic uniquement",
                query_seed: 1,
                k: 10,
                layer: Some(MemoryLayer::Semantic),
                now: 0,
                expect_ids: &["sem"],
            },
        ],
    }
}

/// `put_memory_batch` insère plusieurs souvenirs en un seul appel — porté
/// depuis `put_memory_batch_is_atomic_and_ordered`.
fn put_memory_batch_is_atomic_and_ordered() -> Scenario {
    static ITEMS: &[BatchItem] = &[
        BatchItem {
            id: "b1",
            layer: MemoryLayer::Episodic,
            text: "un",
            vector_seed: 1,
            validity: Validity {
                valid_from: 0,
                valid_until: None,
            },
            source: "user",
        },
        BatchItem {
            id: "b2",
            layer: MemoryLayer::Episodic,
            text: "deux",
            vector_seed: 2,
            validity: Validity {
                valid_from: 0,
                valid_until: None,
            },
            source: "user",
        },
    ];
    Scenario {
        name: "put_memory_batch_is_atomic_and_ordered",
        agent: "scenario-batch",
        steps: vec![
            Step::RememberBatch {
                agent: None,
                items: ITEMS,
            },
            Step::ExpectAgentStats {
                agent: None,
                label: "les deux items du batch sont présents",
                now: 0,
                expect_total: 2,
            },
        ],
    }
}

/// `put_memory_batch` sur un lot vide est un no-op — porté depuis
/// `put_memory_batch_empty_is_noop`.
fn put_memory_batch_empty_is_noop() -> Scenario {
    Scenario {
        name: "put_memory_batch_empty_is_noop",
        agent: "scenario-batch-empty",
        steps: vec![
            Step::RememberBatch {
                agent: None,
                items: &[],
            },
            Step::ExpectAgentStats {
                agent: None,
                label: "aucun souvenir après un batch vide",
                now: 0,
                expect_total: 0,
            },
        ],
    }
}

/// `hydrate` préserve l'ordre des ids demandés et ignore silencieusement un
/// id absent — porté depuis `hydrate_preserves_order_and_skips_missing_ids`.
fn hydrate_preserves_order_and_skips_missing_ids() -> Scenario {
    Scenario {
        name: "hydrate_preserves_order_and_skips_missing_ids",
        agent: "scenario-hydrate-order",
        steps: vec![
            Step::Remember {
                agent: None,
                id: "m1",
                layer: MemoryLayer::Episodic,
                text: "premier",
                vector_seed: 1,
                validity: Validity::since(0),
                source: "user",
            },
            Step::Remember {
                agent: None,
                id: "m2",
                layer: MemoryLayer::Episodic,
                text: "second",
                vector_seed: 2,
                validity: Validity::since(0),
                source: "user",
            },
            Step::ExpectHydrate {
                agent: None,
                label: "ordre demandé préservé, id manquant ignoré",
                ids: &["m2", "missing", "m1"],
                now: 0,
                expect_ids: &["m2", "m1"],
            },
        ],
    }
}

/// `hydrate` est scopé à l'agent même quand l'id est connu d'un autre agent
/// — porté depuis `hydrate_is_scoped_to_agent_even_when_id_is_known`.
fn hydrate_is_scoped_to_agent() -> Scenario {
    Scenario {
        name: "hydrate_is_scoped_to_agent",
        agent: "scenario-hydrate-scope-a",
        steps: vec![
            Step::Remember {
                agent: None, // scenario-hydrate-scope-a
                id: "known-to-b",
                layer: MemoryLayer::Semantic,
                text: "secret de A",
                vector_seed: 1,
                validity: Validity::since(0),
                source: "user",
            },
            Step::ExpectHydrate {
                agent: Some("scenario-hydrate-scope-b"),
                label: "B ne peut pas hydrater un id appartenant à A",
                ids: &["known-to-b"],
                now: 0,
                expect_ids: &[],
            },
        ],
    }
}

/// `purge_agent` supprime souvenirs et graphe d'un seul agent, sans toucher
/// aux autres — porté depuis
/// `purge_agent_removes_memories_and_graph_only_for_that_agent`.
fn purge_agent_removes_memories_and_graph_only_for_that_agent() -> Scenario {
    Scenario {
        name: "purge_agent_removes_memories_and_graph_only_for_that_agent",
        agent: "scenario-purge-a",
        steps: vec![
            Step::Remember {
                agent: None, // scenario-purge-a
                id: "m1",
                layer: MemoryLayer::Episodic,
                text: "x",
                vector_seed: 1,
                validity: Validity::since(0),
                source: "user",
            },
            Step::Remember {
                agent: Some("scenario-purge-b"),
                id: "m2",
                layer: MemoryLayer::Episodic,
                text: "y",
                vector_seed: 1,
                validity: Validity::since(0),
                source: "user",
            },
            Step::GraphEntity {
                id: "e1",
                kind: "thing",
                label: "E1",
                validity: Validity::since(0),
            },
            Step::PurgeAgent { agent: None }, // purge scenario-purge-a
            Step::ExpectAgentStats {
                agent: None,
                label: "A n'a plus rien après sa purge",
                now: 0,
                expect_total: 0,
            },
            Step::ExpectAgentStats {
                agent: Some("scenario-purge-b"),
                label: "B garde ses souvenirs",
                now: 0,
                expect_total: 1,
            },
        ],
    }
}

/// `agent_stats` compte uniquement les souvenirs valides, par couche —
/// porté depuis `agent_stats_counts_only_valid_memories_per_layer`.
fn agent_stats_counts_only_valid_memories_per_layer() -> Scenario {
    Scenario {
        name: "agent_stats_counts_only_valid_memories_per_layer",
        agent: "scenario-stats-layers",
        steps: vec![
            Step::Remember {
                agent: None,
                id: "ep",
                layer: MemoryLayer::Episodic,
                text: "x",
                vector_seed: 1,
                validity: Validity::since(0),
                source: "user",
            },
            Step::Remember {
                agent: None,
                id: "sem",
                layer: MemoryLayer::Semantic,
                text: "y",
                vector_seed: 2,
                validity: Validity::since(0),
                source: "user",
            },
            Step::Remember {
                agent: None,
                id: "expired",
                layer: MemoryLayer::Semantic,
                text: "z",
                vector_seed: 3,
                validity: Validity {
                    valid_from: 0,
                    valid_until: Some(1),
                },
                source: "user",
            },
            Step::ExpectAgentStatsByLayer {
                agent: None,
                label: "un souvenir expiré n'est compté dans aucune couche",
                now: 100,
                expect_short_term: 0,
                expect_episodic: 1,
                expect_procedural: 0,
                expect_semantic: 1,
            },
        ],
    }
}

/// `vector_ranking_ids` et `keyword_ranking_ids` sont tous deux isolés par
/// agent — porté depuis `vector_and_keyword_ranking_ids_are_isolated_and_temporal`
/// (le volet temporel est déjà couvert par les deux scénarios `keyword_ranking_*`
/// ci-dessus).
fn vector_and_keyword_ranking_ids_are_isolated() -> Scenario {
    Scenario {
        name: "vector_and_keyword_ranking_ids_are_isolated",
        agent: "scenario-ranking-isolation-a",
        steps: vec![
            Step::Remember {
                agent: None, // scenario-ranking-isolation-a
                id: "m1",
                layer: MemoryLayer::Episodic,
                text: "le chat dort",
                vector_seed: 1,
                validity: Validity::since(0),
                source: "user",
            },
            Step::Remember {
                agent: Some("scenario-ranking-isolation-other"),
                id: "other",
                layer: MemoryLayer::Episodic,
                text: "le chat dort",
                vector_seed: 1,
                validity: Validity::since(0),
                source: "user",
            },
            Step::ExpectVectorRankingIds {
                agent: None,
                label: "vector_ranking_ids ne voit que le souvenir de l'agent",
                query_seed: 1,
                k: 10,
                now: 0,
                expect_ids: &["m1"],
            },
            Step::ExpectKeywordRankingIds {
                agent: None,
                label: "keyword_ranking_ids ne voit que le souvenir de l'agent",
                match_expr: r#""chat""#,
                k: 10,
                now: 0,
                expect_ids: &["m1"],
            },
        ],
    }
}

/// `recent_episodes` ne retourne que la couche épisodique valide, du plus
/// récent au plus ancien — porté depuis
/// `recent_episodes_returns_only_valid_episodic_layer_newest_first`.
fn recent_episodes_returns_only_valid_episodic_layer_newest_first() -> Scenario {
    Scenario {
        name: "recent_episodes_returns_only_valid_episodic_layer_newest_first",
        agent: "scenario-recent-episodes",
        steps: vec![
            Step::Remember {
                agent: None,
                id: "ep1",
                layer: MemoryLayer::Episodic,
                text: "premier épisode",
                vector_seed: 1,
                validity: Validity::since(10),
                source: "user",
            },
            Step::Remember {
                agent: None,
                id: "ep2",
                layer: MemoryLayer::Episodic,
                text: "second épisode",
                vector_seed: 2,
                validity: Validity::since(20),
                source: "user",
            },
            Step::Remember {
                agent: None,
                id: "sem",
                layer: MemoryLayer::Semantic,
                text: "un fait",
                vector_seed: 3,
                validity: Validity::since(15),
                source: "user",
            },
            Step::ExpectRecentEpisodes {
                agent: None,
                label: "seuls les épisodes, du plus récent au plus ancien",
                limit: 10,
                now: 100,
                expect_texts: &["second épisode", "premier épisode"],
            },
        ],
    }
}

/// `exact_fact_exists` ne matche que la couche sémantique, au contenu
/// exactement identique, scopé à l'agent — porté depuis
/// `exact_fact_exists_matches_only_semantic_layer_exact_content`.
fn exact_fact_exists_matches_only_semantic_layer_exact_content() -> Scenario {
    Scenario {
        name: "exact_fact_exists_matches_only_semantic_layer_exact_content",
        agent: "scenario-exact-fact-a",
        steps: vec![
            Step::Remember {
                agent: None, // scenario-exact-fact-a
                id: "sem",
                layer: MemoryLayer::Semantic,
                text: "Alice travaille chez Acme",
                vector_seed: 1,
                validity: Validity::since(0),
                source: "consolidation",
            },
            Step::Remember {
                agent: None,
                id: "ep",
                layer: MemoryLayer::Episodic,
                text: "Alice travaille chez Acme",
                vector_seed: 2,
                validity: Validity::since(0),
                source: "user",
            },
            Step::ExpectExactFactExists {
                agent: None,
                label: "le fait sémantique exact existe",
                content: "Alice travaille chez Acme",
                expect: true,
            },
            Step::ExpectExactFactExists {
                agent: None,
                label: "un contenu différent n'existe pas",
                content: "Bob travaille chez Beta",
                expect: false,
            },
            Step::ExpectExactFactExists {
                agent: Some("scenario-exact-fact-b"),
                label: "un autre agent ne voit pas le fait de A",
                content: "Alice travaille chez Acme",
                expect: false,
            },
        ],
    }
}
