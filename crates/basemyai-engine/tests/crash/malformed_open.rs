//! Ouverture d'un répertoire store corrompu : erreur typée, pas de panic.

use basemyai_engine::format::wal::{self, WalOp};
use basemyai_engine::{Engine, EngineError};
use tempfile::tempdir;

#[test]
fn corrupt_wal_record_returns_error_not_panic() {
    let dir = tempdir().expect("tempdir");
    Engine::open(dir.path()).expect("create empty store");

    let mut bytes = wal::encode(WalOp::Put, 0, b"key", Some(b"value"));
    let last = bytes.len() - 1;
    bytes[last] ^= 0xFF;
    std::fs::write(dir.path().join("wal.log"), bytes).expect("write corrupt wal");

    let err = match Engine::open(dir.path()) {
        Err(e) => e,
        Ok(_) => panic!("corrupt wal must fail"),
    };
    assert!(
        matches!(err, EngineError::CorruptWal { .. }),
        "expected CorruptWal, got {err:?}"
    );
}

#[test]
fn wrong_encryption_key_fails_fast() {
    let dir = tempdir().expect("tempdir");
    {
        let mut engine = Engine::open_encrypted(dir.path(), b"correct-key").expect("open encrypted");
        engine.put(b"k", b"v").expect("put");
        engine.close().expect("close");
    }

    let err = match Engine::open_encrypted(dir.path(), b"wrong-key") {
        Err(e) => e,
        Ok(_) => panic!("wrong key must fail"),
    };
    assert!(
        matches!(err, EngineError::WrongEncryptionKey { .. }),
        "expected WrongEncryptionKey, got {err:?}"
    );
}
