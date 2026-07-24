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
    cmd.env("NO_COLOR", "1");
    cmd.current_dir(home);
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

/// Depuis la migration natif-only, la sous-commande `maintenance` a été retirée
/// de la CLI. L'invocation doit échouer explicitement comme sous-commande
/// inconnue.
#[test]
fn maintenance_command_is_not_available() {
    let home = tempfile::tempdir().expect("tempdir");
    let db = db_path(home.path());
    init_container(home.path(), &db);

    isolated(home.path())
        .env("BASEMYAI_DB_KEY", KEY)
        .args(["--db"])
        .arg(&db)
        .args(["maintenance", "gc"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("unrecognized subcommand 'maintenance'"));
}

#[test]
fn no_db_path_uses_default_container_path() {
    let home = tempfile::tempdir().expect("tempdir");
    let db = db_path(home.path());

    isolated(home.path())
        .env("BASEMYAI_DB_KEY", KEY)
        .args(["config", "set", "db-path"])
        .arg(&db)
        .assert()
        .success();
    init_container(home.path(), &db);

    isolated(home.path())
        .env("BASEMYAI_DB_KEY", KEY)
        .args(["--format", "json", "--agent", "alice", "inspect"])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "\"path\":{}",
            serde_json::to_string(&db.display().to_string()).expect("path serializes")
        )));
}

#[test]
fn color_never_disables_ansi_sequences() {
    let home = tempfile::tempdir().expect("tempdir");
    let db = db_path(home.path());
    init_container(home.path(), &db);

    isolated(home.path())
        .env("BASEMYAI_DB_KEY", KEY)
        .args(["--color", "never", "--db"])
        .arg(&db)
        .arg("verify")
        .assert()
        .success()
        .stdout(predicate::str::contains('\u{1b}').not());
}

fn write_default_key_file(home: &Path, key: &str) {
    let dir = home.join(".basemyai");
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
            .expect("dir");
        let path = dir.join("key");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .expect("open key");
        writeln!(file, "{key}").expect("write key");
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join("key"), format!("{key}\n")).expect("write key");
    }
}

#[test]
fn init_succeeds_with_default_key_file_without_env() {
    let home = tempfile::tempdir().expect("tempdir");
    write_default_key_file(home.path(), KEY);
    let db = db_path(home.path());
    isolated(home.path())
        .args(["--format", "json", "init"])
        .arg(&db)
        .assert()
        .success();
}

#[test]
fn config_key_generate_never_prints_key_material() {
    let home = tempfile::tempdir().expect("tempdir");
    let out = isolated(home.path())
        .args(["config", "key", "generate"])
        .assert()
        .success()
        .get_output()
        .clone();
    let key_path = home.path().join(".basemyai").join("key");
    assert!(key_path.exists());
    let secret = std::fs::read_to_string(&key_path).expect("read key").trim().to_string();
    assert!(!secret.is_empty());
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!combined.contains(&secret));
}

#[test]
fn config_key_generate_refuses_overwrite() {
    let home = tempfile::tempdir().expect("tempdir");
    write_default_key_file(home.path(), KEY);
    isolated(home.path())
        .args(["config", "key", "generate"])
        .assert()
        .failure();
}

#[test]
fn config_key_check_ok_when_env_set() {
    let home = tempfile::tempdir().expect("tempdir");
    isolated(home.path())
        .env("BASEMYAI_DB_KEY", KEY)
        .args(["--format", "json", "config", "key", "check"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"ok\":true"))
        .stdout(predicate::str::contains("env_db_key"));
}

#[test]
fn forget_adaptive_on_fresh_agent_is_a_noop_with_json_report() {
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
        .args(["forget-adaptive", "--capacity", "10"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON on stdout");
    assert_eq!(v["scanned"], 0);
    assert_eq!(v["evicted"], 0);
    assert_eq!(v["capacity"], 10);
    assert_eq!(v["dry_run"], false);
}

#[test]
fn forget_adaptive_dry_run_flag_is_threaded_through_to_the_json_report() {
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
        .args(["forget-adaptive", "--capacity", "0", "--dry-run"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON on stdout");
    assert_eq!(v["dry_run"], true);
}

/// `forget-adaptive`/`gc` opèrent sur le store nu (`open_engine`), jamais sur
/// une `Memory` complète : elles ne doivent pas exiger de modèle Candle
/// provisionné, contrairement à `remember`/`recall`/`consolidate`.
#[test]
fn forget_adaptive_does_not_require_a_provisioned_embedder() {
    let home = tempfile::tempdir().expect("tempdir");
    let db = db_path(home.path());
    init_container(home.path(), &db);

    isolated(home.path())
        .env("BASEMYAI_DB_KEY", KEY)
        .arg("--db")
        .arg(&db)
        .arg("--agent")
        .arg("alice")
        .args(["forget-adaptive", "--capacity", "5"])
        .assert()
        .success();
}

#[test]
fn gc_on_fresh_agent_is_a_noop_with_json_report() {
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
        .arg("gc")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON on stdout");
    assert_eq!(v["examined"], 0);
    assert_eq!(v["deleted"], 0);
    assert_eq!(v["pages"], 0);
    assert_eq!(v["dry_run"], false);
}

#[test]
fn gc_dry_run_flag_is_threaded_through_to_the_json_report() {
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
        .args(["gc", "--dry-run"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON on stdout");
    assert_eq!(v["dry_run"], true);
}

#[test]
fn gc_rejects_zero_page_size() {
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
        .args(["gc", "--page-size", "0"])
        .assert()
        .code(5)
        .stderr(predicate::str::contains("\"code\":\"VALIDATION_ERROR\""));
}

#[test]
fn gc_does_not_require_a_provisioned_embedder() {
    let home = tempfile::tempdir().expect("tempdir");
    let db = db_path(home.path());
    init_container(home.path(), &db);

    isolated(home.path())
        .env("BASEMYAI_DB_KEY", KEY)
        .arg("--db")
        .arg(&db)
        .arg("--agent")
        .arg("alice")
        .arg("gc")
        .assert()
        .success();
}

const ROTATED_KEY: &str = "rotated-test-key-do-not-use-in-prod";

#[test]
fn low_memory_passphrase_profile_works_for_init_and_rotation() {
    let home = tempfile::tempdir().expect("tempdir");
    let db = db_path(home.path());

    isolated(home.path())
        .env("BASEMYAI_DB_KEY", KEY)
        .arg("init")
        .arg(&db)
        .arg("--low-memory")
        .assert()
        .success();

    isolated(home.path())
        .env("BASEMYAI_DB_KEY", KEY)
        .env("BASEMYAI_DB_KEY_MODE", "passphrase")
        .args(["--db", db.to_str().expect("utf-8 path"), "rotate-key"])
        .args(["--new-key", ROTATED_KEY, "--passphrase", "--low-memory"])
        .assert()
        .success();

    isolated(home.path())
        .env("BASEMYAI_DB_KEY", ROTATED_KEY)
        .env("BASEMYAI_DB_KEY_MODE", "passphrase")
        .args(["--format", "json", "--db", db.to_str().expect("utf-8 path"), "verify"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"valid\":true"));
}

#[test]
fn rotate_low_memory_requires_explicit_passphrase_mode() {
    let home = tempfile::tempdir().expect("tempdir");
    isolated(home.path())
        .args(["rotate-key", "--new-key", ROTATED_KEY, "--low-memory"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--passphrase"));
}

#[test]
fn rotate_key_rewraps_dek_and_old_key_stops_working() {
    let home = tempfile::tempdir().expect("tempdir");
    let db = db_path(home.path());
    init_container(home.path(), &db);

    isolated(home.path())
        .env("BASEMYAI_DB_KEY", KEY)
        .args([
            "--format",
            "json",
            "--db",
            db.to_str().expect("utf-8 path"),
            "rotate-key",
            "--new-key",
            ROTATED_KEY,
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"rotated\":true"));

    isolated(home.path())
        .env("BASEMYAI_DB_KEY", ROTATED_KEY)
        .args(["--format", "json", "--db", db.to_str().expect("utf-8 path"), "verify"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"valid\":true"));

    isolated(home.path())
        .env("BASEMYAI_DB_KEY", KEY)
        .args(["--format", "json", "--db", db.to_str().expect("utf-8 path"), "verify"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("WRONG_ENCRYPTION_KEY"));
}

#[test]
fn verify_physical_and_logical_report_healthy_on_a_fresh_container() {
    let home = tempfile::tempdir().expect("tempdir");
    let db = db_path(home.path());
    init_container(home.path(), &db);

    for mode_flag in ["--physical", "--logical"] {
        let out = isolated(home.path())
            .env("BASEMYAI_DB_KEY", KEY)
            .args([
                "--format",
                "json",
                "--db",
                db.to_str().expect("utf-8 path"),
                "verify",
                mode_flag,
            ])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let v: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON on stdout");
        assert_eq!(v["valid"], true, "mode {mode_flag}: {v}");
        assert_eq!(v["integrity"]["healthy"], true, "mode {mode_flag}: {v}");
    }
}

#[test]
fn repair_dry_run_on_a_healthy_container_proposes_nothing_and_writes_nothing() {
    let home = tempfile::tempdir().expect("tempdir");
    let db = db_path(home.path());
    init_container(home.path(), &db);

    let out = isolated(home.path())
        .env("BASEMYAI_DB_KEY", KEY)
        .args([
            "--format",
            "json",
            "--db",
            db.to_str().expect("utf-8 path"),
            "repair",
            "--dry-run",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON on stdout");
    assert_eq!(v["dry_run"], true);
    assert_eq!(v["actions"], serde_json::json!([]));
    assert_eq!(v["applied"], serde_json::Value::Null);
}

#[test]
fn rebuild_indexes_on_a_fresh_container_is_a_noop_report() {
    let home = tempfile::tempdir().expect("tempdir");
    let db = db_path(home.path());
    init_container(home.path(), &db);

    let out = isolated(home.path())
        .env("BASEMYAI_DB_KEY", KEY)
        .args([
            "--format",
            "json",
            "--db",
            db.to_str().expect("utf-8 path"),
            "rebuild-indexes",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON on stdout");
    assert_eq!(v["memory_mappings_rebuilt"], 0);
    assert_eq!(v["reembedding_required"], serde_json::json!([]));
}

#[test]
fn compact_on_a_fresh_container_reports_before_and_after_stats() {
    let home = tempfile::tempdir().expect("tempdir");
    let db = db_path(home.path());
    init_container(home.path(), &db);

    let out = isolated(home.path())
        .env("BASEMYAI_DB_KEY", KEY)
        .args(["--format", "json", "--db", db.to_str().expect("utf-8 path"), "compact"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let v: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON on stdout");
    // `init` seeds container metadata into the memtable but never flushes
    // it: no SST exists yet. `compact` flushes then merges, so exactly one
    // SST exists afterwards.
    assert_eq!(v["before"]["sst_count"], 0);
    assert_eq!(v["after"]["sst_count"], 1);
}
