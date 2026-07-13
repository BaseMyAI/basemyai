//! Scaffold de tests **déclaratifs multi-backend** (N2,
//! `docs/TODO-NATIVE-ENGINE.md`, rationale : `docs/PLAN-NATIVE-ENGINE.md`
//! §3.2). Un [`Scenario`] décrit une séquence d'opérations mémoire
//! (remember/invalidate/graphe…) et les postconditions attendues, scopée à un
//! `agent` par défaut — chaque étape peut passer un `agent` différent (champ
//! `agent: Option<&'static str>`) pour les scénarios d'isolation
//! multi-agent (N5.3). [`run_scenario`] les rejoue contre **n'importe
//! quelle** implémentation de [`MemoryStore`] — la borne est générique
//! (`S: MemoryStore`), aucune implémentation concrète (`NativeMemoryStore`)
//! n'apparaît dans ce fichier.
//!
//! Le backend natif est rejoué en clair et chiffré — voir `../memory_tests.rs`
//! pour l'enregistrement des backends via `backend_suite!`.

pub(crate) mod scenarios;

use basemyai::storage::{MemoryStore, NewMemory};
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

/// Un souvenir pour [`Step::RememberBatch`] — mêmes champs que
/// [`Step::Remember`], regroupés pour un appel `put_memory_batch`.
pub(crate) struct BatchItem {
    pub id: &'static str,
    pub layer: MemoryLayer,
    pub text: &'static str,
    pub vector_seed: u8,
    pub validity: Validity,
    pub source: &'static str,
}

/// Une étape rejouée dans l'ordre par [`run_scenario`]. Les variantes
/// `Expect*` n'ont aucun effet de bord : elles interrogent le store et
/// comparent au résultat attendu, en panic-ant avec le nom du scénario et
/// l'étape en cause si ça ne correspond pas.
///
/// Chaque variante porte un champ `agent: Option<&'static str>` : `None`
/// utilise l'`agent` par défaut du [`Scenario`], `Some(id)` l'outrepasse pour
/// cette étape — c'est ce qui permet à un même scénario de rejouer des
/// séquences multi-agent (isolation ADR-006).
pub(crate) enum Step {
    /// `MemoryStore::put_memory`.
    Remember {
        agent: Option<&'static str>,
        id: &'static str,
        layer: MemoryLayer,
        text: &'static str,
        vector_seed: u8,
        validity: Validity,
        source: &'static str,
    },
    /// `MemoryStore::put_memory_batch`.
    RememberBatch {
        agent: Option<&'static str>,
        items: &'static [BatchItem],
    },
    /// `MemoryStore::invalidate`.
    Invalidate { id: &'static str, at: i64 },
    /// `MemoryStore::forget`.
    Forget { id: &'static str },
    /// `MemoryStore::purge_agent`.
    PurgeAgent { agent: Option<&'static str> },
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
        agent: Option<&'static str>,
        label: &'static str,
        query_seed: u8,
        k: usize,
        layer: Option<MemoryLayer>,
        now: i64,
        expect_ids: &'static [&'static str],
    },
    /// Postcondition sur `MemoryStore::recall_vector`, champ par champ
    /// (texte, couche) — distincte de [`Step::ExpectRecallVector`] pour ne
    /// pas alourdir les scénarios qui ne vérifient que les ids. Couvre la
    /// fidélité de round-trip `text`/`layer` (portée depuis
    /// `storage_contract.rs::put_memory_then_recall_vector_roundtrips`).
    ExpectRecallVectorFields {
        agent: Option<&'static str>,
        label: &'static str,
        query_seed: u8,
        k: usize,
        layer: Option<MemoryLayer>,
        now: i64,
        expect: &'static [(&'static str, &'static str, MemoryLayer)],
    },
    /// Postcondition sur `MemoryStore::vector_ranking_ids`.
    ExpectVectorRankingIds {
        agent: Option<&'static str>,
        label: &'static str,
        query_seed: u8,
        k: usize,
        now: i64,
        expect_ids: &'static [&'static str],
    },
    /// Postcondition sur `MemoryStore::graph_traverse`.
    ExpectGraphTraverse {
        agent: Option<&'static str>,
        label: &'static str,
        start: &'static str,
        max_depth: u32,
        now: i64,
        expect_ids: &'static [&'static str],
    },
    /// Postcondition sur `MemoryStore::agent_stats` (total toutes couches).
    ExpectAgentStats {
        agent: Option<&'static str>,
        label: &'static str,
        now: i64,
        expect_total: usize,
    },
    /// Postcondition sur `MemoryStore::agent_stats`, couche par couche —
    /// distincte de [`Step::ExpectAgentStats`] pour ne pas alourdir les
    /// scénarios qui ne vérifient que le total.
    ExpectAgentStatsByLayer {
        agent: Option<&'static str>,
        label: &'static str,
        now: i64,
        expect_short_term: usize,
        expect_episodic: usize,
        expect_procedural: usize,
        expect_semantic: usize,
    },
    /// Postcondition sur `MemoryStore::keyword_ranking_ids` (ADR-028) : les
    /// ids retournés doivent correspondre exactement (ordre compris) à
    /// `expect_ids`. `match_expr` est déjà dans le sous-ensemble que
    /// `fts_match_expr()` produit (tokens cités joints par ` OR ` littéral) —
    /// voir `scenarios.rs` pour pourquoi chaque scénario évite les
    /// ex-æquo BM25 (ordre de tri non garanti entre backends en cas d'égalité
    /// stricte de score).
    ExpectKeywordRankingIds {
        agent: Option<&'static str>,
        label: &'static str,
        match_expr: &'static str,
        k: usize,
        now: i64,
        expect_ids: &'static [&'static str],
    },
    /// Postcondition sur `MemoryStore::hydrate` : les ids retournés doivent
    /// correspondre exactement (ordre compris) à `expect_ids` — un id absent
    /// ou d'un autre agent est silencieusement omis (jamais une erreur).
    ExpectHydrate {
        agent: Option<&'static str>,
        label: &'static str,
        ids: &'static [&'static str],
        now: i64,
        expect_ids: &'static [&'static str],
    },
    /// Postcondition sur `MemoryStore::recent_episodes`.
    ExpectRecentEpisodes {
        agent: Option<&'static str>,
        label: &'static str,
        limit: usize,
        now: i64,
        expect_texts: &'static [&'static str],
    },
    /// Postcondition sur `MemoryStore::exact_fact_exists`.
    ExpectExactFactExists {
        agent: Option<&'static str>,
        label: &'static str,
        content: &'static str,
        now: i64,
        expect: bool,
    },
}

/// Un scénario : une séquence d'étapes, scopée par défaut à `agent` (isolation
/// ADR-006) — une étape peut outrepasser cet agent par défaut via son propre
/// champ `agent: Option<&'static str>`, ce qui permet de rejouer des
/// séquences multi-agent dans un seul scénario (N5.3). Deux scénarios
/// différents doivent utiliser des agents distincts s'ils partagent un
/// store, même si en pratique chaque backend enregistré via `backend_suite!`
/// ouvre un store frais par scénario.
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
/// appel au store échoue, si un id d'agent (par défaut ou surchargé) est
/// invalide, ou si une postcondition `Expect*` ne correspond pas.
pub(crate) async fn run_scenario<S: MemoryStore>(store: &S, scenario: &Scenario) {
    let resolve = |name: Option<&'static str>| -> AgentId {
        let raw = name.unwrap_or(scenario.agent);
        AgentId::new(raw).unwrap_or_else(|| panic!("[{}] agent id invalide: {raw}", scenario.name))
    };

    for (i, step) in scenario.steps.iter().enumerate() {
        match step {
            Step::Remember {
                agent,
                id,
                layer,
                text,
                vector_seed,
                validity,
                source,
            } => {
                let agent = resolve(*agent);
                store
                    .put_memory(id, &agent, *layer, text, *validity, &vec_for(*vector_seed), source, 1.0)
                    .await
                    .unwrap_or_else(|e| panic!("{}: put_memory a échoué: {e}", step_ctx(scenario.name, i, "remember")));
            }
            Step::RememberBatch { agent, items } => {
                let agent = resolve(*agent);
                let vectors: Vec<Vec<f32>> = items.iter().map(|it| vec_for(it.vector_seed)).collect();
                let news: Vec<NewMemory<'_>> = items
                    .iter()
                    .zip(&vectors)
                    .map(|(it, v)| NewMemory {
                        id: it.id.to_string(),
                        layer: it.layer,
                        text: it.text,
                        validity: it.validity,
                        vector: v.as_slice(),
                        source: it.source,
                        importance: 1.0,
                    })
                    .collect();
                store.put_memory_batch(&agent, &news).await.unwrap_or_else(|e| {
                    panic!(
                        "{}: put_memory_batch a échoué: {e}",
                        step_ctx(scenario.name, i, "remember_batch")
                    )
                });
            }
            Step::Invalidate { id, at } => {
                let agent = resolve(None);
                store.invalidate(&agent, id, *at).await.unwrap_or_else(|e| {
                    panic!("{}: invalidate a échoué: {e}", step_ctx(scenario.name, i, "invalidate"))
                });
            }
            Step::Forget { id } => {
                let agent = resolve(None);
                store
                    .forget(&agent, id)
                    .await
                    .unwrap_or_else(|e| panic!("{}: forget a échoué: {e}", step_ctx(scenario.name, i, "forget")));
            }
            Step::PurgeAgent { agent } => {
                let agent = resolve(*agent);
                store.purge_agent(&agent).await.unwrap_or_else(|e| {
                    panic!(
                        "{}: purge_agent a échoué: {e}",
                        step_ctx(scenario.name, i, "purge_agent")
                    )
                });
            }
            Step::GraphEntity {
                id,
                kind,
                label,
                validity,
            } => {
                let agent = resolve(None);
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
                let agent = resolve(None);
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
                agent,
                label,
                query_seed,
                k,
                layer,
                now,
                expect_ids,
            } => {
                let agent = resolve(*agent);
                let ctx = step_ctx(scenario.name, i, label);
                let got = store
                    .recall_vector(&agent, &vec_for(*query_seed), *k, *layer, Metric::Cosine, *now, true)
                    .await
                    .unwrap_or_else(|e| panic!("{ctx}: recall_vector a échoué: {e}"));
                let got_ids: Vec<&str> = got.iter().map(|r| r.id.as_str()).collect();
                assert_eq!(got_ids, *expect_ids, "{ctx}: recall_vector ids inattendus");
            }
            Step::ExpectRecallVectorFields {
                agent,
                label,
                query_seed,
                k,
                layer,
                now,
                expect,
            } => {
                let agent = resolve(*agent);
                let ctx = step_ctx(scenario.name, i, label);
                let got = store
                    .recall_vector(&agent, &vec_for(*query_seed), *k, *layer, Metric::Cosine, *now, true)
                    .await
                    .unwrap_or_else(|e| panic!("{ctx}: recall_vector a échoué: {e}"));
                let got_fields: Vec<(&str, &str, MemoryLayer)> =
                    got.iter().map(|r| (r.id.as_str(), r.text.as_str(), r.layer)).collect();
                let expect_fields: Vec<(&str, &str, MemoryLayer)> =
                    expect.iter().map(|(id, text, layer)| (*id, *text, *layer)).collect();
                assert_eq!(got_fields, expect_fields, "{ctx}: recall_vector champs inattendus");
            }
            Step::ExpectVectorRankingIds {
                agent,
                label,
                query_seed,
                k,
                now,
                expect_ids,
            } => {
                let agent = resolve(*agent);
                let ctx = step_ctx(scenario.name, i, label);
                let got = store
                    .vector_ranking_ids(&agent, &vec_for(*query_seed), *k, *now, true)
                    .await
                    .unwrap_or_else(|e| panic!("{ctx}: vector_ranking_ids a échoué: {e}"));
                let got_ids: Vec<&str> = got.iter().map(String::as_str).collect();
                assert_eq!(got_ids, *expect_ids, "{ctx}: vector_ranking_ids ids inattendus");
            }
            Step::ExpectGraphTraverse {
                agent,
                label,
                start,
                max_depth,
                now,
                expect_ids,
            } => {
                let agent = resolve(*agent);
                let ctx = step_ctx(scenario.name, i, label);
                let got = store
                    .graph_traverse(&agent, start, *max_depth, *now)
                    .await
                    .unwrap_or_else(|e| panic!("{ctx}: graph_traverse a échoué: {e}"));
                let got_ids: Vec<&str> = got.iter().map(|r| r.id.as_str()).collect();
                assert_eq!(got_ids, *expect_ids, "{ctx}: graph_traverse ids inattendus");
            }
            Step::ExpectAgentStats {
                agent,
                label,
                now,
                expect_total,
            } => {
                let agent = resolve(*agent);
                let ctx = step_ctx(scenario.name, i, label);
                let stats = store
                    .agent_stats(&agent, *now)
                    .await
                    .unwrap_or_else(|e| panic!("{ctx}: agent_stats a échoué: {e}"));
                assert_eq!(stats.total(), *expect_total, "{ctx}: total inattendu");
            }
            Step::ExpectAgentStatsByLayer {
                agent,
                label,
                now,
                expect_short_term,
                expect_episodic,
                expect_procedural,
                expect_semantic,
            } => {
                let agent = resolve(*agent);
                let ctx = step_ctx(scenario.name, i, label);
                let stats = store
                    .agent_stats(&agent, *now)
                    .await
                    .unwrap_or_else(|e| panic!("{ctx}: agent_stats a échoué: {e}"));
                assert_eq!(stats.short_term, *expect_short_term, "{ctx}: short_term inattendu");
                assert_eq!(stats.episodic, *expect_episodic, "{ctx}: episodic inattendu");
                assert_eq!(stats.procedural, *expect_procedural, "{ctx}: procedural inattendu");
                assert_eq!(stats.semantic, *expect_semantic, "{ctx}: semantic inattendu");
            }
            Step::ExpectKeywordRankingIds {
                agent,
                label,
                match_expr,
                k,
                now,
                expect_ids,
            } => {
                let agent = resolve(*agent);
                let ctx = step_ctx(scenario.name, i, label);
                let got = store
                    .keyword_ranking_ids(&agent, match_expr, *k, *now, true)
                    .await
                    .unwrap_or_else(|e| panic!("{ctx}: keyword_ranking_ids a échoué: {e}"));
                let got_ids: Vec<&str> = got.iter().map(String::as_str).collect();
                assert_eq!(got_ids, *expect_ids, "{ctx}: keyword_ranking_ids ids inattendus");
            }
            Step::ExpectHydrate {
                agent,
                label,
                ids,
                now,
                expect_ids,
            } => {
                let agent = resolve(*agent);
                let ctx = step_ctx(scenario.name, i, label);
                let owned_ids: Vec<String> = ids.iter().map(|s| (*s).to_string()).collect();
                let got = store
                    .hydrate(&agent, &owned_ids, *now)
                    .await
                    .unwrap_or_else(|e| panic!("{ctx}: hydrate a échoué: {e}"));
                let got_ids: Vec<&str> = got.iter().map(|r| r.id.as_str()).collect();
                assert_eq!(got_ids, *expect_ids, "{ctx}: hydrate ids inattendus");
            }
            Step::ExpectRecentEpisodes {
                agent,
                label,
                limit,
                now,
                expect_texts,
            } => {
                let agent = resolve(*agent);
                let ctx = step_ctx(scenario.name, i, label);
                let got = store
                    .recent_episodes(&agent, *limit, *now)
                    .await
                    .unwrap_or_else(|e| panic!("{ctx}: recent_episodes a échoué: {e}"));
                let got_texts: Vec<&str> = got.iter().map(String::as_str).collect();
                assert_eq!(got_texts, *expect_texts, "{ctx}: recent_episodes textes inattendus");
            }
            Step::ExpectExactFactExists {
                agent,
                label,
                content,
                now,
                expect,
            } => {
                let agent = resolve(*agent);
                let ctx = step_ctx(scenario.name, i, label);
                let got = store
                    .exact_fact_exists(&agent, content, *now)
                    .await
                    .unwrap_or_else(|e| panic!("{ctx}: exact_fact_exists a échoué: {e}"));
                assert_eq!(got, *expect, "{ctx}: exact_fact_exists inattendu");
            }
        }
    }
}
