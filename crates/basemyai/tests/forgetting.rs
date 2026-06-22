//! Tests d'intégration de l'oubli adaptatif (VISION §5.2). On insère des
//! souvenirs en SQL direct (pas besoin d'embedder : `emb` est *nullable*), on
//! lance la tâche [`AdaptiveForgetting`], puis on vérifie l'éviction par score
//! (importance + récence), l'isolation par agent et l'effet de la récence.

use basemyai::AdaptiveForgetting;
use basemyai_core::{MaintenanceTask, Store};

/// Insère un souvenir minimal pour `agent`, avec `importance` et `last_access`
/// donnés (`last_access` *nullable*). `emb` reste NULL (colonne nullable).
async fn insert(store: &Store, id: &str, agent: &str, importance: f64, last_access: Option<i64>, valid_from: i64) {
    let conn = store.connect();
    conn.execute(
        "INSERT INTO memory \
         (id, agent_id, layer, content, valid_from, valid_until, emb, importance, last_access) \
         VALUES (?1, ?2, 'semantic', ?1, ?3, NULL, NULL, ?4, ?5)",
        basemyai_core::libsql::params![id, agent, valid_from, importance, last_access],
    )
    .await
    .expect("insert souvenir");
    conn.execute(
        "INSERT INTO memory_fts (id, agent_id, content) VALUES (?1, ?2, ?1)",
        basemyai_core::libsql::params![id, agent],
    )
    .await
    .expect("insert souvenir fts");
}

/// Liste les `id` restants pour un agent donné, triés.
async fn remaining_ids(store: &Store, agent: &str) -> Vec<String> {
    let conn = store.connect();
    let mut rows = conn
        .query(
            "SELECT id FROM memory WHERE agent_id = ?1 ORDER BY id",
            basemyai_core::libsql::params![agent],
        )
        .await
        .expect("query ids");
    let mut ids = Vec::new();
    while let Some(row) = rows.next().await.expect("row") {
        ids.push(row.get::<String>(0).expect("id text"));
    }
    ids
}

async fn count_for(store: &Store, agent: &str) -> i64 {
    let conn = store.connect();
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM memory WHERE agent_id = ?1",
            basemyai_core::libsql::params![agent],
        )
        .await
        .expect("query count");
    let row = rows.next().await.expect("row").expect("une ligne count");
    row.get::<i64>(0).expect("count int")
}

async fn fts_count_for(store: &Store, id: &str, agent: &str) -> i64 {
    let conn = store.connect();
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM memory_fts WHERE id = ?1 AND agent_id = ?2",
            basemyai_core::libsql::params![id, agent],
        )
        .await
        .expect("query fts count");
    let row = rows.next().await.expect("row").expect("une ligne count");
    row.get::<i64>(0).expect("count int")
}

#[tokio::test]
async fn evicts_least_important_beyond_capacity() {
    let store = Store::open_in_memory().await.expect("open");
    store.migrate(&basemyai::schema()).await.expect("migrate");

    // 5 souvenirs, même récence (last_access identique) : seul l'importance
    // départage. Capacité = 3 => les 3 plus importants restent.
    let t = 1_000_i64;
    insert(&store, "m1", "a", 0.1, Some(t), t).await;
    insert(&store, "m2", "a", 0.9, Some(t), t).await;
    insert(&store, "m3", "a", 0.5, Some(t), t).await;
    insert(&store, "m4", "a", 0.7, Some(t), t).await;
    insert(&store, "m5", "a", 0.3, Some(t), t).await;

    let task = AdaptiveForgetting {
        capacity_per_agent: 3,
        recency_half_life_secs: 86_400,
    };
    task.run(&store).await.expect("run");

    assert_eq!(
        count_for(&store, "a").await,
        3,
        "la capacité par agent doit être respectée"
    );
    let kept = remaining_ids(&store, "a").await;
    // Les 3 plus importants : m2 (0.9), m4 (0.7), m3 (0.5).
    assert_eq!(
        kept,
        vec!["m2".to_string(), "m3".to_string(), "m4".to_string()],
        "doivent rester les plus importants"
    );
}

#[tokio::test]
async fn capacity_is_per_agent_isolated() {
    let store = Store::open_in_memory().await.expect("open");
    store.migrate(&basemyai::schema()).await.expect("migrate");

    let t = 2_000_i64;
    // Agent A : 4 souvenirs, capacité 2 => 2 évincés.
    insert(&store, "a1", "A", 0.1, Some(t), t).await;
    insert(&store, "a2", "A", 0.2, Some(t), t).await;
    insert(&store, "a3", "A", 0.3, Some(t), t).await;
    insert(&store, "a4", "A", 0.4, Some(t), t).await;
    // Agent B : 1 seul souvenir => intouché malgré l'éviction de A.
    insert(&store, "b1", "B", 0.01, Some(t), t).await;

    let task = AdaptiveForgetting {
        capacity_per_agent: 2,
        recency_half_life_secs: 86_400,
    };
    task.run(&store).await.expect("run");

    assert_eq!(count_for(&store, "A").await, 2, "A plafonné à 2");
    assert_eq!(
        count_for(&store, "B").await,
        1,
        "B ne doit pas être touché par l'éviction de A"
    );
    assert_eq!(remaining_ids(&store, "B").await, vec!["b1".to_string()]);
}

#[tokio::test]
async fn recency_breaks_ties_at_equal_importance() {
    let store = Store::open_in_memory().await.expect("open");
    store.migrate(&basemyai::schema()).await.expect("migrate");

    // Importance identique : la récence (last_access) départage. Capacité 1.
    // half_life court pour rendre l'écart de récence décisif.
    let recent = 10_000_i64;
    let old = 0_i64;
    insert(&store, "old", "a", 0.5, Some(old), old).await;
    insert(&store, "recent", "a", 0.5, Some(recent), old).await;

    // `now` proche de `recent` : "recent" a une récence ~1, "old" ~0.
    let task = AdaptiveForgetting {
        capacity_per_agent: 1,
        recency_half_life_secs: 3_600,
    };
    // On appelle run après avoir figé un now suffisamment grand via insertion :
    // la tâche utilise now_unix() (temps réel courant >> recent), donc l'écart
    // (now - recent) << (now - old) garde "recent".
    task.run(&store).await.expect("run");

    assert_eq!(count_for(&store, "a").await, 1, "capacité 1");
    assert_eq!(
        remaining_ids(&store, "a").await,
        vec!["recent".to_string()],
        "à importance égale, le souvenir au last_access le plus récent est conservé"
    );
}

#[tokio::test]
async fn evicts_matching_fts_rows_with_memory_rows() {
    let store = Store::open_in_memory().await.expect("open");
    store.migrate(&basemyai::schema()).await.expect("migrate");

    let t = 3_000_i64;
    insert(&store, "drop-me", "a", 0.1, Some(t), t).await;
    insert(&store, "keep-me", "a", 0.9, Some(t), t).await;

    let task = AdaptiveForgetting {
        capacity_per_agent: 1,
        recency_half_life_secs: 86_400,
    };
    task.run(&store).await.expect("run");

    assert_eq!(fts_count_for(&store, "drop-me", "a").await, 0);
    assert_eq!(fts_count_for(&store, "keep-me", "a").await, 1);
}
