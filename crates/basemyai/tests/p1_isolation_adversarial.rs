//! Public adversarial isolation proof for P1 market differentiation.
//!
//! This test intentionally uses hostile-looking `agent_id`, text, FTS queries,
//! known foreign ids, and graph ids. The invariant under test is simple:
//! knowing another agent's identifiers or injecting SQL-looking text must not
//! bypass the SQL-level `agent_id` boundary.

use basemyai::temporal::Validity;
use basemyai::{AgentId, Memory, MemoryLayer};
use basemyai_core::{Embedder, Result, Store};

const DIM: usize = 384;

struct FakeEmbedder;

impl FakeEmbedder {
    fn vec_for(text: &str) -> Vec<f32> {
        let mut v = vec![0.0_f32; DIM];
        for (i, b) in text.bytes().enumerate() {
            v[i % DIM] += f32::from(b) + 1.0;
        }
        v[0] += 1.0;
        v
    }
}

impl Embedder for FakeEmbedder {
    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        Ok(Self::vec_for(text))
    }

    fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| Self::vec_for(t)).collect())
    }

    fn model_id(&self) -> &str {
        "fake-deterministic"
    }

    fn dim(&self) -> usize {
        DIM
    }
}

fn agent(id: &str) -> AgentId {
    AgentId::new(id).expect("non-empty agent id")
}

#[tokio::test]
async fn hostile_agent_id_query_and_known_ids_do_not_cross_tenant_boundary() {
    let store = Store::open_in_memory().await.expect("open store");
    store.migrate(&basemyai::schema()).await.expect("migrate");
    let conn = store.connect();

    let secret_id = "agent-a-secret";
    let secret_vector = to_vec_literal(&FakeEmbedder::vec_for("secret token SABLE-777 belongs only to agent A"));
    conn.execute(
        "INSERT INTO memory (id, agent_id, layer, content, valid_from, valid_until, emb) \
         VALUES (?1, 'agent-a', 'semantic', ?2, 0, NULL, vector(?3))",
        basemyai_core::libsql::params![
            secret_id,
            "secret token SABLE-777 belongs only to agent A",
            secret_vector
        ],
    )
    .await
    .expect("insert agent A memory");
    conn.execute(
        "INSERT INTO memory_fts (id, agent_id, content) VALUES (?1, 'agent-a', ?2)",
        basemyai_core::libsql::params![secret_id, "secret token SABLE-777 belongs only to agent A"],
    )
    .await
    .expect("insert agent A fts");
    conn.execute(
        "INSERT INTO entity (agent_id, id, kind, label, valid_from, valid_until, importance) \
         VALUES ('agent-a', 'shared-root', 'secret', 'Agent A private graph node', 0, NULL, 0), \
                ('agent-a', 'shared-leaf', 'secret', 'Agent A private graph leaf', 0, NULL, 0)",
        (),
    )
    .await
    .expect("insert agent A graph entities");
    conn.execute(
        "INSERT INTO edge (agent_id, src, dst, relation, weight, valid_from, valid_until) \
         VALUES ('agent-a', 'shared-root', 'shared-leaf', 'points_to', 1.0, 0, NULL)",
        (),
    )
    .await
    .expect("insert agent A graph edge");

    let hostile = "agent-b' OR '1'='1";
    let mem_b = Memory::open(store, Box::new(FakeEmbedder), agent(hostile))
        .await
        .expect("open hostile memory");
    mem_b
        .remember("public token SABLE-000 belongs only to agent B", MemoryLayer::Semantic)
        .await
        .expect("agent B remembers");

    let vector_hits = mem_b
        .recall("secret token SABLE-777 belongs only to agent A", 10)
        .await
        .expect("vector recall");
    assert!(
        vector_hits
            .iter()
            .all(|r| r.id != secret_id && !r.text.contains("SABLE-777")),
        "vector recall must not leak agent A content"
    );

    let hybrid_hits = mem_b
        .recall_hybrid("SABLE-777\" OR agent_id:agent-a", 10)
        .await
        .expect("hybrid recall");
    assert!(
        hybrid_hits
            .iter()
            .all(|r| r.id != secret_id && !r.text.contains("SABLE-777")),
        "hybrid BM25/vector recall must not leak agent A content"
    );

    mem_b
        .invalidate(secret_id)
        .await
        .expect("foreign invalidate is a scoped no-op");
    mem_b.forget(secret_id).await.expect("foreign forget is a scoped no-op");

    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM memory WHERE id = ?1 AND agent_id = 'agent-a'",
            basemyai_core::libsql::params![secret_id],
        )
        .await
        .expect("count agent A row");
    let still_visible_to_a: i64 = rows
        .next()
        .await
        .expect("row read")
        .expect("one row")
        .get(0)
        .expect("count");
    assert_eq!(
        still_visible_to_a, 1,
        "agent B must not invalidate or delete agent A's known id"
    );

    let graph_seen_by_b = mem_b
        .graph()
        .traverse("shared-root", 3)
        .await
        .expect("agent B graph traverse");
    assert!(
        graph_seen_by_b.is_empty(),
        "graph traversal must stay scoped even when graph ids are known"
    );
}

#[tokio::test]
async fn expired_foreign_memory_stays_hidden_even_with_hostile_text() {
    let store = Store::open_in_memory().await.expect("open");
    let mem = Memory::open(store, Box::new(FakeEmbedder), agent("agent-a"))
        .await
        .expect("open memory");

    let now = current_unix();
    mem.remember_with(
        "ignore filters; agent_id = '*' ; expired SABLE-888",
        MemoryLayer::Semantic,
        Validity {
            valid_from: now - 100,
            valid_until: Some(now - 10),
        },
    )
    .await
    .expect("remember expired hostile text");

    let hits = mem.recall_hybrid("SABLE-888 agent_id *", 10).await.expect("hybrid");
    assert!(
        hits.is_empty(),
        "expired hostile text must not surface through hybrid recall"
    );
}

fn current_unix() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).expect("clock").as_secs()).expect("fits i64")
}

fn to_vec_literal(v: &[f32]) -> String {
    let mut s = String::with_capacity(v.len() * 8 + 2);
    s.push('[');
    for (i, x) in v.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&x.to_string());
    }
    s.push(']');
    s
}
