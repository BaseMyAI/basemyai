// SPDX-License-Identifier: BUSL-1.1
//! Tests de panne I/O (N11.3, `PLAN-NATIVE-ENGINE.md` §8.2). La majorité de
//! la liste §8.2 est déjà couverte ailleurs — pas de duplication ici :
//! écriture courte / `ENOSPC` / erreur `fsync`/`rename` / arrêt pendant
//! compaction via `tests/failpoints.rs` (`Action::Error` à 8 frontières) et
//! `Action::Abort` via `tests/crash_consistency.rs` ; bit flip / lecture
//! tronquée via `tests/corruption_smoke.rs`.
//!
//! Ce fichier couvre les deux scénarios qui ne l'étaient pas :
//! - **accès refusé** : le fichier tmp cible (`*.sst.tmp`/`crypto.meta.tmp`)
//!   est rendu lecture-seule juste avant que le moteur tente d'écrire
//!   dessus — portable clair/chiffré via `std::fs::Permissions::set_readonly`
//!   (fonctionne aussi bien sous Windows, cible dev/CI de ce repo, que sous
//!   Unix, où ça retire les bits d'écriture).
//! - **fichier temporaire déjà présent** : un tmp périmé/garbage existe déjà
//!   au chemin exact que le moteur va écrire (crash antérieur qui a laissé
//!   un orphelin) — `BlockSstFile::write_new`/`crypto::write_meta` ouvrent
//!   toujours en `create(true).write(true).truncate(true)` (jamais
//!   `create_new`), donc l'écrasement doit être propre par construction ;
//!   ce fichier en est la preuve exécutable, pas une relecture du source.
//!
//! Un dernier test, `#[ignore]`d, affirme en complément de
//! `corruption_smoke.rs::deleted_sst_is_detected_once_catalog_lands`
//! l'assertion cible : `verify_store` (même `FullLogical`) devrait détecter
//! une SST vivante supprimée. Aujourd'hui aucun mode n'a de source
//! indépendante listant les SSTs attendues (R2/R3, invariant I4) — voir le
//! commentaire du test pour la portée exacte.

use std::path::{Path, PathBuf};

use basemyai_engine::{Engine, EngineError, EngineOptions, VerifyMode, verify_store};

const KEY: &[u8] = b"io fault injection key";

fn small_options() -> EngineOptions {
    EngineOptions {
        memtable_flush_threshold: 4,
        compaction_sst_threshold: 100, // jamais de compaction : les fichiers restent en place
        block_size: 256,
        ..EngineOptions::default()
    }
}

fn open(dir: &Path, encrypted: bool) -> Engine {
    if encrypted {
        Engine::open_encrypted_with_options(dir, KEY, small_options()).expect("open encrypted")
    } else {
        Engine::open_with_options(dir, small_options()).expect("open clear")
    }
}

fn reopen(dir: &Path, encrypted: bool) -> Engine {
    open(dir, encrypted)
}

fn sst_tmp_path(dir: &Path, id: u64) -> PathBuf {
    dir.join(format!("{id:020}.sst.tmp"))
}

fn crypto_meta_tmp_path(dir: &Path) -> PathBuf {
    dir.join("crypto.meta.tmp")
}

fn set_readonly(path: &Path, readonly: bool) {
    let mut perms = std::fs::metadata(path).expect("metadata").permissions();
    perms.set_readonly(readonly);
    std::fs::set_permissions(path, perms).expect("set_permissions");
}

// ── accès refusé ──

fn sst_flush_denied_by_readonly_tmp_file_is_typed_and_recovers(encrypted: bool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut engine = open(dir.path(), encrypted);
    engine.put(b"a", b"1").expect("put");

    // Le premier flush d'un store neuf écrit toujours l'id 0.
    let tmp = sst_tmp_path(dir.path(), 0);
    std::fs::write(&tmp, b"pre-existing, about to be locked").expect("seed tmp");
    set_readonly(&tmp, true);

    let err = engine.flush().expect_err("readonly tmp must block the flush");
    assert!(
        matches!(err, EngineError::Io { .. }),
        "expected typed Io error, got {err:?}"
    );
    // `Engine::flush` n'incrémente `next_sst_id` qu'après un `write_new`
    // réussi : un flush raté ne fait bouger ni l'id, ni le memtable, ni le WAL.
    assert_eq!(
        engine.get(b"a").expect("get"),
        Some(b"1".to_vec()),
        "memtable/WAL intacts après l'échec"
    );

    set_readonly(&tmp, false);
    engine.flush().expect("retry succeeds once the obstruction is lifted");
    assert_eq!(engine.get(b"a").expect("get"), Some(b"1".to_vec()));
    drop(engine);

    let reopened = reopen(dir.path(), encrypted);
    assert_eq!(
        reopened.get(b"a").expect("get"),
        Some(b"1".to_vec()),
        "flushed data survives reopen"
    );
}

#[test]
fn sst_flush_denied_by_readonly_tmp_file_is_typed_and_recovers_clear() {
    sst_flush_denied_by_readonly_tmp_file_is_typed_and_recovers(false);
}

#[test]
fn sst_flush_denied_by_readonly_tmp_file_is_typed_and_recovers_encrypted() {
    sst_flush_denied_by_readonly_tmp_file_is_typed_and_recovers(true);
}

#[test]
fn crypto_meta_rotation_denied_by_readonly_tmp_file_leaves_old_key_working_then_recovers() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut engine = open(dir.path(), true);
    engine.put(b"a", b"1").expect("put");

    let tmp = crypto_meta_tmp_path(dir.path());
    std::fs::write(&tmp, b"stale rotation attempt").expect("seed tmp");
    set_readonly(&tmp, true);

    let err = engine
        .rotate_key(b"new key")
        .expect_err("readonly tmp must block rotation");
    assert!(
        matches!(err, EngineError::Io { .. }),
        "expected typed Io error, got {err:?}"
    );
    // `rotate_key` ne mute jamais la DEK en mémoire (seulement son wrap sur
    // disque) : cette même instance reste pleinement utilisable après l'échec.
    assert_eq!(engine.get(b"a").expect("get"), Some(b"1".to_vec()));
    drop(engine);

    // `crypto.meta` sur disque n'a jamais été touché (l'écriture a échoué
    // avant le rename) : l'ancienne clé rouvre toujours.
    let engine = open(dir.path(), true);
    assert_eq!(engine.get(b"a").expect("get"), Some(b"1".to_vec()));
    drop(engine);

    set_readonly(&tmp, false);
    let mut engine = open(dir.path(), true);
    engine
        .rotate_key(b"new key")
        .expect("retry succeeds once the obstruction is lifted");
    drop(engine);

    Engine::open_encrypted_with_options(dir.path(), b"new key", small_options())
        .expect("new key opens after a successful rotation");
    let old_key_err = match Engine::open_encrypted_with_options(dir.path(), KEY, small_options()) {
        Err(e) => e,
        Ok(_) => panic!("old key must be rejected after a successful rotation"),
    };
    assert!(matches!(old_key_err, EngineError::WrongEncryptionKey { .. }));
}

// ── fichier temporaire déjà présent ──

fn sst_flush_overwrites_a_stale_orphan_tmp_file_cleanly(encrypted: bool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut engine = open(dir.path(), encrypted);
    engine.put(b"a", b"1").expect("put");
    engine.put(b"b", b"2").expect("put");

    // Plus gros que la charge utile réelle, pour attraper un bug de
    // troncature partielle (`OpenOptions::truncate(true)` devrait rendre ça
    // sans objet, mais c'est la preuve exécutable, pas une relecture du source).
    let tmp = sst_tmp_path(dir.path(), 0);
    std::fs::write(&tmp, vec![0xAAu8; 4096]).expect("seed stale orphan tmp");

    engine.flush().expect("flush overwrites the stale orphan cleanly");
    assert_eq!(engine.get(b"a").expect("get"), Some(b"1".to_vec()));
    assert_eq!(engine.get(b"b").expect("get"), Some(b"2".to_vec()));
    drop(engine);

    let reopened = reopen(dir.path(), encrypted);
    assert_eq!(reopened.get(b"a").expect("get"), Some(b"1".to_vec()));
    assert_eq!(reopened.get(b"b").expect("get"), Some(b"2".to_vec()));
}

#[test]
fn sst_flush_overwrites_a_stale_orphan_tmp_file_cleanly_clear() {
    sst_flush_overwrites_a_stale_orphan_tmp_file_cleanly(false);
}

#[test]
fn sst_flush_overwrites_a_stale_orphan_tmp_file_cleanly_encrypted() {
    sst_flush_overwrites_a_stale_orphan_tmp_file_cleanly(true);
}

#[test]
fn crypto_meta_rotation_overwrites_a_stale_orphan_tmp_file_cleanly() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut engine = open(dir.path(), true);
    engine.put(b"a", b"1").expect("put");

    let tmp = crypto_meta_tmp_path(dir.path());
    std::fs::write(&tmp, vec![0xAAu8; 4096]).expect("seed stale orphan tmp");

    engine
        .rotate_key(b"new key")
        .expect("rotation overwrites the stale orphan cleanly");
    drop(engine);

    let engine = Engine::open_encrypted_with_options(dir.path(), b"new key", small_options())
        .expect("new key opens after rotation despite the pre-existing tmp orphan");
    assert_eq!(engine.get(b"a").expect("get"), Some(b"1".to_vec()));
}

// ── manifest gap : verify n'y voit pas plus clair que open ──

/// Target-architecture assertion (ENGINE-TARGET-ARCHITECTURE.md §17/§20,
/// invariant I4), companion to
/// `corruption_smoke.rs::deleted_sst_is_detected_once_catalog_lands`: a
/// `verify_store` pass in maximum depth (`FullLogical`) should flag a
/// missing live SST as unhealthy.
///
/// **Currently `#[ignore]`d**: today no `verify` mode has an independent
/// manifest listing the SSTs it expects — the logical pass only ever
/// reconstructs the KV view from the SSTs *present* on disk, so it reports
/// `healthy: true` with no warning even though data is missing (the inverse
/// of the assertion below). Closing this needs the durable catalog (R2/R3
/// of the roadmap) — once that lands, drop the `#[ignore]` and this test
/// becomes the closing proof.
#[test]
#[ignore = "detection requires the durable catalog (ENGINE-TARGET-ARCHITECTURE.md R2/R3, invariant I4) — not implemented yet; un-ignore once verify_store flags a missing live SST"]
fn verify_full_logical_detects_a_deleted_sst_once_catalog_lands() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let mut engine = Engine::open_with_options(dir.path(), small_options()).expect("open");
        for i in 0..8u32 {
            engine.put(format!("k{i}").as_bytes(), b"v").expect("put");
        }
        engine.close().expect("close");
    }
    let sst = std::fs::read_dir(dir.path())
        .expect("read dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.extension().and_then(|e| e.to_str()) == Some("sst"))
        .expect("at least one SST");
    std::fs::remove_file(&sst).expect("delete a live SST");

    let report = verify_store(dir.path(), None, VerifyMode::FullLogical).expect("verify runs");
    assert!(
        !report.healthy,
        "a live SST deleted from disk must be flagged unhealthy once the durable catalog gives \
         verify an independent source to cross-check against — got {report:?}"
    );
}
