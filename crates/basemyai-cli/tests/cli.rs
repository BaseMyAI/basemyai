//! Tests d'intégration CLI : process réel (`assert_cmd`), pas de mock. Couvre
//! les commandes qui n'ont pas besoin de l'embedder Candle local — un modèle
//! n'est pas disponible hors-ligne en CI. `remember`/`recall`/`stats`/
//! `export`/`import`/`consolidate` (qui chargent l'embedder via `open_memory`)
//! restent donc hors de cette suite ; voir `docs/cli.md`.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;

const KEY: &str = "test-key-do-not-use-in-prod";

fn bin() -> Command {
    Command::cargo_bin("basemyai").expect("binary built by cargo test")
}

/// Process isolé de l'environnement réel de la machine : pas de
/// `~/.basemyai/config.toml` ambiant, pas de `BASEMYAI_*` hérité du shell qui
/// lance les tests. `home` doit être un répertoire temporaire dédié au test.
fn isolated(home: &Path) -> Command {
    let mut cmd = bin();
    cmd.env_clear();
    for var in ["SystemRoot", "windir", "PATH"] {
        if let Ok(v) = std::env::var(var) {
            cmd.env(var, v);
        }
    }
    cmd.env("HOME", home);
    cmd.env("USERPROFILE", home);
    cmd
}

fn db_path(dir: &Path) -> PathBuf {
    dir.join("agent.bmai")
}

fn init_container(home: &Path, db: &Path) {
    isolated(home)
        .env("BASEMYAI_DB_KEY", KEY)
        .arg("init")
        .arg(db)
        .assert()
        .success();
}

#[test]
fn missing_key_is_key_required_exit_3() {
    let home = tempfile::tempdir().expect("tempdir");
    let db = db_path(home.path());
    isolated(home.path())
        .args(["--format", "json", "init"])
        .arg(&db)
        .assert()
        .code(3)
        .stderr(predicate::str::contains("\"code\":\"KEY_REQUIRED\""));
}

#[test]
fn init_creates_container_then_rejects_duplicate() {
    let home = tempfile::tempdir().expect("tempdir");
    let db = db_path(home.path());
    init_container(home.path(), &db);
    assert!(db.exists());

    isolated(home.path())
        .env("BASEMYAI_DB_KEY", KEY)
        .args(["--format", "json", "init"])
        .arg(&db)
        .assert()
        .code(6)
        .stderr(predicate::str::contains("\"code\":\"ALREADY_EXISTS\""));
}

#[test]
fn inspect_reports_container_metadata() {
    let home = tempfile::tempdir().expect("tempdir");
    let db = db_path(home.path());
    init_container(home.path(), &db);

    let out = isolated(home.path())
        .env("BASEMYAI_DB_KEY", KEY)
        .arg("--format")
        .arg("json")
        .arg("--db")
        .arg(&db)
        .arg("inspect")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON on stdout");
    assert_eq!(v["metadata"]["format"], "basemyai-memory");
    assert_eq!(v["total_memories"], 0);
}

#[test]
fn verify_reports_valid_container() {
    let home = tempfile::tempdir().expect("tempdir");
    let db = db_path(home.path());
    init_container(home.path(), &db);

    let out = isolated(home.path())
        .env("BASEMYAI_DB_KEY", KEY)
        .arg("--format")
        .arg("json")
        .arg("--db")
        .arg(&db)
        .arg("verify")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON on stdout");
    assert_eq!(v["valid"], true);
}

#[test]
fn migrate_is_idempotent() {
    let home = tempfile::tempdir().expect("tempdir");
    let db = db_path(home.path());
    init_container(home.path(), &db);

    for _ in 0..2 {
        isolated(home.path())
            .env("BASEMYAI_DB_KEY", KEY)
            .arg("--db")
            .arg(&db)
            .arg("migrate")
            .assert()
            .success();
    }
}

#[test]
fn list_on_fresh_agent_is_empty() {
    let home = tempfile::tempdir().expect("tempdir");
    let db = db_path(home.path());
    init_container(home.path(), &db);

    let out = isolated(home.path())
        .env("BASEMYAI_DB_KEY", KEY)
        .arg("--format")
        .arg("json")
        .arg("--db")
        .arg(&db)
        .arg("--agent")
        .arg("alice")
        .arg("list")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON on stdout");
    assert_eq!(v["memories"].as_array().expect("array").len(), 0);
}

#[test]
fn forget_and_invalidate_unknown_id_are_noops() {
    let home = tempfile::tempdir().expect("tempdir");
    let db = db_path(home.path());
    init_container(home.path(), &db);

    isolated(home.path())
        .env("BASEMYAI_DB_KEY", KEY)
        .arg("--db")
        .arg(&db)
        .arg("--agent")
        .arg("alice")
        .args(["forget", "does-not-exist"])
        .assert()
        .success();

    isolated(home.path())
        .env("BASEMYAI_DB_KEY", KEY)
        .arg("--db")
        .arg(&db)
        .arg("--agent")
        .arg("alice")
        .args(["invalidate", "does-not-exist"])
        .assert()
        .success();
}

#[test]
fn purge_requires_explicit_yes() {
    let home = tempfile::tempdir().expect("tempdir");
    let db = db_path(home.path());
    init_container(home.path(), &db);

    isolated(home.path())
        .env("BASEMYAI_DB_KEY", KEY)
        .arg("--format")
        .arg("json")
        .arg("--db")
        .arg(&db)
        .arg("--agent")
        .arg("alice")
        .arg("purge")
        .assert()
        .code(7)
        .stderr(predicate::str::contains("\"code\":\"CONFIRMATION_REQUIRED\""));

    isolated(home.path())
        .env("BASEMYAI_DB_KEY", KEY)
        .arg("--db")
        .arg(&db)
        .arg("--agent")
        .arg("alice")
        .args(["purge", "--yes"])
        .assert()
        .success();
}

#[test]
fn export_json_to_stdout_is_rejected() {
    let home = tempfile::tempdir().expect("tempdir");
    let db = db_path(home.path());

    isolated(home.path())
        .arg("--format")
        .arg("json")
        .arg("--db")
        .arg(&db)
        .arg("--agent")
        .arg("alice")
        .arg("export")
        .assert()
        .code(2)
        .stdout(predicate::str::is_empty())
        .stderr(predicate::str::contains("\"code\":\"USAGE_ERROR\""))
        .stderr(predicate::str::contains("JSONL"));
}

#[test]
fn empty_agent_is_invalid_agent_exit_5() {
    let home = tempfile::tempdir().expect("tempdir");
    let db = db_path(home.path());
    init_container(home.path(), &db);

    isolated(home.path())
        .env("BASEMYAI_DB_KEY", KEY)
        .arg("--format")
        .arg("json")
        .arg("--db")
        .arg(&db)
        .arg("--agent")
        .arg("")
        .arg("list")
        .assert()
        .code(5)
        .stderr(predicate::str::contains("\"code\":\"INVALID_AGENT\""));
}

#[test]
fn graph_round_trip() {
    let home = tempfile::tempdir().expect("tempdir");
    let db = db_path(home.path());
    init_container(home.path(), &db);

    isolated(home.path())
        .env("BASEMYAI_DB_KEY", KEY)
        .arg("--db")
        .arg(&db)
        .arg("--agent")
        .arg("alice")
        .args(["graph", "add-entity", "alice", "person", "Alice"])
        .assert()
        .success();

    isolated(home.path())
        .env("BASEMYAI_DB_KEY", KEY)
        .arg("--db")
        .arg(&db)
        .arg("--agent")
        .arg("alice")
        .args(["graph", "add-entity", "wonderland", "place", "Wonderland"])
        .assert()
        .success();

    isolated(home.path())
        .env("BASEMYAI_DB_KEY", KEY)
        .arg("--db")
        .arg(&db)
        .arg("--agent")
        .arg("alice")
        .args(["graph", "add-edge", "alice", "visited", "wonderland"])
        .assert()
        .success();

    let out = isolated(home.path())
        .env("BASEMYAI_DB_KEY", KEY)
        .arg("--format")
        .arg("json")
        .arg("--db")
        .arg(&db)
        .arg("--agent")
        .arg("alice")
        .args(["graph", "traverse", "alice"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON on stdout");
    let reached = v["reached"].as_array().expect("array");
    assert_eq!(reached.len(), 1);
    assert_eq!(reached[0]["id"], "wonderland");
}

#[test]
fn maintenance_gc_runs_on_empty_db() {
    let home = tempfile::tempdir().expect("tempdir");
    let db = db_path(home.path());
    init_container(home.path(), &db);

    isolated(home.path())
        .env("BASEMYAI_DB_KEY", KEY)
        .arg("--db")
        .arg(&db)
        .args(["maintenance", "gc"])
        .assert()
        .success();
}

#[test]
fn no_db_path_is_not_configured_exit_4() {
    let home = tempfile::tempdir().expect("tempdir");

    isolated(home.path())
        .env("BASEMYAI_DB_KEY", KEY)
        .args(["--format", "json", "--agent", "alice", "inspect"])
        .assert()
        .code(4)
        .stderr(predicate::str::contains("\"code\":\"NOT_CONFIGURED\""));
}
