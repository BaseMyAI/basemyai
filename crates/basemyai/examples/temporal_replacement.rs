//! Demonstrates temporal replacement: an old fact is invalidated and the new
//! fact is the only one recalled.
//!
//! Run: `cargo run --example temporal_replacement -p basemyai`

use basemyai::{AgentId, Memory, MemoryLayer};
use basemyai_core::{Embedder, EncryptionKey};

const DIM: usize = 384;

struct DemoEmbedder;

impl DemoEmbedder {
    fn vec_for(text: &str) -> Vec<f32> {
        let mut v = vec![0.0_f32; DIM];
        for (i, b) in text.bytes().enumerate() {
            v[i % DIM] += f32::from(b) + 1.0;
        }
        v[0] += 1.0;
        v
    }
}

impl Embedder for DemoEmbedder {
    fn embed(&self, text: &str) -> basemyai_core::Result<Vec<f32>> {
        Ok(Self::vec_for(text))
    }

    fn embed_batch(&self, texts: &[String]) -> basemyai_core::Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| Self::vec_for(t)).collect())
    }

    fn model_id(&self) -> &str {
        "demo-deterministic"
    }

    fn dim(&self) -> usize {
        DIM
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let key = EncryptionKey::new("demo-key-do-not-use-in-prod");
    let agent = AgentId::new("temporal-demo").expect("non-empty id");
    let memory = Memory::open_native(dir.path(), &key, Box::new(DemoEmbedder), agent).await?;

    let old_id = memory
        .remember("The user is on the Free billing plan.", MemoryLayer::Semantic)
        .await?;

    println!("Initially remembered: Free plan ({old_id})");

    memory.invalidate(&old_id).await?;
    let new_id = memory
        .remember("The user is on the Pro billing plan.", MemoryLayer::Semantic)
        .await?;

    println!("Invalidated old fact and remembered: Pro plan ({new_id})");

    let hits = memory.recall_hybrid("current billing plan", 5).await?;
    println!("\nRecall for `current billing plan`:");
    for hit in &hits {
        println!("  [{layer}] {text}", layer = hit.layer.table(), text = hit.text);
    }

    assert!(
        hits.iter().any(|r| r.text.contains("Pro billing plan")),
        "the current fact should be recalled"
    );
    assert!(
        hits.iter().all(|r| !r.text.contains("Free billing plan")),
        "the invalidated fact should not be recalled"
    );

    Ok(())
}
