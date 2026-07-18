// SPDX-License-Identifier: BUSL-1.1
//! Contrats pré-implémentation ADR-042 (PR2).
//!
//! PR3 livre les primitives attendues : ces scénarios doivent désormais
//! s'exécuter dans le gate normal et rester verts.

use std::path::{Path, PathBuf};

use basemyai_engine::Engine;

#[test]
fn second_writable_open_is_refused_while_the_first_is_live() {
    let dir = tempfile::tempdir().expect("tempdir");
    let _first = Engine::open_encrypted(dir.path(), b"first key").expect("first writer opens");
    let second = Engine::open_encrypted(dir.path(), b"first key");
    assert!(
        second.is_err(),
        "a second writer must be rejected before it can touch wal.log"
    );
}

#[test]
fn secret_ownership_is_enforced_at_generation_and_persistence_boundaries() {
    let root = workspace_root();
    let key_source = compact_source(&root.join("crates/basemyai-core/src/storage/key.rs"));
    let config_source = compact_source(&root.join("crates/basemyai-cli/src/commands/config_key.rs"));

    assert!(
        key_source.contains("pubfngenerate_passphrase()->Self"),
        "generated credentials must return the zeroizing EncryptionKey wrapper"
    );
    assert!(
        key_source.contains("pubfnpersist_to_default_file(key:&Self"),
        "the persistence API must only accept the zeroizing EncryptionKey wrapper"
    );
    assert!(
        key_source.contains("Zeroizing::new(String::with_capacity("),
        "the newline persistence buffer must itself be zeroized"
    );
    assert!(
        !config_source.contains(".expose()"),
        "config key generation must pass the protected wrapper directly"
    );
}

fn compact_source(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(PathBuf::from)
        .expect("workspace root")
}

#[test]
fn passphrase_store_never_accepts_the_same_bytes_as_a_raw_key() {
    let dir = tempfile::tempdir().expect("tempdir");
    let passphrase = b"same bytes, distinct credential mode";
    drop(Engine::open_with_passphrase(dir.path(), passphrase).expect("create Argon2id store"));

    let raw = Engine::open_encrypted(dir.path(), passphrase);
    assert!(
        matches!(raw, Err(basemyai_engine::EngineError::WrongEncryptionKey { .. })),
        "an Argon2id store must not silently fall back to RawKey"
    );
    Engine::open_with_passphrase(dir.path(), passphrase).expect("same passphrase mode reopens");
}
