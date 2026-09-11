//! The artifact index (spec 4.4) and `artefacto list` (spec 5), through the
//! real binary: `render` records a row and draws a poster, `list` reads the
//! registry with no server running, and the registry keeps rosita's Recents
//! conventions — a newer file is left alone, a corrupt one self-heals, and a
//! row whose source is gone is greyed, never pruned.

mod support;

use std::path::Path;
use support::{bin, Repo};

fn fixture(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/plan")
        .join(name)
        .display()
        .to_string()
}

/// Copy a fixture into the repository under `as_name`, so the row's source
/// path is one the test can delete.
fn plan_in(repo: &Repo, fixture_name: &str, as_name: &str) -> String {
    let target = repo.path().join(as_name);
    std::fs::copy(fixture(fixture_name), &target).expect("copy the fixture");
    // Resolved, because the row records the real path and on macOS the
    // temp directory sits behind a `/var` symlink.
    std::fs::canonicalize(&target)
        .expect("the copy exists")
        .display()
        .to_string()
}

/// The repository's path as the binary sees it (symlinks resolved).
fn real(repo: &Repo) -> std::path::PathBuf {
    std::fs::canonicalize(repo.path()).expect("the repo exists")
}

fn render(repo: &Repo, plan: &str, out: &str) -> serde_json::Value {
    repo.json(&["plan", "render", plan, "--out", out, "--no-open", "--json"])
}

fn list(repo: &Repo) -> serde_json::Value {
    repo.json(&["list", "--json"])
}

#[test]
fn render_records_a_row_and_a_poster_and_list_reads_it_with_no_server() {
    let repo = Repo::new();
    let plan = plan_in(&repo, "kitchen-sink.json", "plan.json");
    let out = render(&repo, &plan, "docs/plan.html");
    assert_eq!(out["index"]["recorded"], true, "{out}");
    let poster = out["index"]["poster"].as_str().expect("the poster's path");
    assert!(
        Path::new(poster).is_file(),
        "the poster is on disk where the result says: {poster}"
    );
    assert!(
        !repo.server_json().exists(),
        "a render starts no server, and list must not need one"
    );

    let listed = list(&repo);
    assert_eq!(listed["ok"], true);
    assert_eq!(listed["readonly"], false);
    let rows = listed["artifacts"].as_array().expect("rows");
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row["id"], "plan:auth-refactor", "the server's own id");
    assert_eq!(row["kind"], "plan");
    assert_eq!(row["title"], "Auth refactor");
    assert_eq!(row["revision"], 0, "rendered, never pushed");
    assert_eq!(row["age"], "just now");
    assert_eq!(row["open_threads"], 0);
    assert_eq!(row["unanchored_threads"], 0);
    assert_eq!(row["verdict"], serde_json::Value::Null);
    assert_eq!(row["source_exists"], true);
    assert_eq!(
        row["source_path"]
            .as_str()
            .map(|p| Path::new(p).is_absolute()),
        Some(true),
        "the source is recorded absolute: {row}"
    );
    assert_eq!(
        row["rendered_path"],
        real(&repo).join("docs/plan.html").display().to_string()
    );
    assert_eq!(row["poster"], poster);
    assert!(
        row["revised_at"].as_str().unwrap().ends_with('Z'),
        "absolute timestamps in the JSON: {row}"
    );
    assert_eq!(
        row["plan_hash"], out["plan_hash"],
        "the row carries the render's own hash"
    );
}

#[test]
fn the_poster_on_disk_is_the_drawn_card_for_an_unpushed_plan() {
    let repo = Repo::new();
    let plan = plan_in(&repo, "kitchen-sink.json", "plan.json");
    let out = render(&repo, &plan, "plan.html");
    let poster = std::fs::read_to_string(out["index"]["poster"].as_str().unwrap()).unwrap();
    let parsed = artefacto::plan::model::parse(&std::fs::read_to_string(&plan).unwrap(), false)
        .unwrap()
        .plan;
    assert_eq!(
        poster,
        artefacto::plan::poster::poster_svg(&parsed, &Default::default()),
        "drawn from the plan model, with no review state"
    );
    assert!(poster.contains(">not pushed<"));
}

#[test]
fn a_render_outside_a_repository_still_succeeds_and_says_it_was_not_indexed() {
    let dir = tempfile::tempdir().unwrap();
    let out = std::process::Command::new(bin())
        .args([
            "plan",
            "render",
            &fixture("minimal.json"),
            "--out",
            "plan.html",
            "--no-open",
            "--json",
        ])
        .current_dir(dir.path())
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], true, "the render itself is fine");
    assert!(dir.path().join("plan.html").is_file());
    assert_eq!(v["index"]["recorded"], false);
    let reason = v["index"]["reason"].as_str().unwrap();
    assert!(reason.contains("git repository"), "{reason}");
    assert!(
        v["index"].get("poster").is_none(),
        "nothing advertised that was not written: {v}"
    );

    // And in text mode the note is on stderr, beside the render.
    let out = std::process::Command::new(bin())
        .args([
            "plan",
            "render",
            &fixture("minimal.json"),
            "--out",
            "plan.html",
            "--no-open",
        ])
        .current_dir(dir.path())
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .output()
        .unwrap();
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not added to the artifact index"),
        "{stderr}"
    );
}

#[test]
fn a_newer_index_is_left_alone_by_render_and_reported_by_list() {
    let repo = Repo::new();
    let plan = plan_in(&repo, "minimal.json", "plan.json");
    let index_path = repo.state_dir().join("index.json");
    std::fs::create_dir_all(repo.state_dir()).unwrap();
    let future =
        r#"{"format":"artefacto.index/2","artifacts":[{"id":"plan:future","shape":"unknown"}]}"#;
    std::fs::write(&index_path, future).unwrap();

    let out = render(&repo, &plan, "plan.html");
    assert_eq!(out["ok"], true);
    assert_eq!(out["index"]["recorded"], false);
    assert!(
        out["index"]["reason"]
            .as_str()
            .unwrap()
            .contains("newer artefacto"),
        "{out}"
    );
    assert_eq!(
        std::fs::read_to_string(&index_path).unwrap(),
        future,
        "the bytes are preserved"
    );
    assert!(
        !repo.state_dir().join("posters").exists(),
        "no poster for a row that was not written"
    );

    let listed = list(&repo);
    assert_eq!(listed["readonly"], true);
    assert_eq!(listed["artifacts"].as_array().unwrap().len(), 0);
    let text = repo.run(&["list"]);
    text.success();
    assert!(text.stderr.contains("newer artefacto"), "{}", text.stderr);
}

#[test]
fn a_corrupt_index_is_reported_and_kept_aside_when_the_next_render_repairs_it() {
    let repo = Repo::new();
    let plan = plan_in(&repo, "minimal.json", "plan.json");
    let index_path = repo.state_dir().join("index.json");
    std::fs::create_dir_all(repo.state_dir()).unwrap();
    std::fs::write(&index_path, "{this is not").unwrap();

    let listed = list(&repo);
    assert_eq!(listed["readonly"], false, "corrupt is not newer");
    assert_eq!(listed["corrupt"], true, "{listed}");
    assert_eq!(listed["artifacts"].as_array().unwrap().len(), 0);
    let text = repo.run(&["list"]);
    text.success();
    assert!(
        text.stderr.contains("could not be read"),
        "a reader is told: {}",
        text.stderr
    );
    assert!(
        !text.stdout.contains("no artifacts yet"),
        "and not told the index is empty: {}",
        text.stdout
    );

    let out = render(&repo, &plan, "plan.html");
    assert_eq!(out["index"]["recorded"], true, "{out}");
    let repaired: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&index_path).unwrap()).expect("repaired");
    assert_eq!(repaired["format"], "artefacto.index/1");
    assert_eq!(
        std::fs::read_to_string(repo.state_dir().join("index.json.corrupt")).unwrap(),
        "{this is not",
        "the old bytes are kept aside, not thrown away"
    );
    let listed = list(&repo);
    assert_eq!(listed["corrupt"], false);
    assert_eq!(listed["artifacts"].as_array().unwrap().len(), 1);
}

#[test]
fn one_unreadable_row_keeps_every_other_row_and_is_reported() {
    // Spec 4.4: never auto-prune. A row this binary cannot read costs that
    // row's listing and nothing else; it is written back as it was.
    let repo = Repo::new();
    let plan = plan_in(&repo, "minimal.json", "plan.json");
    let index_path = repo.state_dir().join("index.json");
    std::fs::create_dir_all(repo.state_dir()).unwrap();
    let good = serde_json::json!({
        "id": "plan:good", "kind": "plan", "title": "Good", "plan_hash": "sha256:g",
        "source_path": "/tmp/good.json", "revision": 2, "revised_at": "2026-09-10T00:00:00Z",
        "recorded_at": "2026-09-10T00:00:00Z", "open_threads": 1, "verdict": "approve",
    });
    let bad = serde_json::json!({ "id": "plan:odd", "shape": "unknown" });
    std::fs::write(
        &index_path,
        serde_json::json!({ "format": "artefacto.index/1", "artifacts": [good, bad] }).to_string(),
    )
    .unwrap();

    let listed = list(&repo);
    assert_eq!(listed["corrupt"], false);
    assert_eq!(listed["unreadable_rows"], 1, "{listed}");
    let rows = listed["artifacts"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], "plan:good");
    assert_eq!(rows[0]["verdict"], "approve");
    let text = repo.run(&["list"]);
    text.success();
    assert!(
        text.stderr
            .contains("1 row in index.json could not be read"),
        "{}",
        text.stderr
    );
    assert!(text.stdout.contains("plan:good"), "{}", text.stdout);

    render(&repo, &plan, "plan.html");
    let file: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&index_path).unwrap()).unwrap();
    let ids: Vec<&str> = file["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        ["plan:demo", "plan:good", "plan:odd"],
        "nothing lost: {file}"
    );
    assert_eq!(
        file["artifacts"][2],
        serde_json::json!({ "id": "plan:odd", "shape": "unknown" }),
        "the row it could not read, byte for byte"
    );
    assert_eq!(list(&repo)["artifacts"].as_array().unwrap().len(), 2);
}

#[test]
fn a_missing_source_greys_the_row_and_is_never_removed() {
    let repo = Repo::new();
    let gone = plan_in(&repo, "minimal.json", "gone.json");
    render(&repo, &gone, "gone.html");
    std::fs::remove_file(&gone).unwrap();

    // Another record, which is when pruning would happen if it happened.
    let kept = plan_in(&repo, "kitchen-sink.json", "kept.json");
    render(&repo, &kept, "kept.html");

    let rows = list(&repo)["artifacts"].clone();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 2, "the row whose file is gone is still listed");
    let row = rows
        .iter()
        .find(|r| r["id"] == "plan:demo")
        .expect("the greyed row");
    assert_eq!(row["source_exists"], false);
    assert_eq!(row["source_path"], gone);

    let text = repo.run(&["list"]);
    text.success();
    assert!(
        text.stdout.contains(&format!("{gone}  (missing)")),
        "{}",
        text.stdout
    );
}

#[test]
fn list_orders_newest_first_and_a_re_render_moves_its_row_up() {
    let repo = Repo::new();
    let a = plan_in(&repo, "minimal.json", "a.json");
    let b = plan_in(&repo, "kitchen-sink.json", "b.json");
    render(&repo, &a, "a.html");
    render(&repo, &b, "b.html");
    let ids = |v: &serde_json::Value| {
        v["artifacts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&list(&repo)), ["plan:auth-refactor", "plan:demo"]);

    render(&repo, &a, "a.html");
    assert_eq!(
        ids(&list(&repo)),
        ["plan:demo", "plan:auth-refactor"],
        "one row per artifact, and the latest render leads"
    );
}

#[test]
fn list_computes_ages_from_the_file_either_side_of_a_day() {
    let repo = Repo::new();
    let plan = plan_in(&repo, "minimal.json", "plan.json");
    render(&repo, &plan, "plan.html");
    let index_path = repo.state_dir().join("index.json");
    let mut file: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&index_path).unwrap()).unwrap();
    let now = artefacto::time::now_secs();
    let ago = |secs: u64| artefacto::time::rfc3339(now - secs);

    for (age, label) in [
        (23 * 3600, "23 hours ago"),
        (25 * 3600, "yesterday"),
        (49 * 3600, "2 days ago"),
    ] {
        file["artifacts"][0]["revised_at"] = serde_json::json!(ago(age));
        std::fs::write(&index_path, file.to_string()).unwrap();
        let listed = list(&repo);
        assert_eq!(listed["artifacts"][0]["age"], label);
        assert_eq!(
            listed["artifacts"][0]["revised_at"],
            ago(age),
            "the absolute time too"
        );
        let text = repo.run(&["list"]);
        assert!(text.stdout.contains(label), "{}", text.stdout);
    }
}

#[test]
fn list_with_nothing_recorded_says_so() {
    let repo = Repo::new();
    let listed = list(&repo);
    assert_eq!(listed["ok"], true);
    assert_eq!(listed["artifacts"].as_array().unwrap().len(), 0);
    let text = repo.run(&["list"]);
    text.success();
    assert!(text.stdout.contains("no artifacts yet"), "{}", text.stdout);
}

#[test]
fn list_outside_a_repository_is_a_usage_error() {
    let dir = tempfile::tempdir().unwrap();
    let out = std::process::Command::new(bin())
        .args(["list"])
        .current_dir(dir.path())
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("git repository"));
}

// ---------------------------------------------------------------------------
// The server's half: a push records the row, and the review keeps it current.
// ---------------------------------------------------------------------------

use support::InProcess;

struct Live {
    repo: Repo,
    server: InProcess,
    plan: String,
    session: String,
}

fn pushed(fixture_name: &str) -> Live {
    let repo = Repo::new();
    let server = InProcess::start_in(&repo);
    let plan = plan_in(&repo, fixture_name, "plan.json");
    let out = repo.json(&["plan", "push", &plan, "--json", "--no-open"]);
    Live {
        repo,
        server,
        plan,
        session: out["session"].as_str().expect("a session").to_string(),
    }
}

impl Live {
    /// The one row, since these tests push one plan.
    fn row(&self) -> serde_json::Value {
        let listed = list(&self.repo);
        let rows = listed["artifacts"].as_array().expect("rows");
        assert_eq!(rows.len(), 1, "one row per artifact: {listed}");
        rows[0].clone()
    }

    fn poster(&self) -> String {
        let row = self.row();
        let path = row["poster"].as_str().expect("a poster path");
        std::fs::read_to_string(path).expect("the poster is on disk")
    }

    fn open_thread(&self, artifact: &str, client: &str, target: &str, blocking: bool) -> String {
        let cookie = self.server.session_cookie(artifact);
        let opened = self.server.post_cmd(
            &cookie,
            artifact,
            serde_json::json!({
                "cmd": "thread.open", "client_id": client, "ref": target,
                "text": format!("about {target}"), "blocking": blocking, "opened_revision": 1,
            }),
        );
        opened["assigned"].as_str().expect("an id").to_string()
    }

    fn push_again(&self, base: u32) -> serde_json::Value {
        let base = base.to_string();
        self.repo.json(&[
            "plan",
            "push",
            &self.plan,
            "--json",
            "--no-open",
            "--session",
            &self.session,
            "--base-revision",
            &base,
        ])
    }
}

#[test]
fn push_records_the_revision_and_every_later_change_to_the_review() {
    let l = pushed("minimal.json");
    let row = l.row();
    assert_eq!(row["id"], "plan:demo");
    assert_eq!(row["revision"], 1);
    assert_eq!(row["source_path"], l.plan);
    assert_eq!(
        row["rendered_path"],
        serde_json::Value::Null,
        "never rendered statically"
    );
    let status = l.repo.json(&["status", "--json"]);
    assert_eq!(
        row["revised_at"], status["artifacts"][0]["revised_at"],
        "the row's time is the revision event's own"
    );
    assert!(
        artefacto::time::parse_rfc3339(row["revised_at"].as_str().unwrap()).is_some(),
        "and it is a real timestamp: {row}"
    );
    assert_eq!(row["age"], "just now");
    let poster = l.poster();
    assert!(poster.contains(">rev 1<"), "{poster}");
    assert!(poster.contains("0 open · in review"), "{poster}");

    let thread = l.open_thread("plan:demo", "cid-1", "task:t-a", true);
    assert_eq!(l.row()["open_threads"], 1, "a thread opened by the page");
    assert!(l.poster().contains("1 open · in review"));

    l.repo
        .run(&["resolve", &thread, "--session", &l.session, "--changed"])
        .success();
    assert_eq!(l.row()["open_threads"], 0, "resolved by the agent");

    let cookie = l.server.session_cookie("plan:demo");
    let second = l.open_thread("plan:demo", "cid-3", "phase:p-one", false);
    assert_eq!(l.row()["open_threads"], 1);
    l.server.post_cmd(
        &cookie,
        "plan:demo",
        serde_json::json!({ "cmd": "thread.delete", "client_id": "cid-4", "thread": second }),
    );
    assert_eq!(l.row()["open_threads"], 0, "deleted by the reviewer");

    l.server.post_cmd(
        &cookie,
        "plan:demo",
        serde_json::json!({
            "cmd": "review.submit", "client_id": "cid-2",
            "verdict": "approve", "base_revision": 1,
        }),
    );
    let row = l.row();
    assert_eq!(row["verdict"], "approve");
    assert_eq!(row["submitted"], true);
    assert!(l.poster().contains("0 open · approved"));
    let status = l.repo.json(&["status", "--json"]);
    assert_eq!(status["artifacts"][0]["verdict"], "approve", "{status}");

    // A second revision reopens the review and keeps the last verdict.
    let out = l.push_again(1);
    assert_eq!(out["revision"], 2);
    let row = l.row();
    assert_eq!(row["revision"], 2);
    assert_eq!(row["submitted"], false);
    assert_eq!(row["verdict"], "approve", "the last verdict, still");
    assert!(l.poster().contains(">rev 2<"));
    assert!(
        row["revised_at"].as_str().unwrap()
            >= status["artifacts"][0]["revised_at"].as_str().unwrap(),
        "revised_at moves with the revision"
    );
}

#[test]
fn a_static_render_and_its_push_are_one_row_that_keeps_the_rendered_path() {
    let repo = Repo::new();
    let plan = plan_in(&repo, "minimal.json", "plan.json");
    render(&repo, &plan, "plan.html");
    let _server = InProcess::start_in(&repo);
    repo.json(&["plan", "push", &plan, "--json", "--no-open"]);

    let listed = list(&repo);
    let rows = listed["artifacts"].as_array().unwrap();
    assert_eq!(
        rows.len(),
        1,
        "the same plan, rendered then pushed, is one row"
    );
    assert_eq!(rows[0]["revision"], 1);
    assert_eq!(
        rows[0]["rendered_path"],
        real(&repo).join("plan.html").display().to_string(),
        "the push does not forget where the static page was written"
    );
    let poster = std::fs::read_to_string(rows[0]["poster"].as_str().unwrap()).unwrap();
    assert!(poster.contains(">rev 1<"), "the push redrew the poster");
}

#[test]
fn a_thread_whose_element_a_push_removed_counts_as_unanchored() {
    let l = pushed("kitchen-sink.json");
    l.open_thread("plan:auth-refactor", "cid-1", "task:t-bench", false);
    assert_eq!(l.row()["open_threads"], 1);

    let text = std::fs::read_to_string(&l.plan).unwrap();
    let without = text.replace(
        r#",
        { "id": "t-bench", "title": "Benchmarks", "status": "cut", "risk": "low" }"#,
        "",
    );
    assert_ne!(text, without, "the edit found its line");
    std::fs::write(&l.plan, without).unwrap();
    let out = l.push_again(1);
    assert_eq!(out["revision"], 2, "{out}");

    let row = l.row();
    assert_eq!(row["open_threads"], 0);
    assert_eq!(row["unanchored_threads"], 1);
    assert!(l.poster().contains("1 unanchored"));
}

#[test]
fn a_stored_plan_with_a_field_this_binary_does_not_know_still_has_a_row() {
    // A newer artefacto pushed it; this one serves it leniently (see the
    // page tests) and must index it the same way, or the newest artifact is
    // the one missing from the index.
    let repo = Repo::new();
    let server = InProcess::start_in(&repo);
    {
        use artefacto::server::event::Actor;
        use artefacto::server::http::Committer;
        let c = Committer::open(&server.shared);
        c.append(
            "plan:future",
            1,
            Actor::Agent,
            "revision.published",
            serde_json::json!({
                "plan": {
                    "format": "artefacto.plan/1",
                    "meta": { "id": "future", "title": "From the future", "mood": "calm" },
                    "phases": [{ "id": "p-one", "title": "Phase one", "tasks": [
                        { "id": "t-a", "title": "Task A" }
                    ] }]
                },
                "plan_hash": "sha256:future",
                "source_path": "/tmp/future.json",
                "summary": "first"
            }),
        )
        .expect("seed");
    }
    let listed = list(&repo);
    let rows = listed["artifacts"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{listed}");
    assert_eq!(rows[0]["id"], "plan:future");
    assert_eq!(rows[0]["title"], "From the future");
    assert_eq!(rows[0]["revision"], 1);
    let poster = std::fs::read_to_string(rows[0]["poster"].as_str().unwrap()).unwrap();
    assert!(poster.contains("From the future"), "{poster}");
}

#[test]
fn a_render_of_a_live_artifact_keeps_the_reviews_facts() {
    // A render knows the plan and nothing of the review; the row is one
    // row, and the review's facts are the server's to change.
    let l = pushed("minimal.json");
    let thread = l.open_thread("plan:demo", "cid-1", "task:t-a", true);
    let before = l.row();
    assert_eq!(before["open_threads"], 1);
    // Backdate the revision in the file, so "the render's time" and "the
    // revision's time" cannot coincide within one second.
    let index_path = l.repo.state_dir().join("index.json");
    let mut file: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&index_path).unwrap()).unwrap();
    file["artifacts"][0]["revised_at"] = serde_json::json!("2026-08-01T00:00:00Z");
    std::fs::write(&index_path, file.to_string()).unwrap();

    let out = render(&l.repo, &l.plan, "plan.html");
    assert_eq!(out["index"]["recorded"], true, "{out}");
    let row = l.row();
    assert_eq!(row["revision"], 1, "still the pushed revision: {row}");
    assert_eq!(row["open_threads"], 1);
    assert_eq!(
        row["revised_at"], "2026-08-01T00:00:00Z",
        "the revision's time, not the render's"
    );
    assert_eq!(row["age"], "on 2026-08-01");
    assert_eq!(
        row["rendered_path"],
        real(&l.repo).join("plan.html").display().to_string()
    );
    assert_eq!(row["plan_hash"], before["plan_hash"]);
    let poster = l.poster();
    assert!(poster.contains(">rev 1<"), "{poster}");
    assert!(poster.contains("1 open · in review"), "{poster}");

    // The same after a verdict.
    l.repo
        .run(&["resolve", &thread, "--session", &l.session, "--changed"])
        .success();
    let cookie = l.server.session_cookie("plan:demo");
    l.server.post_cmd(
        &cookie,
        "plan:demo",
        serde_json::json!({
            "cmd": "review.submit", "client_id": "cid-2", "verdict": "approve", "base_revision": 1,
        }),
    );
    render(&l.repo, &l.plan, "plan.html");
    let row = l.row();
    assert_eq!(row["verdict"], "approve");
    assert_eq!(row["submitted"], true);
    assert_eq!(row["open_threads"], 0);
    assert!(l.poster().contains("0 open · approved"));

    // And a row that was never pushed still takes the render as its revision.
    let other = plan_in(&l.repo, "kitchen-sink.json", "other.json");
    render(&l.repo, &other, "other.html");
    let listed = list(&l.repo);
    let fresh = listed["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "plan:auth-refactor")
        .unwrap();
    assert_eq!(fresh["revision"], 0);
    assert_eq!(fresh["age"], "just now");
}

#[test]
fn a_server_over_a_newer_index_says_so_once_in_its_log() {
    // The server keeps rows current best effort; when it cannot, because a
    // newer artefacto wrote the file, that must not be silent. Against the
    // real daemon, whose stderr is server.log.
    let repo = Repo::new();
    let plan = plan_in(&repo, "minimal.json", "plan.json");
    repo.json(&["plan", "push", &plan, "--json", "--no-open"]);
    let index_path = repo.state_dir().join("index.json");
    let future = r#"{"format":"artefacto.index/2","artifacts":[]}"#;
    std::fs::write(&index_path, future).unwrap();

    // Two row-changing events: the note is said once, not per commit.
    let out = repo.json(&["open", "--json"]);
    let port = repo.port();
    let path = out["url"]
        .as_str()
        .unwrap()
        .strip_prefix(&format!("http://127.0.0.1:{port}"))
        .unwrap()
        .to_string();
    let response = support::get(port, &path);
    let cookie = response
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("set-cookie:"))
        .and_then(|l| l.split_once(": ").map(|(_, v)| v))
        .and_then(|v| v.split(';').next())
        .expect("a cookie")
        .trim()
        .to_string();
    for client in ["c1", "c2"] {
        let body = serde_json::json!({
            "cmd": "thread.open", "client_id": client, "ref": "task:t-a",
            "text": "x", "opened_revision": 1,
        })
        .to_string();
        let response = support::raw(
            port,
            &format!(
                "POST /a/plan:demo/cmd HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nCookie: {cookie}\r\n\
                 Origin: http://127.0.0.1:{port}\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            ),
        );
        assert_eq!(support::status_of(&response), 200, "{response}");
    }
    repo.stop();
    assert_eq!(
        std::fs::read_to_string(&index_path).unwrap(),
        future,
        "never rewritten"
    );
    let log = std::fs::read_to_string(repo.state_dir().join("server.log")).unwrap();
    assert_eq!(
        log.matches("newer artefacto").count(),
        1,
        "said once: {log}"
    );
}

#[test]
fn list_over_only_unreadable_rows_says_so_and_not_no_artifacts() {
    let repo = Repo::new();
    let index_path = repo.state_dir().join("index.json");
    std::fs::create_dir_all(repo.state_dir()).unwrap();
    std::fs::write(
        &index_path,
        r#"{"format":"artefacto.index/1","artifacts":[{"id":"plan:demo","revision":"bad"}]}"#,
    )
    .unwrap();
    let text = repo.run(&["list"]);
    text.success();
    assert!(
        text.stderr
            .contains("1 row in index.json could not be read"),
        "{}",
        text.stderr
    );
    assert!(
        !text.stdout.contains("no artifacts yet"),
        "the two would contradict: {}",
        text.stdout
    );
    let listed = list(&repo);
    assert_eq!(listed["unreadable_rows"], 1);
    assert_eq!(listed["artifacts"].as_array().unwrap().len(), 0);
}

#[test]
fn a_second_repair_keeps_the_first_kept_aside_copy() {
    let repo = Repo::new();
    let plan = plan_in(&repo, "minimal.json", "plan.json");
    let index_path = repo.state_dir().join("index.json");
    std::fs::create_dir_all(repo.state_dir()).unwrap();
    std::fs::write(&index_path, "{first").unwrap();
    render(&repo, &plan, "plan.html");
    std::fs::write(&index_path, "{second").unwrap();
    render(&repo, &plan, "plan.html");
    let dir = repo.state_dir();
    assert_eq!(
        std::fs::read_to_string(dir.join("index.json.corrupt")).unwrap(),
        "{first"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("index.json.corrupt.1")).unwrap(),
        "{second"
    );
    assert_eq!(list(&repo)["artifacts"].as_array().unwrap().len(), 1);
}
