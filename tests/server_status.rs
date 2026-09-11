//! `status --json`: everything an agent needs to rejoin a review, and never
//! the token.
//!
//! Spec 5: "prints port, artifacts, revisions, open and unanchored threads,
//! the last event sequence, each lease's `acked_seq`, the lease holder and
//! its age, reviewer presence, and the exact `events --follow` command line
//! for the skill to arm." Every field here is one the skill reads, so each is
//! asserted by the value a real review produces, not by its presence.

mod support;

use std::path::Path;
use support::{InProcess, Repo};

fn plan_in(repo: &Repo) -> String {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plan/minimal.json");
    let target = repo.path().join("plan.json");
    std::fs::copy(&source, &target).expect("copy the fixture");
    target.display().to_string()
}

struct Review {
    repo: Repo,
    server: InProcess,
    plan: String,
    session: String,
}

/// A pushed plan under the agent name `claude`, so the follow line has a
/// name to carry that is not the default.
fn pushed() -> Review {
    let repo = Repo::new();
    let server = InProcess::start_in(&repo);
    let plan = plan_in(&repo);
    let out = repo.json(&[
        "plan",
        "push",
        &plan,
        "--json",
        "--no-open",
        "--agent",
        "claude",
    ]);
    let session = out["session"].as_str().expect("a session").to_string();
    Review {
        repo,
        server,
        plan,
        session,
    }
}

impl Review {
    fn cookie(&self) -> String {
        self.server.session_cookie("plan:demo")
    }

    fn status(&self) -> serde_json::Value {
        self.repo.json(&["status", "--json"])
    }

    fn open_thread(&self, cookie: &str, client: &str, target: &str, blocking: bool) -> String {
        let opened = self.server.post_cmd(
            cookie,
            "plan:demo",
            serde_json::json!({
                "cmd": "thread.open", "client_id": client, "ref": target,
                "text": format!("about {target}"), "blocking": blocking, "opened_revision": 1,
            }),
        );
        opened["assigned"].as_str().expect("an id").to_string()
    }
}

#[test]
fn status_json_lists_each_artifact_with_its_review_state() {
    let r = pushed();
    let cookie = r.cookie();

    // A blocking thread the reviewer then asks about, and the agent answers.
    let first = r.open_thread(&cookie, "cid-1", "task:t-a", true);
    r.server.post_cmd(
        &cookie,
        "plan:demo",
        serde_json::json!({
            "cmd": "chat.send", "client_id": "cid-2", "thread": first,
            "text": "and why?", "opened_revision": 1,
        }),
    );
    r.repo
        .run(&[
            "reply",
            "--session",
            &r.session,
            "--thread",
            &first,
            "because",
        ])
        .success();
    // A second thread the agent declines.
    let second = r.open_thread(&cookie, "cid-3", "phase:p-one", false);
    r.repo
        .run(&[
            "resolve",
            &second,
            "--session",
            &r.session,
            "--declined",
            "--note",
            "out of scope",
        ])
        .success();
    // A blocking thread the agent changed the plan for: resolved, so it no
    // longer blocks anything. Opened on a selection, so `quote` has a value.
    let opened = r.server.post_cmd(
        &cookie,
        "plan:demo",
        serde_json::json!({
            "cmd": "thread.open", "client_id": "cid-6", "ref": "meta:demo",
            "text": "about the plan", "blocking": true, "quote": "Demo plan",
            "opened_revision": 1,
        }),
    );
    let third = opened["assigned"].as_str().expect("an id").to_string();
    r.repo
        .run(&["resolve", &third, "--session", &r.session, "--changed"])
        .success();
    // Page-level chat and a reviewed mark.
    r.server.post_cmd(
        &cookie,
        "plan:demo",
        serde_json::json!({
            "cmd": "chat.send", "client_id": "cid-4", "text": "hello", "opened_revision": 1,
        }),
    );
    r.server.post_cmd(
        &cookie,
        "plan:demo",
        serde_json::json!({
            "cmd": "element.reviewed", "client_id": "cid-5", "ref": "task:t-a", "on": true,
            "opened_revision": 1,
        }),
    );

    let status = r.status();
    assert_eq!(status["ok"], true);
    assert_eq!(status["port"], r.server.port);
    assert_eq!(status["last_seq"], r.server.last_seq());
    assert!(
        status["state_dir"].as_str().is_some(),
        "the state directory is part of the output: {status}"
    );

    let artifacts = status["artifacts"].as_array().expect("an array");
    assert_eq!(artifacts.len(), 1, "{status}");
    let a = &artifacts[0];
    assert_eq!(a["id"], "plan:demo");
    assert_eq!(a["kind"], "plan");
    assert_eq!(a["title"], "Demo plan");
    assert_eq!(a["revision"], 1);
    assert!(
        a["plan_hash"].as_str().unwrap().starts_with("sha256:"),
        "{a}"
    );
    assert!(
        a["source_path"].as_str().unwrap().ends_with("/plan.json"),
        "{a}"
    );
    assert!(
        a["feedback_path"]
            .as_str()
            .unwrap()
            .ends_with("/plan-feedback.json"),
        "where a submit lands, so a restarted agent can find it: {a}"
    );
    assert_eq!(a["submitted"], false);
    assert_eq!(a["open_threads"], 1, "{a}");
    assert_eq!(a["unanchored_threads"], 0, "{a}");
    assert_eq!(
        a["blocking_threads"], 1,
        "a resolved thread blocks nothing, whatever its checkbox said: {a}"
    );
    let chat = a["chat"].as_array().expect("page-level chat as messages");
    assert_eq!(chat.len(), 1, "page-level chat, not thread chat: {a}");
    assert_eq!(chat[0]["actor"], "reviewer");
    assert_eq!(chat[0]["text"], "hello");
    r.repo
        .run(&["reply", "--session", &r.session, "hello yourself"])
        .success();
    let a = &r.status()["artifacts"][0];
    let chat = a["chat"].as_array().unwrap();
    assert_eq!(chat.len(), 2, "{a}");
    assert_eq!(
        chat[1]["actor"], "agent",
        "in order, so the reply is last: {a}"
    );
    assert_eq!(chat[1]["text"], "hello yourself");
    assert_eq!(a["reviewed"], 1, "{a}");

    let threads = a["threads"].as_array().expect("threads");
    assert_eq!(threads.len(), 3, "{a}");
    assert_eq!(threads[0]["id"], first);
    assert_eq!(threads[0]["ref"], "task:t-a");
    assert_eq!(threads[0]["status"], "open");
    assert_eq!(threads[0]["blocking"], true);
    assert_eq!(
        threads[0]["quote"], "",
        "nothing was selected when this thread opened: {a}"
    );
    assert_eq!(threads[2]["quote"], "Demo plan", "the selection: {a}");
    let messages = threads[0]["messages"].as_array().expect("messages");
    assert_eq!(
        messages.len(),
        3,
        "the comment, the question, the answer: {a}"
    );
    assert_eq!(messages[0]["actor"], "reviewer");
    assert_eq!(messages[0]["text"], "about task:t-a");
    assert_eq!(messages[1]["text"], "and why?");
    assert_eq!(
        messages[2]["actor"], "agent",
        "spec 7 rule 3: the thread shows the agent its own reply: {a}"
    );
    assert_eq!(messages[2]["text"], "because");
    assert!(messages[2]["ts"].is_string(), "{a}");
    assert_eq!(threads[1]["id"], second);
    assert_eq!(threads[1]["status"], "declined");
    assert_eq!(threads[1]["blocking"], false);
    let notes = threads[1]["messages"].as_array().unwrap();
    assert_eq!(
        notes.last().unwrap()["actor"],
        "agent",
        "the note is a message"
    );
    assert_eq!(notes.last().unwrap()["text"], "out of scope");
}

#[test]
fn status_json_names_the_follow_command_for_the_lease_holder() {
    let r = pushed();
    let status = r.status();
    assert_eq!(status["lease"]["agent"], "claude", "{status}");
    assert_eq!(status["lease"]["mode"], "waiting", "a push is a short call");
    assert!(status["lease"]["age_secs"].is_u64(), "{status}");
    assert_eq!(status["lease"]["acked_seq"], 0, "{status}");

    let follow = &status["follow"];
    assert_eq!(follow["agent"], "claude", "{status}");
    assert_eq!(
        follow["command"], "artefacto events --follow --agent claude",
        "the exact line the skill arms: the same name rejoins the push's lease"
    );
    assert_eq!(
        follow["argv"],
        serde_json::json!(["artefacto", "events", "--follow", "--agent", "claude"]),
        "and the same line as an argv, for a harness that takes one"
    );
    assert!(
        !follow["command"].as_str().unwrap().contains("--session"),
        "never the token; the follow rejoins by name"
    );
}

#[test]
fn status_json_follow_line_defaults_the_agent_name_when_nobody_holds_the_lease() {
    let repo = Repo::new();
    let server = InProcess::start_in(&repo);
    let status = repo.json(&["status", "--json"]);
    assert!(status["lease"].is_null(), "{status}");
    assert_eq!(status["follow"]["agent"], "agent", "{status}");
    assert_eq!(
        status["follow"]["command"],
        "artefacto events --follow --agent agent"
    );
    assert_eq!(status["artifacts"], serde_json::json!([]));
    drop(server);
}

#[test]
fn status_json_quotes_an_agent_name_a_shell_would_split() {
    let repo = Repo::new();
    let _server = InProcess::start_in(&repo);
    let plan = plan_in(&repo);
    repo.json(&[
        "plan",
        "push",
        &plan,
        "--json",
        "--no-open",
        "--agent",
        "my agent's",
    ]);
    let status = repo.json(&["status", "--json"]);
    assert_eq!(
        status["follow"]["command"], "artefacto events --follow --agent 'my agent'\\''s'",
        "a name with a space or a quote must survive being pasted into a shell"
    );
    assert_eq!(
        status["follow"]["argv"][4], "my agent's",
        "argv is verbatim"
    );
}

#[test]
fn status_json_never_prints_the_session_token() {
    let r = pushed();
    let out = r.repo.run(&["status", "--json"]);
    out.success();
    assert!(
        !out.stdout.contains(&r.session),
        "spec 5: a token cannot be picked up by something that only reads status"
    );
    assert!(!out.stdout.contains(&r.repo.secret()));
}

#[test]
fn status_json_reports_reviewer_presence() {
    let r = pushed();
    let before = r.status();
    assert_eq!(before["reviewer"]["pages"], 0, "{before}");
    assert_eq!(before["reviewer"]["present"], false, "{before}");
    assert_eq!(before["reviewer"]["seen"], false, "{before}");

    let mut page = r.server.connect_page();
    page.hello();
    let during = r.status();
    assert_eq!(during["reviewer"]["pages"], 1, "{during}");
    assert_eq!(during["reviewer"]["present"], true, "{during}");
    assert_eq!(during["reviewer"]["seen"], true, "{during}");
    assert!(
        during["reviewer"]["last_activity_secs"].is_u64(),
        "a page arriving is activity: {during}"
    );
    assert_eq!(during["reviewer"]["idle"], false);
    assert_eq!(during["reviewer"]["away"], false);

    drop(page);
    // Detection needs writes; the heartbeat supplies them in production and
    // the test does here, so it depends on no timer.
    support::wait_for(
        || {
            r.server.broadcast_test_frame(1);
            r.server.page_count() == 0
        },
        "the page to hang up",
    );
    let after = r.status();
    assert_eq!(after["reviewer"]["pages"], 0, "{after}");
    assert_eq!(after["reviewer"]["seen"], true, "a reviewer has been here");
}

#[test]
fn status_json_carries_each_leases_cursor() {
    let r = pushed();
    let cookie = r.cookie();
    r.open_thread(&cookie, "cid-1", "task:t-a", false);
    let seq = r.server.last_seq();
    r.repo
        .run(&["ack", "--seq", &seq.to_string(), "--session", &r.session])
        .success();

    let status = r.status();
    assert_eq!(status["cursors"]["claude"], seq, "{status}");
    assert_eq!(status["lease"]["acked_seq"], seq, "{status}");
}

#[test]
fn status_json_marks_a_thread_whose_element_is_gone_as_unanchored() {
    let r = pushed();
    let cookie = r.cookie();
    r.open_thread(&cookie, "cid-1", "task:t-a", false);

    // Revision 2 drops the task the thread hangs on.
    let mut plan: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&r.plan).unwrap()).unwrap();
    plan["phases"][0]["tasks"] = serde_json::json!([{ "id": "t-b", "title": "Task B" }]);
    std::fs::write(&r.plan, plan.to_string()).unwrap();
    r.repo.json(&[
        "plan",
        "push",
        &r.plan,
        "--json",
        "--no-open",
        "--session",
        &r.session,
        "--base-revision",
        "1",
    ]);

    let status = r.status();
    let a = &status["artifacts"][0];
    assert_eq!(a["revision"], 2, "{a}");
    assert_eq!(a["open_threads"], 0, "{a}");
    assert_eq!(a["unanchored_threads"], 1, "{a}");
    assert_eq!(a["threads"][0]["status"], "unanchored", "{a}");
}

#[test]
fn status_json_reports_a_submitted_review() {
    let r = pushed();
    let cookie = r.cookie();
    r.server.post_cmd(
        &cookie,
        "plan:demo",
        serde_json::json!({
            "cmd": "review.submit", "client_id": "cid-1",
            "verdict": "approve", "base_revision": 1,
        }),
    );
    let status = r.status();
    assert_eq!(status["artifacts"][0]["submitted"], true, "{status}");
}

#[test]
fn status_text_says_what_the_json_says() {
    let r = pushed();
    let out = r.repo.run(&["status"]);
    let text = out.success().stdout.clone();
    assert!(text.contains("plan:demo"), "{text}");
    assert!(text.contains("Demo plan"), "{text}");
    assert!(text.contains("revision 1"), "{text}");
    assert!(text.contains("claude"), "the holder: {text}");
    assert!(
        text.contains("artefacto events --follow --agent claude"),
        "the follow line is what a person copies: {text}"
    );
    assert!(!text.contains(&r.session), "never the token");
}
