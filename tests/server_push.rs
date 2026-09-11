//! `artefacto plan push`, driven as the real binary.
//!
//! Two rules carry this file.
//!
//! Spec 5's: **`--base-revision` comes from the caller**. It is the revision
//! the agent last saw. Reading the current revision at push time instead would
//! compare the server to itself and always pass, which would make the only
//! concurrency check in the system do nothing at all.
//!
//! And codex's #8: a revision and its resolutions are **one commit**. The log
//! test `an_interrupted_commit_is_dropped_whole` proves the framing; this file
//! proves push uses it.

mod support;

use std::path::{Path, PathBuf};
use support::{InProcess, Repo};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/plan")
        .join(name)
}

/// A plan file inside the repository, so `push` has something local to send.
fn plan_in(repo: &Repo, name: &str) -> String {
    let source = fixture(name);
    let target = repo.path().join("plan.json");
    std::fs::copy(&source, &target).expect("copy the fixture");
    target.display().to_string()
}

fn attached() -> (Repo, InProcess, String) {
    let repo = Repo::new();
    let server = InProcess::start_in(&repo);
    let plan = plan_in(&repo, "minimal.json");
    (repo, server, plan)
}

fn push(repo: &Repo, plan: &str, extra: &[&str]) -> support::Out {
    let mut args = vec!["plan", "push", plan, "--json", "--no-open"];
    args.extend_from_slice(extra);
    repo.run(&args)
}

fn push_json(repo: &Repo, plan: &str, extra: &[&str]) -> serde_json::Value {
    let out = push(repo, plan, extra);
    assert_eq!(out.code, 0, "push failed: {}", out.stderr);
    serde_json::from_str(&out.stdout).expect("json")
}

#[test]
fn the_first_push_needs_neither_flag_and_reports_the_whole_contract() {
    let (repo, _server, plan) = attached();
    let v = push_json(&repo, &plan, &[]);

    assert_eq!(v["revision"], 1);
    assert_eq!(v["artifact"], "plan:demo", "the id is plan:<meta.id>");
    assert!(
        v["url"].as_str().unwrap().contains("/b/"),
        "push hands back a bootstrap URL: {}",
        v["url"]
    );
    // Spec 5: every JSON result carries these.
    assert!(v["plan_hash"].as_str().unwrap().starts_with("sha256:"));
    assert_eq!(v["title"], "Demo plan");
    assert_eq!(v["phases"], 1);
    assert_eq!(v["tasks"], 1);
    assert!(
        v["session"].as_str().is_some_and(|s| !s.is_empty()),
        "push is often an agent's first command, so it hands back the token \
         every later mutation carries"
    );
    assert_eq!(v["summary"], "first revision");
}

#[test]
fn a_later_push_must_pass_base_revision_or_force() {
    let (repo, _server, plan) = attached();
    push_json(&repo, &plan, &[]);

    let out = push(&repo, &plan, &[]);
    assert_eq!(
        out.code, 2,
        "a usage error, even though the server is what noticed"
    );
    assert!(out.stderr.contains("--base-revision"), "{}", out.stderr);
}

#[test]
fn a_stale_base_revision_is_refused_with_exit_7() {
    let (repo, server, plan) = attached();
    push_json(&repo, &plan, &[]);
    push_json(&repo, &plan, &["--base-revision", "1"]);
    let before = server.last_seq();

    let out = push(&repo, &plan, &["--base-revision", "1"]);
    assert_eq!(out.code, 7);
    assert!(
        out.stderr.contains("status --json"),
        "the message says how to catch up: {}",
        out.stderr
    );
    assert_eq!(server.last_seq(), before, "and nothing was written");
}

#[test]
fn force_overrides_a_stale_base_revision() {
    let (repo, _server, plan) = attached();
    push_json(&repo, &plan, &[]);
    push_json(&repo, &plan, &["--base-revision", "1"]);
    let v = push_json(&repo, &plan, &["--force"]);
    assert_eq!(v["revision"], 3);
}

#[test]
fn an_invalid_plan_is_refused_and_nothing_is_logged() {
    let repo = Repo::new();
    let server = InProcess::start_in(&repo);
    let plan = plan_in(&repo, "invalid-cycle.json");
    let before = server.last_seq();

    let out = push(&repo, &plan, &[]);
    assert_eq!(out.code, 1);
    assert_eq!(
        server.last_seq(),
        before,
        "validation happens before anything reaches the log"
    );
}

#[test]
fn a_revision_event_carries_the_whole_plan_and_a_change_summary() {
    let (repo, server, plan) = attached();
    push_json(&repo, &plan, &[]);

    let e = server.last_event_of_type("revision.published");
    assert!(
        e["data"]["plan"]["phases"].is_array(),
        "not just a summary: spec 4.2 needs the log alone to rebuild the body"
    );
    assert!(e["data"]["plan_hash"]
        .as_str()
        .unwrap()
        .starts_with("sha256:"));
    assert!(
        e["data"]["summary"].is_string(),
        "spec 6.3: a revision carries a change summary"
    );
    assert_eq!(e["actor"], "agent");
}

#[test]
fn a_summary_says_what_moved_between_revisions() {
    let (repo, server, plan) = attached();
    push_json(&repo, &plan, &[]);

    // Add a task, so the second revision has something to describe.
    let mut doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&plan).unwrap()).unwrap();
    doc["phases"][0]["tasks"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({ "id": "t-b", "title": "Task B" }));
    std::fs::write(&plan, doc.to_string()).unwrap();

    let v = push_json(&repo, &plan, &["--base-revision", "1"]);
    assert_eq!(v["summary"], "1 added");
    assert_eq!(
        server.last_event_of_type("revision.published")["data"]["summary"],
        "1 added"
    );
}

#[test]
fn a_revision_and_its_resolutions_commit_together() {
    let (repo, server, plan) = attached();
    let first = push_json(&repo, &plan, &[]);
    let session = first["session"].as_str().unwrap().to_string();

    let cookie = server.session_cookie("plan:demo");
    let open = |client_id: &str, target: &str| -> String {
        let reply = server.post_cmd(
            &cookie,
            "plan:demo",
            serde_json::json!({
                "cmd": "thread.open", "client_id": client_id,
                "ref": target, "text": "look at this", "opened_revision": 1,
            }),
        );
        assert_eq!(reply["ok"], true, "{reply}");
        reply["assigned"].as_str().expect("an id").to_string()
    };
    let t1 = open("c1", "task:t-a");
    let t2 = open("c2", "phase:p-one");

    let file = repo.path().join("resolutions.json");
    std::fs::write(
        &file,
        serde_json::json!([
            { "thread": t1, "status": "changed", "note": "fixed" },
            { "thread": t2, "status": "declined", "note": "out of scope" },
        ])
        .to_string(),
    )
    .unwrap();

    let before = server.last_seq();
    push_json(
        &repo,
        &plan,
        &[
            "--base-revision",
            "1",
            "--session",
            &session,
            "--resolutions",
            file.to_str().unwrap(),
        ],
    );

    assert_eq!(server.thread_status(&t1), "changed");
    assert_eq!(server.thread_status(&t2), "declined");
    assert_eq!(
        server.last_seq() - before,
        3,
        "one revision plus two resolutions"
    );
    let marks: Vec<serde_json::Value> = server
        .events_of_type("thread.resolved")
        .into_iter()
        .map(|e| e["batch"].clone())
        .collect();
    let revision = server.last_event_of_type("revision.published");
    assert_eq!(
        revision["batch"]["count"], 3,
        "appended as one commit, so a crash cannot leave the revision without them"
    );
    assert!(
        marks.iter().all(|m| m["id"] == revision["batch"]["id"]),
        "and all three name the same commit"
    );
}

#[test]
fn a_resolution_for_an_unknown_thread_is_refused_and_the_revision_with_it() {
    let (repo, server, plan) = attached();
    push_json(&repo, &plan, &[]);
    let file = repo.path().join("resolutions.json");
    std::fs::write(
        &file,
        serde_json::json!([{ "thread": "c-99", "status": "changed", "note": "" }]).to_string(),
    )
    .unwrap();
    let before = server.last_seq();

    let out = push(
        &repo,
        &plan,
        &[
            "--base-revision",
            "1",
            "--resolutions",
            file.to_str().unwrap(),
        ],
    );
    assert_ne!(out.code, 0);
    assert_eq!(
        server.last_seq(),
        before,
        "a bad resolution takes the whole push with it, rather than committing \
         a revision that answers nothing"
    );
}

#[test]
fn the_artifact_survives_a_restart_from_the_log_alone() {
    let (repo, server, plan) = attached();
    push_json(&repo, &plan, &[]);
    let before = artefacto::server::http::with_review(&server.shared, |r| r.snapshot());

    let server = server.restart();
    let after = artefacto::server::http::with_review(&server.shared, |r| r.snapshot());
    assert_eq!(before, after, "the log is the only source of truth");
    assert!(after["artifacts"]["plan:demo"]["plan"]["phases"].is_array());
}

#[test]
fn a_push_with_a_superseded_session_exits_6() {
    let (repo, _server, plan) = attached();
    let stale = push_json(&repo, &plan, &["--agent", "claude"])["session"]
        .as_str()
        .unwrap()
        .to_string();
    repo.run(&["await", "--timeout", "1s", "--agent", "codex", "--takeover"])
        .success();

    let out = push(&repo, &plan, &["--base-revision", "1", "--session", &stale]);
    assert_eq!(out.code, 6);
}

#[test]
fn a_push_while_another_agent_holds_the_lease_exits_6() {
    let (repo, _server, plan) = attached();
    repo.run(&["await", "--timeout", "1s", "--agent", "claude"])
        .success();

    let out = push(&repo, &plan, &["--agent", "codex"]);
    assert_eq!(out.code, 6);
    assert!(out.stderr.contains("claude"), "{}", out.stderr);
}

#[test]
fn a_pushed_revision_reaches_an_open_page_as_one_frame() {
    // Spec 4.3: the server sends "one snapshot holding the rendered body,
    // thread state, and resolutions together", because sending them separately
    // would let the reviewer see a body from one revision beside threads from
    // another.
    let (repo, server, plan) = attached();
    let first = push_json(&repo, &plan, &[]);
    let session = first["session"].as_str().unwrap().to_string();

    let cookie = server.session_cookie("plan:demo");
    let reply = server.post_cmd(
        &cookie,
        "plan:demo",
        serde_json::json!({
            "cmd": "thread.open", "client_id": "c1",
            "ref": "task:t-a", "text": "why", "opened_revision": 1,
        }),
    );
    let thread = reply["assigned"].as_str().unwrap().to_string();

    let mut page = server.connect_page();
    page.hello();

    let file = repo.path().join("resolutions.json");
    std::fs::write(
        &file,
        serde_json::json!([{ "thread": thread, "status": "changed", "note": "done" }]).to_string(),
    )
    .unwrap();
    push_json(
        &repo,
        &plan,
        &[
            "--base-revision",
            "1",
            "--session",
            &session,
            "--resolutions",
            file.to_str().unwrap(),
        ],
    );

    let frame = page.next_frame();
    let events = frame["events"].as_array().expect("events");
    assert_eq!(events.len(), 2, "one frame, not two: {frame}");
    assert_eq!(events[0]["type"], "revision.published");
    assert_eq!(events[1]["type"], "thread.resolved");
}

#[test]
fn push_starts_a_server_when_none_is_running() {
    // Push is the first command an agent runs, so exiting 4 here would only
    // mean "run serve and try again" for no reason.
    let repo = Repo::new();
    let plan = plan_in(&repo, "minimal.json");
    let out = push(&repo, &plan, &[]);
    repo.stop();

    assert_eq!(out.code, 0, "{}", out.stderr);
    let v: serde_json::Value = serde_json::from_str(&out.stdout).expect("json");
    assert_eq!(v["revision"], 1);
}

#[test]
fn a_token_superseded_during_validation_is_refused_inside_the_gate() {
    // `handle_push` claims the lease, then parses and validates a whole plan
    // with no lock held, then appends. A `--takeover` that lands in that
    // window has to be caught at the append, or a stale agent publishes. Spec
    // 4.2: "a token from a superseded generation is refused. Without this a
    // stale agent that lost the lease could still write." `reply` and
    // `resolve` did this from the start; push, the biggest mutation, did not.
    use artefacto::server::lease::{self, Claim};
    use artefacto::server::push::{self, PushBody};

    let server = InProcess::start();
    let claude = lease::acquire(&server.shared, Claim::waiting("claude")).unwrap();
    lease::acquire(&server.shared, Claim::waiting("codex").with_takeover(true)).unwrap();
    let plan: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(fixture("minimal.json")).unwrap()).unwrap();
    let before = server.last_seq();

    let refused = push::commit(
        &server.shared,
        &claude,
        &PushBody {
            plan,
            ..Default::default()
        },
    )
    .expect_err("a superseded token must not publish");
    assert!(
        matches!(refused, push::Refusal::Lease(lease::LeaseError::Superseded)),
        "{refused:?}"
    );
    assert_eq!(server.last_seq(), before, "and nothing was appended");
}

// --- what the page receives -------------------------------------------------

#[test]
fn a_push_frame_carries_the_rendered_body_to_pages_and_nothing_to_agents() {
    // Spec 4.3: the page gets one snapshot holding the rendered body, the
    // thread state, and the resolutions together. The body rides on the frame
    // the socket delivers; the agent's frame, which comes from the log, never
    // carries it.
    let (repo, server, plan) = attached();
    push_json(&repo, &plan, &[]);
    let cookie = server.session_cookie("plan:demo");
    let opened = server.post_cmd(
        &cookie,
        "plan:demo",
        serde_json::json!({
            "cmd": "thread.open", "client_id": "cid-1", "ref": "task:t-a",
            "text": "x", "opened_revision": 1,
        }),
    );
    let thread = opened["assigned"].as_str().unwrap().to_string();

    let mut page = server.connect_page();
    page.hello();
    let resolutions = repo.path().join("resolutions.json");
    std::fs::write(
        &resolutions,
        serde_json::json!([{ "thread": thread, "status": "changed", "note": "done" }]).to_string(),
    )
    .unwrap();
    // The same plan, retitled: a second revision of the same artifact.
    let second = repo.path().join("plan.json");
    let revised = std::fs::read_to_string(&second)
        .unwrap()
        .replace("Demo plan", "Demo plan, revised");
    std::fs::write(&second, revised).unwrap();
    let v = push_json(
        &repo,
        second.to_str().unwrap(),
        &[
            "--base-revision",
            "1",
            "--resolutions",
            resolutions.to_str().unwrap(),
        ],
    );
    assert_eq!(v["revision"], 2);

    let frame = page.next_frame();
    assert_eq!(frame["events"][0]["type"], "revision.published");
    assert_eq!(frame["events"][1]["type"], "thread.resolved");
    let html = frame["html"]
        .as_str()
        .expect("the rendered body rides with the frame");
    assert!(html.starts_with("<body"));
    assert!(
        html.contains("Demo plan, revised"),
        "the body is the new revision's"
    );
    assert!(html.contains("id=\"plan-data\""));
    assert!(!html.contains("<style"));
    assert!(
        frame["events"][0]["data"].get("html").is_none(),
        "the event itself is what the log holds"
    );

    let out = repo.run(&["await", "--timeout", "2s", "--agent", "codex", "--takeover"]);
    let r: serde_json::Value = serde_json::from_str(out.success().stdout.trim()).unwrap();
    assert!(
        r.get("html").is_none(),
        "an agent's frame comes from the log and carries no body"
    );
    assert!(
        !out.stdout.contains("<body"),
        "and no rendered markup leaks into the agent's transcript"
    );
    let _ = server;
}
