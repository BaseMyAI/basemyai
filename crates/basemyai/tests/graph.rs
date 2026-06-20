//! Tests d'intégration du graphe (Phase 2, VISION §4.1) : traversée multi-sauts
//! par CTE récursive, isolation par agent, exclusion temporelle, terminaison sur
//! cycle.

use basemyai::temporal::Validity;
use basemyai::{AgentId, Graph};
use basemyai_core::Store;
use std::path::PathBuf;

fn agent(id: &str) -> AgentId {
    AgentId::new(id).expect("non-empty agent id")
}

fn now() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).expect("clock").as_secs()).expect("fits i64")
}

fn temp_db_path(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("basemyai-{name}-{}-{}.db", std::process::id(), now()))
}

async fn migrated_store() -> Store {
    let store = Store::open_in_memory().await.expect("open");
    store.migrate(&basemyai::schema()).await.expect("migrate");
    store
}

async fn migrated_file_store(path: &std::path::Path) -> Store {
    let store = Store::open(path, None).await.expect("open file store");
    store.migrate(&basemyai::schema()).await.expect("migrate");
    store
}

#[tokio::test]
async fn traverses_multiple_hops() {
    let store = migrated_store().await;
    let g = Graph::new(&store, agent("a"));

    // Alice → (employeur) Acme → (a_racheté) Beta
    g.add_entity("alice", "person", "Alice").await.expect("alice");
    g.add_entity("acme", "company", "Acme").await.expect("acme");
    g.add_entity("beta", "company", "Beta").await.expect("beta");
    g.add_edge("alice", "employeur", "acme", 1.0).await.expect("edge1");
    g.add_edge("acme", "a_racheté", "beta", 1.0).await.expect("edge2");

    // Profondeur 1 : seulement Acme.
    let d1 = g.traverse("alice", 1).await.expect("traverse d1");
    assert_eq!(d1.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["acme"]);
    assert_eq!(d1[0].depth, 1);

    // Profondeur 2 : Acme (1) puis Beta (2).
    let d2 = g.traverse("alice", 2).await.expect("traverse d2");
    let ids: Vec<_> = d2.iter().map(|r| (r.id.as_str(), r.depth)).collect();
    assert_eq!(ids, [("acme", 1), ("beta", 2)]);
}

#[tokio::test]
async fn isolation_hides_other_agents_edges() {
    let store = migrated_store().await;
    let ga = Graph::new(&store, agent("A"));
    let gb = Graph::new(&store, agent("B"));

    // Même base : A construit un chemin, B ne doit rien voir.
    ga.add_entity("x", "thing", "X").await.expect("x");
    ga.add_entity("y", "thing", "Y").await.expect("y");
    ga.add_edge("x", "rel", "y", 1.0).await.expect("edge");

    let seen_by_b = gb.traverse("x", 3).await.expect("b traverse");
    assert!(seen_by_b.is_empty(), "B ne doit voir aucune entité/arête de A");
}

#[tokio::test]
async fn agents_can_reuse_same_graph_ids_without_conflict() {
    let store = migrated_store().await;
    let ga = Graph::new(&store, agent("A"));
    let gb = Graph::new(&store, agent("B"));

    ga.add_entity("alice", "person", "Alice A").await.expect("alice A");
    ga.add_entity("acme", "company", "Acme A").await.expect("acme A");
    ga.add_edge("alice", "works_at", "acme", 1.0).await.expect("edge A");

    gb.add_entity("alice", "person", "Alice B").await.expect("alice B");
    gb.add_entity("acme", "company", "Acme B").await.expect("acme B");
    gb.add_edge("alice", "works_at", "acme", 1.0).await.expect("edge B");

    let seen_by_a = ga.traverse("alice", 1).await.expect("A traverse");
    let seen_by_b = gb.traverse("alice", 1).await.expect("B traverse");

    assert_eq!(seen_by_a[0].label, "Acme A");
    assert_eq!(seen_by_b[0].label, "Acme B");
}

#[tokio::test]
async fn file_backed_same_store_isolates_graph_agents() {
    let path = temp_db_path("graph-isolation");
    let store_a = migrated_file_store(&path).await;
    let store_b = migrated_file_store(&path).await;
    let ga = Graph::new(&store_a, agent("A"));
    let gb = Graph::new(&store_b, agent("B"));

    ga.add_entity("alice", "person", "Alice A").await.expect("alice A");
    ga.add_entity("acme", "company", "Acme A").await.expect("acme A");
    ga.add_edge("alice", "works_at", "acme", 1.0).await.expect("edge A");

    gb.add_entity("alice", "person", "Alice B").await.expect("alice B");
    gb.add_entity("acme", "company", "Acme B").await.expect("acme B");
    gb.add_edge("alice", "works_at", "acme", 1.0).await.expect("edge B");

    let seen_by_a = ga.traverse("alice", 1).await.expect("A traverse");
    let seen_by_b = gb.traverse("alice", 1).await.expect("B traverse");

    assert_eq!(seen_by_a[0].label, "Acme A");
    assert_eq!(seen_by_b[0].label, "Acme B");
}

#[tokio::test]
async fn excludes_expired_entities_and_edges() {
    let store = migrated_store().await;
    let g = Graph::new(&store, agent("a"));
    let n = now();

    g.add_entity("root", "thing", "Root").await.expect("root");
    // Cible encore valide, atteinte par une arête encore valide.
    g.add_entity("live", "thing", "Live").await.expect("live");
    g.add_edge("root", "rel", "live", 1.0).await.expect("edge live");
    // Cible expirée : ne doit pas remonter même si une arête y mène.
    g.add_entity_with(
        "stale",
        "thing",
        "Stale",
        Validity {
            valid_from: n - 100,
            valid_until: Some(n - 10),
        },
    )
    .await
    .expect("stale");
    g.add_edge("root", "rel", "stale", 1.0).await.expect("edge stale");

    let reached = g.traverse("root", 2).await.expect("traverse");
    let ids: Vec<_> = reached.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["live"], "l'entité expirée ne doit pas apparaître");
}

#[tokio::test]
async fn terminates_on_cycle() {
    let store = migrated_store().await;
    let g = Graph::new(&store, agent("a"));

    // Cycle a → b → a : la traversée bornée doit terminer, pas boucler.
    g.add_entity("a1", "thing", "A1").await.expect("a1");
    g.add_entity("b1", "thing", "B1").await.expect("b1");
    g.add_edge("a1", "rel", "b1", 1.0).await.expect("e1");
    g.add_edge("b1", "rel", "a1", 1.0).await.expect("e2");

    let reached = g.traverse("a1", 5).await.expect("traverse cycle");
    // a1 exclu (départ), b1 atteint à profondeur 1.
    assert_eq!(reached.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["b1"]);
    assert_eq!(reached[0].depth, 1);
}
