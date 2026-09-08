use assert_cmd::Command;
use predicates::str::contains;

fn bin() -> Command {
    Command::cargo_bin("artefacto").expect("binary builds")
}

fn fixture(name: &str) -> String {
    format!("{}/tests/fixtures/plan/{name}", env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn check_reports_a_valid_plan() {
    bin()
        .args(["plan", "check", &fixture("minimal.json")])
        .assert()
        .success()
        .stdout(contains("valid"));
}

#[test]
fn check_fails_on_an_invalid_plan() {
    bin()
        .args(["plan", "check", &fixture("invalid-dup-id.json")])
        .assert()
        .code(1);
}

#[test]
fn check_json_emits_one_object_per_file() {
    let out = bin()
        .args([
            "plan",
            "check",
            "--json",
            &fixture("minimal.json"),
            &fixture("kitchen-sink.json"),
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let doc: serde_json::Value = serde_json::from_slice(&out).expect("stdout is one JSON document");
    let files = doc["files"].as_array().expect("files array");
    assert_eq!(files.len(), 2, "one entry per input file");
    for f in files {
        assert_eq!(f["ok"], true);
        assert!(f["plan_hash"].as_str().unwrap().starts_with("sha256:"));
        assert!(
            f["title"].is_string(),
            "title is needed by the studio badge"
        );
        assert!(f["phases"].is_number());
        assert!(f["tasks"].is_number());
        assert!(f["path"].is_string());
    }
}

#[test]
fn check_json_reports_a_bad_file_without_failing_the_good_one() {
    let out = bin()
        .args([
            "plan",
            "check",
            "--json",
            &fixture("minimal.json"),
            &fixture("invalid-dup-id.json"),
        ])
        .assert()
        .code(1)
        .get_output()
        .stdout
        .clone();
    let doc: serde_json::Value = serde_json::from_slice(&out).unwrap();
    let files = doc["files"].as_array().unwrap();
    assert_eq!(files[0]["ok"], true);
    assert_eq!(files[1]["ok"], false);
    assert!(!files[1]["errors"].as_array().unwrap().is_empty());
}

#[test]
fn check_lenient_downgrades_unknown_fields_to_warnings() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("extra.json");
    let base = std::fs::read_to_string(fixture("minimal.json")).unwrap();
    let mut v: serde_json::Value = serde_json::from_str(&base).unwrap();
    v["meta"]["not_a_real_field"] = serde_json::json!("x");
    std::fs::write(&p, serde_json::to_string(&v).unwrap()).unwrap();

    bin()
        .args(["plan", "check", p.to_str().unwrap()])
        .assert()
        .code(1);
    bin()
        .args(["plan", "check", "--lenient", p.to_str().unwrap()])
        .assert()
        .success();
}

#[test]
fn check_reports_a_missing_file_as_a_usage_error() {
    bin()
        .args(["plan", "check", "/nonexistent/plan.json"])
        .assert()
        .code(2)
        .stderr(contains("/nonexistent/plan.json"));
}

#[test]
fn a_missing_file_does_not_hide_the_files_around_it() {
    let out = bin()
        .args([
            "plan",
            "check",
            "--json",
            &fixture("minimal.json"),
            "/nonexistent/plan.json",
            &fixture("kitchen-sink.json"),
        ])
        .assert()
        .code(2)
        .get_output()
        .stdout
        .clone();
    let doc: serde_json::Value = serde_json::from_slice(&out)
        .expect("a batch still prints JSON even when one file is unreadable");
    let files = doc["files"].as_array().expect("files array");
    assert_eq!(files.len(), 3, "every input file gets an entry");
    assert_eq!(files[0]["ok"], true);
    assert_eq!(
        files[1]["ok"], false,
        "the unreadable file is reported, not skipped"
    );
    assert_eq!(
        files[2]["ok"], true,
        "checking continues past the unreadable file"
    );
}

#[test]
fn render_writes_a_document_starting_with_the_marker_line() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("plan.html");
    bin()
        .args([
            "plan",
            "render",
            &fixture("minimal.json"),
            "--out",
            out.to_str().unwrap(),
            "--no-open",
        ])
        .assert()
        .success();
    let html = std::fs::read_to_string(&out).expect("render wrote the file");
    assert!(
        html.starts_with("<!-- artefacto:generated context=sha256:"),
        "first line: {:?}",
        html.lines().next()
    );
    assert!(html.contains("<!doctype html>") || html.contains("<!DOCTYPE html>"));
}

#[test]
fn render_json_reports_what_the_dispatcher_records() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("plan.html");
    let stdout = bin()
        .args([
            "plan",
            "render",
            &fixture("kitchen-sink.json"),
            "--out",
            out.to_str().unwrap(),
            "--no-open",
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let doc: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert!(doc["plan_hash"].as_str().unwrap().starts_with("sha256:"));
    assert!(doc["title"].is_string());
    assert!(doc["phases"].is_number());
    assert!(doc["tasks"].is_number());
    assert_eq!(doc["out"], out.display().to_string());
}

#[test]
fn render_refuses_an_invalid_plan_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("plan.html");
    bin()
        .args([
            "plan",
            "render",
            &fixture("invalid-cycle.json"),
            "--out",
            out.to_str().unwrap(),
            "--no-open",
        ])
        .assert()
        .code(1);
    assert!(
        !out.exists(),
        "a rejected plan must not leave a partial file"
    );
}

#[test]
fn render_resolves_a_relative_out_against_the_invocation_directory() {
    let dir = tempfile::tempdir().unwrap();
    bin()
        .current_dir(dir.path())
        .args([
            "plan",
            "render",
            &fixture("minimal.json"),
            "--out",
            "nested/plan.html",
            "--no-open",
        ])
        .assert()
        .success();
    assert!(
        dir.path().join("nested/plan.html").exists(),
        "relative --out anchors to cwd"
    );
}
