//! Demonstrates `consolidate()`: episodic memories → semantic facts + graph.
//!
//! Uses a `FakeLlm` that returns a hard-coded JSON extraction — no real LLM
//! or model files needed.
//!
//! Run: `cargo run --example llm_consolidation -p basemyai`

use basemyai::{AgentId, LlmInference, Memory, MemoryLayer, consolidate};
use basemyai_core::{Embedder, Store};

struct FakeEmbedder;

impl Embedder for FakeEmbedder {
    fn embed(&self, _text: &str) -> basemyai_core::Result<Vec<f32>> {
        Ok(vec![0.0; 384])
    }
    fn embed_batch(&self, texts: &[String]) -> basemyai_core::Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![0.0; 384]).collect())
    }
    fn model_id(&self) -> &str {
        "fake-384"
    }
    fn dim(&self) -> usize {
        384
    }
}

struct FakeLlm;

#[async_trait::async_trait]
impl LlmInference for FakeLlm {
    async fn complete(&self, _prompt: &str) -> basemyai::Result<String> {
        Ok(r#"{
            "facts": ["Paris is the capital of France"],
            "entities": [
                {"id": "paris",  "kind": "city",    "label": "Paris"},
                {"id": "france", "kind": "country",  "label": "France"}
            ],
            "relations": [
                {"src": "paris", "relation": "capital_of", "dst": "france"}
            ]
        }"#
        .to_string())
    }

    fn model_id(&self) -> &str {
        "fake-llm"
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let store = Store::open_in_memory().await?;
    let agent = AgentId::new("consolidation-demo").expect("non-empty id");
    let memory = Memory::open(store, Box::new(FakeEmbedder), agent).await?;

    // Store a raw episode (what happened).
    memory
        .remember(
            "Alice attended a conference in Paris, the capital of France.",
            MemoryLayer::Episodic,
        )
        .await?;

    // Consolidate: extract facts + populate graph via FakeLlm.
    let report = consolidate(&memory, &FakeLlm).await?;
    println!(
        "Consolidation: {} episode(s) seen, {} fact(s) added, {} entity(ies) upserted, {} relation(s).",
        report.episodes_seen, report.facts_added, report.entities_upserted, report.relations_upserted,
    );

    // The extracted fact should now be searchable in semantic layer.
    let facts = memory
        .recall_by_layer("What is the capital of France?", MemoryLayer::Semantic, 5)
        .await?;
    println!("\nSemantic facts after consolidation ({}):", facts.len());
    for f in &facts {
        println!("  • {}", f.text);
    }

    Ok(())
}
