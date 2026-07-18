use assert_cmd::Command;
use predicates::prelude::*;

const CASE: &str = r#"{"schema_version":1,"id":"cli-smoke","suite":"cli","description":"cli smoke","seed":9,"query":"CLI-900","k":2,"token_budget":128,"options":{"source_policy":"allow_all"},"memories":[{"id":"target","text":"CLI-900 deterministic target","layer":"semantic","relevance":3}],"must_include":["target"],"retrieval":{"hybrid":{"must_include":["target"]}}}"#;

#[test]
fn run_writes_json_and_human_reports() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let dataset = directory.path().join("cases.jsonl");
    let json_report = directory.path().join("report.json");
    let human_report = directory.path().join("report.md");
    std::fs::write(&dataset, CASE).expect("write dataset");

    Command::cargo_bin("basemyai-eval")
        .expect("binary")
        .args([
            "run",
            dataset.to_str().expect("utf-8 path"),
            "--output",
            json_report.to_str().expect("utf-8 path"),
            "--human",
            human_report.to_str().expect("utf-8 path"),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("1/1 cases passed"));

    let json = std::fs::read_to_string(json_report).expect("JSON report");
    let human = std::fs::read_to_string(human_report).expect("human report");
    assert!(json.contains("\"failed_cases\": 0"));
    assert!(human.contains("`cli-smoke`"));
}

#[test]
fn invalid_dataset_uses_runtime_error_exit_code() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let dataset = directory.path().join("invalid.jsonl");
    let report = directory.path().join("report.json");
    std::fs::write(&dataset, "{}\n").expect("write invalid dataset");

    Command::cargo_bin("basemyai-eval")
        .expect("binary")
        .args([
            "run",
            dataset.to_str().expect("utf-8 path"),
            "--output",
            report.to_str().expect("utf-8 path"),
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("invalid JSON on line 1"));
}
