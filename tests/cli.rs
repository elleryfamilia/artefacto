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

/// A HOME with no install receipt in it, so `update` cannot mistake the
/// developer's real install for the one under test. Every update test needs
/// this: without it the command reads `~/.config/artefacto/` and would report
/// on whatever is actually installed on the machine running the suite.
fn homeless() -> (tempfile::TempDir, Command) {
    let home = tempfile::tempdir().expect("a temp home");
    let mut c = bin();
    c.env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .env_remove("CARGO_HOME");
    (home, c)
}

#[test]
fn update_without_a_receipt_says_so_and_names_the_installer() {
    let (_home, mut c) = homeless();
    let assert = c.args(["update", "--check"]).assert().code(4);
    let err = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        err.contains("not installed by the artefacto installer"),
        "it must say why it cannot update: {err}"
    );
    assert!(
        err.contains("artefacto-installer.sh"),
        "and name the way to get an install it can update: {err}"
    );
    // Nothing on stdout: an agent parsing this must not find prose there.
    assert!(
        assert.get_output().stdout.is_empty(),
        "a refusal writes to stderr only"
    );
}

#[test]
fn update_refuses_when_the_receipt_is_for_a_binary_somewhere_else() {
    // The hazard this closes: axoupdater installs into the *receipt's* prefix,
    // not next to the binary you ran. So `cargo run -- update` from a checkout
    // would replace the copy on the developer's PATH. axoupdater's own guard
    // refuses, but it refuses by reporting "no update needed" -- which reads
    // as "you are on the latest release", a confident answer to a question it
    // never asked, and it never touches the network to find out.
    let home = tempfile::tempdir().expect("a temp home");
    let elsewhere = home.path().join("elsewhere");
    std::fs::create_dir_all(home.path().join(".config/artefacto")).unwrap();
    std::fs::create_dir_all(elsewhere.join("bin")).unwrap();
    std::fs::write(
        home.path().join(".config/artefacto/artefacto-receipt.json"),
        serde_json::json!({
            "binaries": ["artefacto"],
            "install_layout": "cargo-home",
            "install_prefix": elsewhere.to_str().unwrap(),
            "modify_path": true,
            "provider": { "source": "cargo-dist", "version": "0.32.0" },
            "source": {
                "app_name": "artefacto",
                "name": "artefacto",
                "owner": "elleryfamilia",
                "release_type": "github"
            },
            "version": "0.1.0"
        })
        .to_string(),
    )
    .unwrap();

    let assert = bin()
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .args(["update", "--check"])
        .assert()
        .code(4);
    let err = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        err.contains("not the installed artefacto"),
        "it must say this copy is not the one the receipt names: {err}"
    );
    assert!(
        !err.contains("latest release") && !err.contains("is available"),
        "and must not answer the version question it never asked: {err}"
    );
}

#[test]
fn update_help_documents_check_and_says_installs_only() {
    bin()
        .args(["update", "--help"])
        .assert()
        .success()
        .stdout(contains("--check"))
        .stdout(contains("Installer-based installs only"));
}

#[test]
fn the_refresher_writes_a_verdict_and_says_nothing() {
    // The detached child the server spawns. It exists only to leave a file
    // behind: anything it printed would go to /dev/null, and anything it
    // returned would be ignored, so silence and exit 0 are the contract.
    let home = tempfile::tempdir().expect("a temp home");
    let state = tempfile::tempdir().expect("a temp state dir");
    let assert = bin()
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .env("XDG_STATE_HOME", state.path())
        .args(["update", "--refresh-cache"])
        .assert()
        .success();
    let out = assert.get_output();
    assert!(
        out.stdout.is_empty() && out.stderr.is_empty(),
        "the refresher is silent: stdout {:?} stderr {:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // Shared across repositories: one answer per machine, not one per repo.
    let cache = state.path().join("artefacto").join("update-check");
    let text = std::fs::read_to_string(&cache)
        .unwrap_or_else(|e| panic!("the verdict must be written to {}: {e}", cache.display()));
    let mut parts = text.split_whitespace();
    parts
        .next()
        .expect("a timestamp")
        .parse::<u64>()
        .expect("a number");
    assert_eq!(
        parts.next(),
        Some(env!("CARGO_PKG_VERSION")),
        "stamped with the binary that asked, so a later version ignores it"
    );
    // No receipt in this HOME, so there is nothing to tell anyone: "failed"
    // is the honest verdict, not "current".
    assert_eq!(parts.next(), Some("failed"), "in: {text:?}");
}

#[test]
fn the_kill_switch_stops_the_refresher_from_asking() {
    let home = tempfile::tempdir().expect("a temp home");
    let state = tempfile::tempdir().expect("a temp state dir");
    bin()
        .env("HOME", home.path())
        .env("XDG_STATE_HOME", state.path())
        .env("ARTEFACTO_NO_UPDATE_CHECK", "1")
        .args(["update", "--check"])
        .assert()
        .code(4);
    // The switch is about the automatic check. A command typed by hand still
    // does what it was asked -- it refused above because there is no receipt,
    // not because the switch is set.
    assert!(
        !state.path().join("artefacto").join("update-check").exists(),
        "an explicit command writes no verdict cache"
    );
}

#[test]
fn refresh_cache_is_not_advertised() {
    // It is not a thing to run by hand: it exists so the server can learn
    // about a release without waiting on the network itself.
    let assert = bin().args(["update", "--help"]).assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    assert!(
        !stdout.contains("refresh-cache"),
        "the hidden flag must stay out of the help: {stdout}"
    );
}

#[test]
fn no_ordinary_command_can_reach_the_network() {
    // The promise the README makes, held by a test rather than by care.
    //
    // Pointed at a port nothing is listening on: any code that tried to reach
    // GitHub through the proxy would fail, and `--check` is the one command
    // allowed to care. Everything below must be completely unaffected,
    // because none of them ask anything of the network at all.
    let home = tempfile::tempdir().expect("a temp home");
    let state = tempfile::tempdir().expect("a temp state dir");
    let nowhere = "http://127.0.0.1:9";

    for args in [
        vec!["plan", "check", &fixture("minimal.json")],
        vec!["plan", "schema"],
        vec!["list", "--json"],
        vec!["status", "--json"],
    ] {
        let out = bin()
            .env("HOME", home.path())
            .env("XDG_STATE_HOME", state.path())
            .env("HTTP_PROXY", nowhere)
            .env("HTTPS_PROXY", nowhere)
            .env("ALL_PROXY", nowhere)
            .args(&args)
            .output()
            .expect("it runs");
        let stderr = String::from_utf8_lossy(&out.stderr).to_lowercase();
        assert!(
            !stderr.contains("proxy") && !stderr.contains("connection refused"),
            "`artefacto {}` must not touch the network: {stderr}",
            args.join(" ")
        );
    }

    // And none of them left a verdict behind, which is the other half: the
    // check is the server's job, not something every command does on the way
    // past.
    assert!(
        !state.path().join("artefacto").join("update-check").exists(),
        "no ordinary command writes an update verdict"
    );
}
