//! Provenance typée du graphe, anti-spoofing import et filtre recall
//! (ADR-045, AGENT-MEM-1).

#[path = "../support/mod.rs"]
mod support;

use basemyai::{ExtractedEntity, Extraction, Memory, MemoryLayer, apply_extraction};
use support::{FakeEmbedder, agent};

async fn open_memory(agent_id: &str) -> Memory {
    let store = support::open_native_store();
    Memory::from_native_store(std::sync::Arc::new(store), Box::new(FakeEmbedder), agent(agent_id))
        .await
        .expect("open memory")
}

fn minimal_header(agent: &str) -> String {
    format!(
        r#"{{"type":"header","format":"basemyai-export","version":1,"agent_id":"{agent}","embedding_model":"fake","embedding_dim":384,"exported_at":0}}"#
    )
}

/// AGENT-MEM-1's exact scenario: an entity reimported from a JSONL export
/// must never influence recall the way a directly-added entity does — even
/// though nothing in the import format lets it claim otherwise (there is no
/// `source` field on an `Entity` JSONL line to begin with — anti-spoof is
/// structural, not a runtime check that could be bypassed).
#[tokio::test]
async fn imported_entity_label_is_excluded_from_default_recall_but_a_user_entity_is_not() {
    let mem = open_memory("gprov-import").await;

    // A poisoned label reimported from a (possibly forged) export.
    let jsonl = format!(
        "{}\n{}",
        minimal_header("gprov-import"),
        r#"{"type":"entity","id":"acme-imported","kind":"org","label":"Acme Imported Corp","valid_from":0}"#
    );
    mem.import_jsonl(&jsonl).await.expect("import entity");
    mem.remember("Alice mentions Acme Imported Corp in passing", MemoryLayer::Semantic)
        .await
        .expect("remember mentioning the imported entity's label");

    let hits = mem
        .search_graph("Acme Imported Corp", 10)
        .await
        .expect("search_graph over the imported entity");
    assert!(
        hits.is_empty(),
        "an imported entity's label must not influence graph-filtered recall by default: {hits:?}"
    );

    // The same label, but attached to a directly-added (User) entity, must
    // work exactly as before — this isn't recall being broken, only import
    // being excluded.
    mem.graph()
        .add_entity("acme-direct", "org", "Acme Direct Corp")
        .await
        .expect("add direct entity");
    mem.remember("Bob mentions Acme Direct Corp explicitly", MemoryLayer::Semantic)
        .await
        .expect("remember mentioning the direct entity's label");

    let direct_hits = mem
        .search_graph("Acme Direct Corp", 10)
        .await
        .expect("search_graph over the direct entity");
    assert!(
        direct_hits.iter().any(|r| r.text.contains("Bob")),
        "a directly-added (User) entity must still influence graph-filtered recall: {direct_hits:?}"
    );
}

/// Re-importing an export where an entity id already exists locally must not
/// touch the existing (already-tagged) record — `entities_skipped`, same
/// idempotence contract as memories (ADR-032 §3).
#[tokio::test]
async fn reimporting_an_existing_entity_id_is_skipped_not_overwritten() {
    let mem = open_memory("gprov-reimport").await;
    mem.graph()
        .add_entity("stable-id", "org", "Original Label")
        .await
        .expect("seed direct entity");

    let jsonl = format!(
        "{}\n{}",
        minimal_header("gprov-reimport"),
        r#"{"type":"entity","id":"stable-id","kind":"org","label":"Spoofed Label","valid_from":0}"#
    );
    let report = mem.import_jsonl(&jsonl).await.expect("import over existing id");
    assert_eq!(
        report.entities, 0,
        "existing id must not be counted as freshly inserted"
    );
    assert_eq!(report.entities_skipped, 1);

    // The pre-existing (User-sourced) label still drives recall — proof the
    // import never touched the record at all, not just that a counter says so.
    mem.remember("Carol mentions Original Label", MemoryLayer::Semantic)
        .await
        .expect("remember");
    let hits = mem.search_graph("Original Label", 10).await.expect("search_graph");
    assert!(
        hits.iter().any(|r| r.text.contains("Carol")),
        "the original User-sourced entity must still be live and influence recall: {hits:?}"
    );
}

/// Entities created by the consolidation pipeline (`apply_extraction`) are
/// tagged `Consolidation`, not `User` — and, unlike `Import`, they still
/// participate in the default graph-filtered recall (only `Import` is
/// excluded by ADR-045 §3).
#[tokio::test]
async fn consolidation_entities_are_tagged_and_still_drive_recall() {
    let mem = open_memory("gprov-consolidation").await;

    let extraction = Extraction {
        facts: Vec::new(),
        entities: vec![ExtractedEntity {
            id: "beta-consolidated".to_string(),
            kind: "org".to_string(),
            label: "Beta Consolidated Inc".to_string(),
        }],
        relations: Vec::new(),
    };
    let report = apply_extraction(&mem, &extraction).await.expect("apply_extraction");
    assert_eq!(report.entities_upserted, 1);

    mem.remember("Dave mentions Beta Consolidated Inc", MemoryLayer::Semantic)
        .await
        .expect("remember");
    let hits = mem
        .search_graph("Beta Consolidated Inc", 10)
        .await
        .expect("search_graph over a consolidation-sourced entity");
    assert!(
        hits.iter().any(|r| r.text.contains("Dave")),
        "a Consolidation-sourced entity must still influence graph-filtered recall \
         (only Import is excluded by default): {hits:?}"
    );
}
