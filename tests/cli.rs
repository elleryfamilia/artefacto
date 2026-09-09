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

#[test]
fn render_reports_an_unreadable_plan_as_a_usage_error() {
    let dir = tempfile::tempdir().unwrap();
    bin()
        .args([
            "plan",
            "render",
            "/nonexistent/plan.json",
            "--out",
            dir.path().join("plan.html").to_str().unwrap(),
            "--no-open",
        ])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("/nonexistent/plan.json"));
    assert!(!dir.path().join("plan.html").exists(), "nothing is written");
}

#[test]
fn render_json_reports_a_failure_as_json() {
    let dir = tempfile::tempdir().unwrap();
    let out = bin()
        .args([
            "plan",
            "render",
            &fixture("invalid-cycle.json"),
            "--out",
            dir.path().join("plan.html").to_str().unwrap(),
            "--no-open",
            "--json",
        ])
        .assert()
        .code(1)
        .get_output()
        .stdout
        .clone();
    let doc: serde_json::Value =
        serde_json::from_slice(&out).expect("--json emits a JSON envelope even on failure");
    assert_eq!(doc["ok"], false);
    assert!(!doc["errors"].as_array().unwrap().is_empty());
}

#[test]
fn render_json_reports_an_unreadable_plan_as_json() {
    let dir = tempfile::tempdir().unwrap();
    let out = bin()
        .args([
            "plan",
            "render",
            "/nonexistent/plan.json",
            "--out",
            dir.path().join("plan.html").to_str().unwrap(),
            "--no-open",
            "--json",
        ])
        .assert()
        .code(2)
        .get_output()
        .stdout
        .clone();
    let doc: serde_json::Value = serde_json::from_slice(&out).expect("JSON on stdout");
    assert_eq!(doc["ok"], false);
    assert_eq!(doc["errors"][0]["code"], "unreadable");
}

#[test]
fn status_reports_fresh_after_a_render() {
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
    bin()
        .args([
            "plan",
            "status",
            &fixture("minimal.json"),
            "--out",
            out.to_str().unwrap(),
        ])
        .assert()
        .success()
        .stdout(contains("fresh"));
}

#[test]
fn status_reports_stale_when_the_plan_changed() {
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
    bin()
        .args([
            "plan",
            "status",
            &fixture("kitchen-sink.json"),
            "--out",
            out.to_str().unwrap(),
        ])
        .assert()
        .code(1)
        .stdout(contains("stale"));
}

#[test]
fn status_reports_missing_when_nothing_was_rendered() {
    let dir = tempfile::tempdir().unwrap();
    bin()
        .args([
            "plan",
            "status",
            &fixture("minimal.json"),
            "--out",
            dir.path().join("absent.html").to_str().unwrap(),
        ])
        .assert()
        .code(1)
        .stdout(contains("none"));
}

#[test]
fn status_json_carries_the_hashes_it_compared() {
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
    let stdout = bin()
        .args([
            "plan",
            "status",
            &fixture("minimal.json"),
            "--out",
            out.to_str().unwrap(),
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let doc: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(doc["state"], "fresh");
    assert_eq!(doc["plan_hash"], doc["rendered_hash"]);
}

#[test]
fn schema_prints_the_reference() {
    bin()
        .args(["plan", "schema"])
        .assert()
        .success()
        .stdout(contains("artefacto.plan/1"));
}

#[test]
fn no_command_output_mentions_loadout() {
    // artefacto is a separate project. Someone using it will not have loadout
    // and should never see its name — including when their plan.json still
    // carries loadout's old format string, and including whatever a command
    // writes to stderr, not just stdout.
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("plan.html");
    let out_str = out.to_str().unwrap().to_string();
    let legacy = fixture("legacy-format.json");

    let cases: Vec<Vec<&str>> = vec![
        vec!["plan", "schema"],
        vec!["--help"],
        vec!["plan", "--help"],
        vec!["plan", "check", legacy.as_str()],
        vec!["plan", "check", "--json", legacy.as_str()],
        vec![
            "plan",
            "render",
            "--json",
            legacy.as_str(),
            "--out",
            out_str.as_str(),
        ],
    ];

    for args in cases {
        let assert = bin().args(&args).assert().success();
        let output = assert.get_output();
        let stdout = String::from_utf8_lossy(&output.stdout).to_lowercase();
        let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();
        assert!(
            !stdout.contains("loadout"),
            "`{args:?}` mentioned loadout on stdout: {stdout}"
        );
        assert!(
            !stderr.contains("loadout"),
            "`{args:?}` mentioned loadout on stderr: {stderr}"
        );
    }
}

// --- Final-review consistency fixes -------------------------------------

#[test]
fn every_json_result_carries_a_boolean_ok() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("plan.html");

    let check_out = bin()
        .args(["plan", "check", "--json", &fixture("minimal.json")])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let doc: serde_json::Value = serde_json::from_slice(&check_out).unwrap();
    assert_eq!(
        doc["ok"], true,
        "check --json on a valid plan must report ok: true"
    );

    let render_out = bin()
        .args([
            "plan",
            "render",
            &fixture("minimal.json"),
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
    let doc: serde_json::Value = serde_json::from_slice(&render_out).unwrap();
    assert_eq!(
        doc["ok"], true,
        "render --json on a successful render must report ok: true"
    );

    let status_ok = bin()
        .args([
            "plan",
            "status",
            &fixture("minimal.json"),
            "--out",
            out.to_str().unwrap(),
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let doc: serde_json::Value = serde_json::from_slice(&status_ok).unwrap();
    assert_eq!(
        doc["ok"], true,
        "status --json on a fresh render must report ok: true"
    );
    assert_eq!(doc["state"], "fresh");

    let status_fail = bin()
        .args([
            "plan",
            "status",
            "/nonexistent/plan.json",
            "--out",
            out.to_str().unwrap(),
            "--json",
        ])
        .assert()
        .code(2)
        .get_output()
        .stdout
        .clone();
    let doc: serde_json::Value = serde_json::from_slice(&status_fail)
        .expect("status --json still emits an envelope when the plan can't be read");
    assert_eq!(doc["ok"], false);
    assert_eq!(
        doc["state"], "unknown",
        "the plan couldn't be read, so freshness can't be known either"
    );
}

#[test]
fn status_json_carries_the_plan_facts() {
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
    let stdout = bin()
        .args([
            "plan",
            "status",
            &fixture("minimal.json"),
            "--out",
            out.to_str().unwrap(),
            "--json",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let doc: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert!(doc["plan_hash"].as_str().unwrap().starts_with("sha256:"));
    assert!(doc["title"].is_string(), "status should carry the title");
    assert!(doc["phases"].is_number());
    assert!(doc["tasks"].is_number());
}

#[test]
fn error_detail_lines_never_appear_on_stdout() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("plan.html");

    let check_out = bin()
        .args(["plan", "check", &fixture("invalid-dup-id.json")])
        .assert()
        .code(1)
        .get_output()
        .stdout
        .clone();
    assert!(
        !String::from_utf8_lossy(&check_out).contains("error["),
        "check's error[...] detail lines belong on stderr"
    );

    let render_out = bin()
        .args([
            "plan",
            "render",
            &fixture("invalid-cycle.json"),
            "--out",
            out.to_str().unwrap(),
            "--no-open",
        ])
        .assert()
        .code(1)
        .get_output()
        .stdout
        .clone();
    assert!(!String::from_utf8_lossy(&render_out).contains("error["));

    let status_out = bin()
        .args([
            "plan",
            "status",
            &fixture("invalid-cycle.json"),
            "--out",
            out.to_str().unwrap(),
        ])
        .assert()
        .code(1)
        .get_output()
        .stdout
        .clone();
    assert!(!String::from_utf8_lossy(&status_out).contains("error["));
}

#[test]
fn render_json_reports_warnings() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("plan.html");
    let stdout = bin()
        .args([
            "plan",
            "render",
            &fixture("learning-v0-15.json"),
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
    let warnings = doc["warnings"].as_array().expect("warnings array");
    let codes: Vec<&str> = warnings
        .iter()
        .map(|w| w["code"].as_str().unwrap())
        .collect();
    assert!(codes.contains(&"long_summary"), "codes: {codes:?}");
    assert!(codes.contains(&"wall_of_text"), "codes: {codes:?}");
    assert!(codes.contains(&"long_goal"), "codes: {codes:?}");
}

#[test]
fn render_write_failure_still_emits_a_json_envelope() {
    let dir = tempfile::tempdir().unwrap();
    // A file where a directory needs to go: `create_dir_all` on its parent
    // will fail, so the write never gets a chance to run either.
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, b"not a directory").unwrap();
    let out = blocker.join("plan.html");

    let stdout = bin()
        .args([
            "plan",
            "render",
            &fixture("minimal.json"),
            "--out",
            out.to_str().unwrap(),
            "--no-open",
            "--json",
        ])
        .assert()
        .code(2)
        .get_output()
        .stdout
        .clone();
    let doc: serde_json::Value = serde_json::from_slice(&stdout)
        .expect("a write failure must still print a parseable JSON envelope");
    assert_eq!(doc["ok"], false);
    assert_eq!(doc["errors"][0]["code"], "write_failed");
}

#[test]
fn check_json_reports_the_deprecated_format_as_a_warning_not_stderr() {
    let assert = bin()
        .args(["plan", "check", "--json", &fixture("legacy-format.json")])
        .assert()
        .success();
    let output = assert.get_output();
    let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let warnings = doc["files"][0]["warnings"]
        .as_array()
        .expect("warnings array");
    assert!(
        warnings.iter().any(|w| w["code"] == "deprecated_format"),
        "warnings: {warnings:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();
    assert!(
        !stderr.contains("deprecated"),
        "the deprecation note must not print to stderr: {stderr}"
    );
}
