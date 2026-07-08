//! Open a memory, remember text, recall it, then invalidate.
//!
//! Uses an in-memory store (no file, no encryption required) and a fake
//! embedder that returns zero-vectors — no model files needed.
//!
//! Run: `cargo run --example memory_basic -p basemyai`

use basemyai::{AgentId, Memory, MemoryLayer};
use basemyai_core::{Embedder, EncryptionKey};

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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let key = EncryptionKey::new("demo-key-do-not-use-in-prod");
    let agent = AgentId::new("demo-agent").expect("non-empty id");
    let memory = Memory::open_native(dir.path(), &key, Box::new(FakeEmbedder), agent).await?;

    memory
        .remember("The Eiffel Tower is in Paris.", MemoryLayer::Semantic)
        .await?;
    memory
        .remember("Paris is the capital of France.", MemoryLayer::Semantic)
        .await?;
    memory.remember("Bonjour!", MemoryLayer::ShortTerm).await?;

    println!("=== recall (top 2) ===");
    let results = memory.recall("What city is the Eiffel Tower in?", 2).await?;
    for r in &results {
        println!(
            "  [{layer}] {score:.3}  {text}",
            layer = r.layer.table(),
            score = r.score,
            text = r.text
        );
    }

    let first_id = results[0].id.clone();
    memory.invalidate(&first_id).await?;
    println!("\nInvalidated id={first_id}");

    let after = memory.recall("Eiffel Tower", 5).await?;
    println!("Recall after invalidation: {} result(s)", after.len());

    let stats = memory.stats().await?;
    println!(
        "\nStats: {} semantic, {} short-term (valid).",
        stats.semantic, stats.short_term
    );

    Ok(())
}
