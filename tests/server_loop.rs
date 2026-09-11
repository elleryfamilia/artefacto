//! The loop, end to end: an agent publishes, a reviewer asks, the agent
//! answers, the reviewer submits, and the feedback document lands on disk.
//!
//! Everything here drives the real binary for the agent's half and the real
//! HTTP command protocol for the reviewer's. The page is still a fake client —
//! that is plan 3's gap, and it is the one thing these tests do not prove.

mod support;

use artefacto::server::presence::{self, Nudges};
use std::path::{Path, PathBuf};
use std::time::Duration;
use support::{InProcess, Repo};

fn plan_in(repo: &Repo) -> String {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plan/minimal.json");
    let target = repo.path().join("plan.json");
    std::fs::copy(&source, &target).expect("copy the fixture");
    target.display().to_string()
}

struct Loop {
    repo: Repo,
    server: InProcess,
    session: String,
}

fn start() -> Loop {
    start_with(Nudges::off())
}

fn start_with(nudges: Nudges) -> Loop {
    let repo = Repo::new();
    let server = InProcess::start_in_with(&repo, nudges);
    let plan = plan_in(&repo);
    let pushed = repo.run(&["plan", "push", &plan, "--json", "--no-open"]);
    assert_eq!(pushed.code, 0, "{}", pushed.stderr);
    let result: serde_json::Value = serde_json::from_str(&pushed.stdout).expect("json");
    let session = result["session"].as_str().expect("a session").to_string();
    Loop {
        repo,
        server,
        session,
    }
}

impl Loop {
    fn cookie(&self) -> String {
        self.server.session_cookie("plan:demo")
    }

    fn feedback_path(&self) -> PathBuf {
        self.repo.path().join("plan-feedback.json")
    }
}

#[test]
fn the_whole_loop_runs_once_through() {
    let l = start();
    let cookie = l.cookie();

    // The reviewer comments on a task and asks the agent about it.
    let opened = l.server.post_cmd(
        &cookie,
        "plan:demo",
        serde_json::json!({
            "cmd": "thread.open", "client_id": "cid-1", "ref": "task:t-a",
            "text": "why a trait here?", "blocking": true, "opened_revision": 1,
        }),
    );
    let thread = opened["assigned"].as_str().expect("an id").to_string();
    assert_eq!(thread, "c-1", "ids are server-assigned and stable");
    l.server.post_cmd(
        &cookie,
        "plan:demo",
        serde_json::json!({
            "cmd": "chat.send", "client_id": "cid-2", "thread": thread,
            "text": "and is it worth it?", "opened_revision": 1,
        }),
    );

    // The agent hears it.
    let heard = l
        .repo
        .run(&["await", "--timeout", "5s", "--session", &l.session]);
    let heard: serde_json::Value = serde_json::from_str(heard.success().stdout.trim()).unwrap();
    assert_eq!(heard["status"], "chat");
    let events = heard["events"].as_array().unwrap();
    assert_eq!(
        events.last().unwrap()["data"]["text"],
        "and is it worth it?"
    );

    // The agent answers, and the reviewer's page sees it without a reload.
    let mut page = l.server.connect_page();
    page.hello();
    l.repo
        .run(&[
            "reply",
            "--session",
            &l.session,
            "--thread",
            &thread,
            "so Redis can slot in",
        ])
        .success();
    let frame = page.next_frame();
    assert_eq!(frame["events"][0]["type"], "thread.replied");
    assert_eq!(frame["events"][0]["data"]["text"], "so Redis can slot in");

    // The agent resolves it and pushes the next revision.
    l.repo
        .run(&[
            "resolve",
            &thread,
            "--session",
            &l.session,
            "--changed",
            "--note",
            "swapped the concrete type out",
        ])
        .success();
    assert_eq!(l.server.thread_status(&thread), "changed");

    // The reviewer submits.
    l.server.post_cmd(
        &cookie,
        "plan:demo",
        serde_json::json!({
            "cmd": "review.submit", "client_id": "cid-3",
            "verdict": "approve", "base_revision": 1,
        }),
    );

    let submitted = l
        .repo
        .run(&["await", "--timeout", "5s", "--session", &l.session]);
    let submitted: serde_json::Value =
        serde_json::from_str(submitted.success().stdout.trim()).unwrap();
    assert_eq!(submitted["status"], "submitted");
    let last = submitted["events"].as_array().unwrap().last().unwrap();
    let path = last["data"]["path"]
        .as_str()
        .expect("the event names the file");

    // And the file-based loop keeps working.
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(doc["format"], "artefacto.feedback/1");
    assert_eq!(doc["verdict"], "approve");
    assert_eq!(doc["base_revision"], 1);
    assert_eq!(doc["plan_id"], "demo");
    assert_eq!(doc["comments"][0]["id"], "c-1");
    assert_eq!(doc["comments"][0]["ref"], "task:t-a");
    assert_eq!(doc["comments"][0]["status"], "changed");
    assert_eq!(doc["comments"][0]["blocking"], true);
    assert_eq!(
        doc["comments"][0]["replies"][0]["text"],
        "and is it worth it?"
    );
    assert!(doc["answers"].is_array());
    assert!(doc["reviewed"].is_array());
}

#[test]
fn the_feedback_file_is_written_before_the_event_that_names_it() {
    // An earlier draft appended the event, then computed the path, then put
    // the path into the already-appended event.
    let l = start();
    l.server.post_cmd(
        &l.cookie(),
        "plan:demo",
        serde_json::json!({
            "cmd": "review.submit", "client_id": "cid-1",
            "verdict": "request_changes", "base_revision": 1,
        }),
    );

    let e = l.server.last_event_of_type("review.submitted");
    let path = e["data"]["path"]
        .as_str()
        .expect("the event carries a path");
    assert_eq!(
        Path::new(path),
        std::fs::canonicalize(l.feedback_path()).unwrap(),
        "push canonicalizes the plan's path, so the file lands beside the real one"
    );
    assert!(
        Path::new(path).exists(),
        "the file was already there when the event was written"
    );
    assert_eq!(
        e["data"]["feedback"]["format"], "artefacto.feedback/1",
        "and the event carries the document itself, so an agent needs no disk"
    );
}

#[test]
fn a_feedback_write_leaves_no_partial_file() {
    let l = start();
    l.server.post_cmd(
        &l.cookie(),
        "plan:demo",
        serde_json::json!({
            "cmd": "review.submit", "client_id": "cid-1",
            "verdict": "approve", "base_revision": 1,
        }),
    );

    let names: Vec<String> = std::fs::read_dir(l.repo.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.contains("feedback"))
        .collect();
    assert_eq!(names, ["plan-feedback.json"], "a rename leaves no .tmp");
}

#[test]
fn an_open_blocking_comment_turns_a_comment_verdict_into_request_changes() {
    let l = start();
    let cookie = l.cookie();
    l.server.post_cmd(
        &cookie,
        "plan:demo",
        serde_json::json!({
            "cmd": "thread.open", "client_id": "cid-1", "ref": "task:t-a",
            "text": "this blocks", "blocking": true, "opened_revision": 1,
        }),
    );
    l.server.post_cmd(
        &cookie,
        "plan:demo",
        serde_json::json!({
            "cmd": "review.submit", "client_id": "cid-2",
            "verdict": "comment", "base_revision": 1,
        }),
    );

    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(l.feedback_path()).unwrap()).unwrap();
    assert_eq!(doc["verdict"], "request_changes");
}

#[test]
fn a_resubmit_with_the_same_client_id_does_not_duplicate_the_review() {
    let l = start();
    let cookie = l.cookie();
    let submit = serde_json::json!({
        "cmd": "review.submit", "client_id": "cid-1",
        "verdict": "approve", "base_revision": 1,
    });
    l.server.post_cmd(&cookie, "plan:demo", submit.clone());
    let after = l.server.last_seq();

    l.server.post_cmd(&cookie, "plan:demo", submit);
    assert_eq!(
        l.server.last_seq(),
        after,
        "a disconnect around submit must not duplicate it"
    );
}

// --- presence ---------------------------------------------------------------

#[test]
fn taking_and_releasing_the_lease_announces_presence_to_the_page() {
    let server = InProcess::start();
    server.seed_artifact();
    let mut page = server.connect_page();
    page.hello();

    let session = artefacto::server::lease::acquire(
        &server.shared,
        artefacto::server::lease::Claim::waiting("claude"),
    )
    .unwrap();
    let attached = page.next_frame();
    assert_eq!(attached["events"][0]["type"], "agent.attached");
    assert_eq!(attached["events"][0]["data"]["mode"], "waiting");
    assert_eq!(attached["events"][0]["data"]["agent"], "claude");

    artefacto::server::lease::release(&server.shared, &session.token);
    assert_eq!(page.next_frame()["events"][0]["type"], "agent.detached");
}

#[test]
fn presence_is_announced_and_never_logged() {
    // A logged `agent.attached` would mean the next server to read this log
    // tells a page an agent is here that left hours ago.
    let server = InProcess::start();
    server.seed_artifact();
    let session = artefacto::server::lease::acquire(
        &server.shared,
        artefacto::server::lease::Claim::waiting("claude"),
    )
    .unwrap();
    artefacto::server::lease::release(&server.shared, &session.token);

    assert_eq!(server.count_events("agent.attached"), 0);
    assert_eq!(server.count_events("agent.detached"), 0);
    assert_eq!(
        server.count_events("lease.taken"),
        1,
        "the lease itself is state and is logged"
    );
}

#[test]
fn re_polling_does_not_repaint_the_presence_pill() {
    // Spec 4.2: "the pill changes only when the lease changes hands or
    // expires", which is why presence is derived from the lease rather than
    // from traffic.
    let server = InProcess::start();
    server.seed_artifact();
    let mut page = server.connect_page();
    page.hello();
    let session = artefacto::server::lease::acquire(
        &server.shared,
        artefacto::server::lease::Claim::waiting("claude"),
    )
    .unwrap();
    assert_eq!(page.next_frame()["events"][0]["type"], "agent.attached");

    for _ in 0..3 {
        artefacto::server::lease::acquire(
            &server.shared,
            artefacto::server::lease::Claim::waiting("claude").with_token(Some(&session.token)),
        )
        .unwrap();
    }
    assert!(
        page.no_frame_within(Duration::from_millis(200)),
        "a poll that changed nothing must not reach the page"
    );
}

#[test]
fn a_nudge_reaches_the_page_as_a_banner_and_is_not_logged() {
    let l = start();
    let mut page = l.server.connect_page();
    page.hello();

    l.repo
        .run(&[
            "reply",
            "--session",
            &l.session,
            "--nudge",
            "have a look at phase one",
        ])
        .success();

    let frame = page.next_frame();
    assert_eq!(frame["events"][0]["type"], "nudge");
    assert_eq!(
        frame["events"][0]["data"]["text"],
        "have a look at phase one"
    );
    assert_eq!(
        l.server.count_events("nudge"),
        0,
        "a banner is not a fact about the review"
    );
}

// --- the agent's write verbs ------------------------------------------------

#[test]
fn reply_reads_stdin_when_asked() {
    let l = start();
    let out = l.repo.run_with_stdin(
        &["reply", "--session", &l.session, "--stdin"],
        "from a pipe",
    );
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(
        l.server.last_event_of_type("chat.sent")["data"]["text"],
        "from a pipe"
    );
}

#[test]
fn a_page_level_reply_needs_no_artifact_when_there_is_only_one() {
    let l = start();
    l.repo
        .run(&["reply", "--session", &l.session, "reading it now"])
        .success();
    assert_eq!(
        l.server.last_event_of_type("chat.sent")["actor"],
        "agent",
        "spec 5: --artifact may be omitted when the server has exactly one"
    );
}

#[test]
fn resolve_requires_exactly_one_verdict() {
    let l = start();
    let cookie = l.cookie();
    let opened = l.server.post_cmd(
        &cookie,
        "plan:demo",
        serde_json::json!({
            "cmd": "thread.open", "client_id": "cid-1", "ref": "task:t-a",
            "text": "x", "opened_revision": 1,
        }),
    );
    let thread = opened["assigned"].as_str().unwrap();

    assert_eq!(
        l.repo
            .run(&["resolve", thread, "--session", &l.session])
            .code,
        2,
        "clap refuses a resolve with no verdict"
    );
    assert_eq!(
        l.repo
            .run(&[
                "resolve",
                thread,
                "--session",
                &l.session,
                "--changed",
                "--declined"
            ])
            .code,
        2,
        "and one with both"
    );
}

#[test]
fn a_reply_with_a_superseded_session_exits_6_and_writes_nothing() {
    let l = start();
    l.repo
        .run(&["await", "--timeout", "1s", "--agent", "codex", "--takeover"])
        .success();
    let before = l.server.last_seq();

    let out = l
        .repo
        .run(&["reply", "--session", &l.session, "still here?"]);
    assert_eq!(out.code, 6);
    assert_eq!(l.server.last_seq(), before);
}

#[test]
fn the_agents_own_reply_does_not_come_back_to_it() {
    let l = start();
    l.repo
        .run(&["reply", "--session", &l.session, "a note"])
        .success();

    let out = l
        .repo
        .run(&["await", "--timeout", "1s", "--session", &l.session]);
    let r: serde_json::Value = serde_json::from_str(out.success().stdout.trim()).unwrap();
    assert_eq!(r["status"], "timeout");
    assert!(
        r["events"].as_array().unwrap().is_empty(),
        "spec 7 has the agent scanning frames for chat it must answer, not its own output"
    );
}

// --- the nudge timers -------------------------------------------------------

fn nudged(idle: Option<Duration>, away: Option<Duration>) -> InProcess {
    let server = InProcess::start_with_nudges(Nudges { idle, away });
    server.seed_artifact();
    server
}

#[test]
fn idle_fires_once_per_quiet_period_and_re_arms_after_activity() {
    let server = nudged(Some(Duration::from_secs(900)), None);
    let _page = server.connect_page();
    support::wait_for(|| server.page_count() == 1, "the page should attach");
    server.mark_reviewer_activity_at(0);

    presence::tick(&server.shared, 901_000);
    assert_eq!(server.count_events("reviewer.idle"), 1);
    presence::tick(&server.shared, 1_200_000);
    assert_eq!(
        server.count_events("reviewer.idle"),
        1,
        "once per quiet period, not once per tick"
    );

    server.mark_reviewer_activity_at(1_300_000);
    presence::tick(&server.shared, 2_300_000);
    assert_eq!(
        server.count_events("reviewer.idle"),
        2,
        "activity re-arms it"
    );
}

#[test]
fn a_reviewer_who_reads_for_twenty_minutes_is_not_idle() {
    // Spec 6.2: activity is measured from a page ping on scroll, keys, pointer
    // and visibility, "not from the last comment".
    let server = nudged(Some(Duration::from_secs(900)), None);
    let _page = server.connect_page();
    support::wait_for(|| server.page_count() == 1, "the page should attach");

    for minute in 0..20i64 {
        server.mark_reviewer_activity_at(minute * 60_000);
        presence::tick(&server.shared, minute * 60_000 + 30_000);
    }
    assert_eq!(server.count_events("reviewer.idle"), 0);
}

#[test]
fn an_agent_polling_is_not_the_reviewers_activity() {
    // An `await` is an HTTP request every 90 seconds. If one clock served both
    // timers, `reviewer.idle` could never fire with an agent attached.
    let repo = Repo::new();
    let server = InProcess::start_in_with(
        &repo,
        Nudges {
            idle: Some(Duration::from_secs(900)),
            away: None,
        },
    );
    server.seed_artifact();
    let _page = server.connect_page();
    support::wait_for(|| server.page_count() == 1, "the page should attach");
    server.mark_reviewer_activity_at(0);

    for _ in 0..5 {
        repo.run(&["await", "--timeout", "1s"]).success();
    }
    presence::tick(&server.shared, 901_000);
    assert_eq!(
        server.count_events("reviewer.idle"),
        1,
        "the agent's polling did not reset the reviewer's clock"
    );
}

#[test]
fn away_fires_once_and_back_fires_on_return() {
    let server = nudged(None, Some(Duration::from_secs(300)));
    let page = server.connect_page();
    support::wait_for(|| server.page_count() == 1, "the page should attach");
    drop(page);
    support::wait_for(
        || {
            server.broadcast_test_frame(1);
            server.page_count() == 0
        },
        "the page should be gone",
    );

    presence::tick(&server.shared, 1_000);
    presence::tick(&server.shared, 400_000);
    assert_eq!(server.count_events("reviewer.away"), 1);
    presence::tick(&server.shared, 800_000);
    assert_eq!(
        server.count_events("reviewer.away"),
        1,
        "once, not every tick"
    );

    let _returned = server.connect_page();
    support::wait_for(|| server.page_count() == 1, "the page should be back");
    presence::tick(&server.shared, 900_000);
    assert_eq!(server.count_events("reviewer.back"), 1);
}

#[test]
fn away_does_not_fire_before_a_reviewer_has_ever_been_here() {
    let server = nudged(None, Some(Duration::from_secs(300)));
    presence::tick(&server.shared, 1_000);
    presence::tick(&server.shared, 900_000);
    assert_eq!(
        server.count_events("reviewer.away"),
        0,
        "a server nobody opened has no reviewer to be away"
    );
}

#[test]
fn away_does_not_fire_once_the_review_is_submitted() {
    let repo = Repo::new();
    let server = InProcess::start_in_with(
        &repo,
        Nudges {
            idle: None,
            away: Some(Duration::from_secs(300)),
        },
    );
    let plan = plan_in(&repo);
    repo.run(&["plan", "push", &plan, "--json", "--no-open"])
        .success();

    let cookie = server.session_cookie("plan:demo");
    let page = server.connect_page();
    support::wait_for(|| server.page_count() == 1, "the page should attach");
    server.post_cmd(
        &cookie,
        "plan:demo",
        serde_json::json!({
            "cmd": "review.submit", "client_id": "cid-1",
            "verdict": "approve", "base_revision": 1,
        }),
    );
    drop(page);
    support::wait_for(
        || {
            server.broadcast_test_frame(1);
            server.page_count() == 0
        },
        "the page should be gone",
    );

    presence::tick(&server.shared, 1_000);
    presence::tick(&server.shared, 900_000);
    assert_eq!(
        server.count_events("reviewer.away"),
        0,
        "a finished review is not an abandoned one"
    );
}

#[test]
fn off_disables_a_timer_entirely() {
    // Spec 16: both nudge timers can be set to `off`.
    let server = nudged(None, None);
    let _page = server.connect_page();
    support::wait_for(|| server.page_count() == 1, "the page should attach");
    server.mark_reviewer_activity_at(0);

    presence::tick(&server.shared, 100_000_000);
    assert_eq!(server.count_events("reviewer.idle"), 0);
    assert_eq!(server.count_events("reviewer.away"), 0);
}

#[test]
fn an_idle_nudge_reaches_the_agent_as_an_active_frame() {
    // Spec 6.2 makes it active, so it goes through the log and the cursor
    // rather than being announced like presence.
    let repo = Repo::new();
    let server = InProcess::start_in_with(
        &repo,
        Nudges {
            idle: Some(Duration::from_secs(900)),
            away: None,
        },
    );
    server.seed_artifact();
    let _page = server.connect_page();
    support::wait_for(|| server.page_count() == 1, "the page should attach");
    server.mark_reviewer_activity_at(0);
    presence::tick(&server.shared, 901_000);

    let out = repo.run(&["await", "--timeout", "2s"]);
    let r: serde_json::Value = serde_json::from_str(out.success().stdout.trim()).unwrap();
    assert_eq!(r["status"], "idle");
    assert_eq!(
        r["events"].as_array().unwrap().last().unwrap()["artifact"],
        "plan:demo",
        "named so an agent filtering with --artifact still hears it"
    );
}
