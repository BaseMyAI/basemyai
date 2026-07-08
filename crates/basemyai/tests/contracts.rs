//! Contrats de la sémantique mémoire : logique temporelle, isolation, couches,
//! conversion d'erreur, et assemblage par injection de dépendance.

use basemyai::temporal::Validity;
use basemyai::{AgentId, Memory, MemoryError, MemoryLayer};
use basemyai_core::{Embedder, Result as CoreResult};
mod support;

// ── Double de test : Embedder synthétique (sync) pour la DI. ──

struct FakeEmbedder;
impl Embedder for FakeEmbedder {
    fn embed(&self, _text: &str) -> CoreResult<Vec<f32>> {
        Ok(vec![0.0; 384])
    }
    fn embed_batch(&self, texts: &[String]) -> CoreResult<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![0.0; 384]).collect())
    }
    fn model_id(&self) -> &str {
        "fake-embedder"
    }
    fn dim(&self) -> usize {
        384
    }
}

// ── Temporal RAG (ADR-005) ──

#[test]
fn validity_without_expiry_is_always_valid_from_start() {
    let v = Validity::since(100);
    assert!(!v.is_valid_at(99));
    assert!(v.is_valid_at(100));
    assert!(v.is_valid_at(10_000));
}

#[test]
fn validity_with_expiry_is_exclusive_on_until() {
    let v = Validity {
        valid_from: 100,
        valid_until: Some(200),
    };
    assert!(!v.is_valid_at(50));
    assert!(v.is_valid_at(150));
    assert!(!v.is_valid_at(200)); // borne haute exclusive
}

// ── Isolation (ADR-006) & couches (ADR-004) ──

#[test]
fn agent_id_rejects_empty() {
    assert!(AgentId::new("").is_none());
    assert!(AgentId::new("assistant-42").is_some());
}

#[test]
fn memory_layers_map_to_table_names() {
    assert_eq!(MemoryLayer::ShortTerm.table(), "short_term");
    assert_eq!(MemoryLayer::Episodic.table(), "episodic");
    assert_eq!(MemoryLayer::Procedural.table(), "procedural");
    assert_eq!(MemoryLayer::Semantic.table(), "semantic");
}

// ── Erreurs ──

#[test]
fn core_error_converts_into_memory_error() {
    let mem: MemoryError = basemyai_core::CoreError::Encryption.into();
    assert!(matches!(mem, MemoryError::Core(_)));
}

// ── Injection de dépendance : Memory s'assemble depuis les primitives du core ──

#[tokio::test]
async fn memory_assembles_via_dependency_injection() {
    let store = std::sync::Arc::new(support::open_native_store());
    let agent = AgentId::new("assistant-42").expect("valid agent");
    let mem = Memory::from_native_store(store, Box::new(FakeEmbedder), agent.clone())
        .await
        .expect("memory opens");
    assert_eq!(mem.agent(), &agent);
}
