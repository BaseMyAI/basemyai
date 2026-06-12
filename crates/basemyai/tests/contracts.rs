//! Contrats de la sémantique mémoire : logique temporelle, isolation, couches,
//! conversion d'erreur, et assemblage par injection de dépendance.

use basemyai::temporal::{Validity, temporal_filter};
use basemyai::{AgentId, Memory, MemoryError, MemoryLayer};
use basemyai_core::{Embedder, Result as CoreResult, Store};

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

#[test]
fn temporal_filter_is_parameterized_with_two_binds() {
    let f = temporal_filter(1_234);
    assert!(f.where_sql.contains("valid_until"));
    assert!(f.where_sql.contains('?'));
    assert_eq!(f.params.len(), 2);
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
    let store = Store::open_in_memory().await.expect("store opens");
    let agent = AgentId::new("assistant-42").expect("valid agent");
    let mem = Memory::new(store, Box::new(FakeEmbedder), agent.clone());
    assert_eq!(mem.agent(), &agent);
}

// ── Chiffrement obligatoire (ADR-007) ─────────────────────────────────────────

#[tokio::test]
async fn open_without_key_fails_for_file_store() {
    // Un store sur fichier sans clé de chiffrement doit être rejeté par Memory::open.
    // Les stores `:memory:` sont exemptés (éphémères, chiffrement sans objet).
    let path = std::env::temp_dir().join(format!("basemyai_enc_test_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path); // état propre

    let store = Store::open(&path, None).await.expect("store opens sans clé");
    let agent = AgentId::new("test-enc").expect("valid");
    let result = Memory::open(store, Box::new(FakeEmbedder), agent).await;

    let _ = std::fs::remove_file(&path); // nettoyage

    assert!(
        matches!(result, Err(basemyai::MemoryError::EncryptionRequired)),
        "Memory::open doit refuser un store fichier non chiffré"
    );
}

#[tokio::test]
async fn open_in_memory_store_bypasses_encryption_requirement() {
    // Les stores `:memory:` sont éphémères — Memory::open les accepte sans clé.
    let store = Store::open_in_memory().await.expect("store opens");
    let agent = AgentId::new("test-mem").expect("valid");
    Memory::open(store, Box::new(FakeEmbedder), agent)
        .await
        .expect("Memory::open accepte un store in-memory sans clé de chiffrement");
}
