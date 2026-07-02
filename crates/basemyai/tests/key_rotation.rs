//! Rotation de clé de chiffrement (`Memory::rotate_key`, feature `crypto`) :
//! roundtrip complet à travers la façade `Memory`, pas seulement le `Store`
//! du core (couvert séparément dans `basemyai-core/tests/store.rs`).

#![cfg(feature = "crypto")]

use basemyai::{AgentId, Memory, MemoryLayer};
use basemyai_core::{Embedder, EncryptionKey, Result, Store};

const DIM: usize = 384;

/// Embedder déterministe (cf. `porting.rs`/`memory.rs`) : pas de Candle ici,
/// seul le roundtrip chiffrement/rotation est sous test.
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

fn temp_db_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("basemyai-key-rotation-{name}-{}.db", std::process::id()))
}

fn cleanup(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(path.with_extension("db-wal"));
    let _ = std::fs::remove_file(path.with_extension("db-shm"));
}

/// Rotation en place via `Memory::rotate_key` : la donnée mémorisée avant
/// rotation reste intacte sous la nouvelle clé, l'ancienne clé n'ouvre plus
/// rien. La `Memory` ayant exécuté la rotation est laissée tomber ensuite
/// (cf. doc `rotate_key` : le pool de lecteurs devient caduc), une nouvelle
/// `Memory::open` est requise pour continuer à utiliser le fichier.
#[tokio::test]
async fn rotate_key_preserves_data_and_invalidates_old_key() {
    let path = temp_db_path("roundtrip");
    cleanup(&path);

    let remembered_id = {
        let store = Store::open(&path, Some(EncryptionKey::new("old-passphrase")))
            .await
            .expect("open encrypted store");
        let memory = Memory::open(store, Box::new(FakeEmbedder), agent("rotate-agent"))
            .await
            .expect("open memory");

        let id = memory
            .remember("the moon is made of rock", MemoryLayer::Semantic)
            .await
            .expect("remember");

        memory
            .rotate_key(EncryptionKey::new("new-passphrase"))
            .await
            .expect("rotate_key succeeds");

        id
        // `memory`/`store` dropped here — must not be reused post-rotation.
    };

    // L'ancienne clé ne doit plus permettre d'ouvrir la mémoire : soit
    // `Store::open` échoue directement (header illisible), soit il réussit
    // mais `Memory::open` échoue à la migration (page de donnée illisible) —
    // les deux comportements sont observés selon l'état du fichier, ce test
    // n'exige que « aucune des deux étapes ne produit une mémoire utilisable ».
    {
        let usable = match Store::open(&path, Some(EncryptionKey::new("old-passphrase"))).await {
            Ok(store) => Memory::open(store, Box::new(FakeEmbedder), agent("rotate-agent"))
                .await
                .is_ok(),
            Err(_) => false,
        };
        assert!(!usable, "l'ancienne clé ne doit plus déchiffrer la mémoire après rotation");
    }

    // La nouvelle clé rouvre la mémoire et retrouve le souvenir intact.
    {
        let store = Store::open(&path, Some(EncryptionKey::new("new-passphrase")))
            .await
            .expect("reopen with new key");
        let memory = Memory::open(store, Box::new(FakeEmbedder), agent("rotate-agent"))
            .await
            .expect("open memory with new key");

        let hits = memory.recall("the moon is made of rock", 5).await.expect("recall");
        assert!(
            hits.iter().any(|r| r.id == remembered_id),
            "le souvenir mémorisé avant rotation doit rester lisible sous la nouvelle clé"
        );
    }

    cleanup(&path);
}

/// `rotate_key` propage l'erreur du core (`CoreError::Encryption`) quand le
/// store sous-jacent n'est pas chiffré — inatteignable via `Memory::open`
/// normalement (chiffrement obligatoire sur fichier, ADR-007), donc exercé
/// ici via le store de test non chiffré (`test-util`).
#[cfg(feature = "test-util")]
#[tokio::test]
async fn rotate_key_on_unencrypted_memory_fails() {
    let path = temp_db_path("unencrypted");
    cleanup(&path);

    let memory = Memory::open_test_file(&path, "rotate-agent-plain")
        .await
        .expect("open unencrypted test file");

    let err = memory
        .rotate_key(EncryptionKey::new("whatever"))
        .await
        .expect_err("rotate_key on an unencrypted store must fail");
    assert!(matches!(
        err,
        basemyai::MemoryError::Core(basemyai_core::CoreError::Encryption)
    ));

    cleanup(&path);
}
