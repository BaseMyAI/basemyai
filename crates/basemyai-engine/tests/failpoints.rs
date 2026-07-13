// SPDX-License-Identifier: BUSL-1.1
//! N7.4 — injection d'erreurs aux frontières de durabilité (mode `error` ;
//! le mode `abort` est exercé par le harnais kill-loop, pas ici). Chaque
//! test vérifie deux choses : l'appel moteur échoue **typé** exactement à la
//! frontière visée, et le store **rouvre proprement** derrière — l'état
//! laissé par la panne est toujours un état valide.
//!
//! Le registre failpoint est global au process : tous les tests de ce
//! fichier se sérialisent sur [`LOCK`] et désarment leurs points en sortie
//! (y compris en cas d'échec, via le guard).

use std::sync::{Mutex, MutexGuard, OnceLock};

use basemyai_engine::failpoint::{self, Action};
use basemyai_engine::{Engine, EngineError, EngineOptions};

fn lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    // Un test précédent qui a paniqué avec le verrou tenu ne doit pas
    // empoisonner les suivants : l'état partagé réel (le registre) est
    // toujours remis à zéro par le guard ci-dessous.
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Désarme tout à la sortie du scope, même si le test panique.
struct ClearOnDrop;
impl Drop for ClearOnDrop {
    fn drop(&mut self) {
        failpoint::clear_all();
    }
}

fn small_options() -> EngineOptions {
    EngineOptions {
        memtable_flush_threshold: 4,
        compaction_sst_threshold: 2,
        block_size: 256,
        ..EngineOptions::default()
    }
}

fn assert_injected_io(err: &EngineError, site: &str) {
    match err {
        EngineError::Io { path, .. } => {
            assert_eq!(path.to_string_lossy(), format!("failpoint:{site}"));
        }
        other => panic!("expected injected Io at `{site}`, got {other:?}"),
    }
}

#[test]
fn wal_append_boundaries_fail_typed_and_store_reopens() {
    let _serial = lock();
    let _clear = ClearOnDrop;
    for site in ["after_wal_append", "after_wal_fsync"] {
        failpoint::clear_all();
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let mut engine = Engine::open(dir.path()).expect("open");
            engine.put(b"before", b"1").expect("put before arming");
            failpoint::set(site, Action::Error);
            let err = engine.put(b"during", b"2").expect_err("armed put must fail");
            assert_injected_io(&err, site);
        }
        failpoint::clear_all();
        // The record's bytes are on disk in both cases (the failure fires
        // after write_all, resp. after fsync) — replay must surface it, and
        // the pre-failure record is untouched.
        let engine = Engine::open(dir.path()).expect("reopen");
        assert_eq!(engine.get(b"before").expect("get").as_deref(), Some(&b"1"[..]));
        assert_eq!(engine.get(b"during").expect("get").as_deref(), Some(&b"2"[..]));
    }
}

#[test]
fn sst_write_boundaries_fail_typed_and_wal_still_holds_the_data() {
    let _serial = lock();
    let _clear = ClearOnDrop;
    for site in ["after_sst_tmp_write", "after_sst_tmp_fsync", "after_sst_rename"] {
        failpoint::clear_all();
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let mut engine = Engine::open_with_options(dir.path(), small_options()).expect("open");
            for i in 0..3u32 {
                engine.put(format!("k{i}").as_bytes(), b"v").expect("put");
            }
            failpoint::set(site, Action::Error);
            // The 4th put crosses the flush threshold → SST write path.
            let err = engine.put(b"k3", b"v").expect_err("armed flush must fail");
            assert_injected_io(&err, site);
        }
        failpoint::clear_all();
        // Whatever happened to the SST file, the WAL was NOT truncated (the
        // failure fires before `before_wal_truncate`'s site) — every put,
        // including the one whose flush failed, must replay.
        let engine = Engine::open_with_options(dir.path(), small_options()).expect("reopen");
        for i in 0..4u32 {
            assert_eq!(
                engine.get(format!("k{i}").as_bytes()).expect("get").as_deref(),
                Some(&b"v"[..]),
                "site {site}: k{i} must survive via WAL replay"
            );
        }
    }
}

#[test]
fn before_wal_truncate_leaves_sst_and_wal_coexisting_without_duplication() {
    let _serial = lock();
    let _clear = ClearOnDrop;
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let mut engine = Engine::open_with_options(dir.path(), small_options()).expect("open");
        for i in 0..3u32 {
            engine.put(format!("k{i}").as_bytes(), b"v").expect("put");
        }
        failpoint::set("before_wal_truncate", Action::Error);
        let err = engine.put(b"k3", b"v").expect_err("armed flush must fail");
        assert_injected_io(&err, "before_wal_truncate");
    }
    failpoint::clear_all();
    // The SST is durable AND the WAL still holds the same records — replay
    // over the SST is idempotent (same keys, same values), never duplicated.
    let engine = Engine::open_with_options(dir.path(), small_options()).expect("reopen");
    let all = engine.scan_prefix(b"k").expect("scan");
    assert_eq!(all.len(), 4, "exactly k0..k3, no duplicates from SST+WAL overlap");
}

#[test]
fn during_compaction_failure_keeps_every_pre_compaction_sst_readable() {
    let _serial = lock();
    let _clear = ClearOnDrop;
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let mut engine = Engine::open_with_options(dir.path(), small_options()).expect("open");
        failpoint::set("during_compaction", Action::Error);
        let mut saw_injection = false;
        for i in 0..20u32 {
            match engine.put(format!("key-{i:03}").as_bytes(), b"v") {
                Ok(()) => {}
                Err(err) => {
                    assert_injected_io(&err, "during_compaction");
                    saw_injection = true;
                }
            }
        }
        assert!(saw_injection, "compaction threshold 2 must have been crossed");
    }
    failpoint::clear_all();
    let engine = Engine::open_with_options(dir.path(), small_options()).expect("reopen");
    for i in 0..20u32 {
        assert_eq!(
            engine.get(format!("key-{i:03}").as_bytes()).expect("get").as_deref(),
            Some(&b"v"[..]),
            "key-{i:03} must survive an aborted compaction"
        );
    }
}

#[test]
fn after_crypto_meta_write_failure_still_leaves_a_committed_wrap() {
    let _serial = lock();
    let _clear = ClearOnDrop;
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let mut engine = Engine::open_encrypted(dir.path(), b"old key").expect("open");
        engine.put(b"a", b"1").expect("put");
        failpoint::set("after_crypto_meta_write", Action::Error);
        // The rename already happened when the failure fires: the rotation
        // IS committed even though the call reports an error — exactly the
        // window an operator retry must tolerate.
        let err = engine.rotate_key(b"new key").expect_err("armed rotate must fail");
        assert_injected_io(&err, "after_crypto_meta_write");
    }
    failpoint::clear_all();
    let Err(err) = Engine::open_encrypted(dir.path(), b"old key") else {
        panic!("old key must be out")
    };
    assert!(matches!(err, EngineError::WrongEncryptionKey { .. }));
    let engine = Engine::open_encrypted(dir.path(), b"new key").expect("new key opens");
    assert_eq!(engine.get(b"a").expect("get").as_deref(), Some(&b"1"[..]));
}

/// Helper, jamais exécuté directement (`#[ignore]`) : relancé en process
/// enfant par le test ci-dessous avec `BASEMYAI_FAILPOINTS` posé — le
/// registre d'un process *neuf* doit être armé depuis l'env seul.
#[test]
#[ignore = "child-process helper for env_configuration_arms_failpoints_in_a_fresh_process"]
fn helper_child_env_armed_site_injects() {
    let err = failpoint::hit("env_armed_site").expect_err("env must have armed this site");
    assert_injected_io(&err, "env_armed_site");
    assert!(
        failpoint::hit("some_other_site").is_ok(),
        "only the named site is armed"
    );
}

#[test]
fn env_configuration_arms_failpoints_in_a_fresh_process() {
    let exe = std::env::current_exe().expect("test exe");
    let status = std::process::Command::new(exe)
        .args(["--exact", "helper_child_env_armed_site_injects", "--ignored"])
        .env("BASEMYAI_FAILPOINTS", "env_armed_site=error")
        .status()
        .expect("spawn child test process");
    assert!(status.success(), "child must see the env-armed failpoint");

    // Unknown action names must be a loud failure in the child, never a
    // silently disarmed harness.
    let exe = std::env::current_exe().expect("test exe");
    let status = std::process::Command::new(exe)
        .args(["--exact", "helper_child_env_armed_site_injects", "--ignored"])
        .env("BASEMYAI_FAILPOINTS", "env_armed_site=typo")
        .status()
        .expect("spawn child test process");
    assert!(!status.success(), "a typo'd action must panic, not silently skip");
}
