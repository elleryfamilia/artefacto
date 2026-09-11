//! The loop exactly as the skill prescribes it, driven against a real daemon
//! with the real commands, in both modes.
//!
//! `skills/artefacto-plan/SKILL.md` is the document under test. Each step
//! here is one the skill tells an agent to take, in the order it says, with
//! the flags it names; the reviewer's half is the page's command protocol
//! through a cookie obtained the way a browser obtains one, from `open`.
//! Nothing reaches into the server: if the skill's instructions do not work
//! from the outside, they do not work.

mod support;

use std::path::Path;
use support::{get, raw, Follower, Repo};

fn plan_in(repo: &Repo) -> String {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plan/minimal.json");
    let target = repo.path().join("plan.json");
    std::fs::copy(&source, &target).expect("copy the fixture");
    target.display().to_string()
}

/// A browser's cookie, obtained the way the skill's user obtains one: a link
/// from `open`, followed once.
fn reviewer_cookie(repo: &Repo) -> String {
    let out = repo.json(&["open", "--json"]);
    let url = out["url"].as_str().expect("a link");
    let port = repo.port();
    let path = url
        .strip_prefix(&format!("http://127.0.0.1:{port}"))
        .expect("a link to this server");
    get(port, path)
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("set-cookie:"))
        .and_then(|l| l.split_once(": ").map(|(_, v)| v))
        .and_then(|v| v.split(';').next())
        .expect("the link sets the cookie")
        .trim()
        .to_string()
}

/// One page command, as the page posts it.
fn page(repo: &Repo, cookie: &str, body: serde_json::Value) -> serde_json::Value {
    let port = repo.port();
    let payload = body.to_string();
    let request = format!(
        "POST /a/plan:demo/cmd HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nCookie: {cookie}\r\n\
         Origin: http://127.0.0.1:{port}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    let response = raw(port, &request);
    let body = response.split("\r\n\r\n").nth(1).unwrap_or("");
    serde_json::from_str(body).unwrap_or_else(|e| panic!("not json: {e}\n{response}"))
}

fn last_event(frame: &serde_json::Value) -> &serde_json::Value {
    frame["events"]
        .as_array()
        .and_then(|e| e.last())
        .expect("a frame has at least one event")
}

/// Run the body, then stop the daemon whether or not it passed.
fn with_daemon(repo: &Repo, body: impl FnOnce()) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));
    repo.stop();
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

/// Revision 2 of the fixture: task A kept, a task B added, so every thread
/// stays anchored.
fn revise(plan: &str) {
    let mut doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(plan).unwrap()).unwrap();
    doc["phases"][0]["tasks"] = serde_json::json!([
        { "id": "t-a", "title": "Task A" },
        { "id": "t-b", "title": "Task B, added for c-1" }
    ]);
    std::fs::write(plan, doc.to_string()).unwrap();
}

#[test]
fn monitor_mode_runs_the_loop_the_skill_prescribes() {
    let repo = Repo::new();
    let plan = plan_in(&repo);

    // Section 1: push, with no server running. Keep session and revision.
    let pushed = repo.json(&["plan", "push", &plan, "--json", "--no-open"]);
    with_daemon(&repo, || {
        let session = pushed["session"].as_str().expect("a token").to_string();
        assert_eq!(pushed["revision"], 1);
        assert!(pushed["revision_seq"].is_u64(), "named, never acknowledged");

        // Section 2 / Claude Code: arm the line status prints, as printed.
        let status = repo.json(&["status", "--json"]);
        let argv: Vec<String> = status["follow"]["argv"]
            .as_array()
            .expect("argv")
            .iter()
            .map(|w| w.as_str().unwrap().to_string())
            .collect();
        assert_eq!(argv[0], "artefacto");
        let args: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
        let mut follow = Follower::spawn(&repo, &args);

        let hello = follow.next_frame();
        assert_eq!(hello["format"], "artefacto.session/1");
        assert_eq!(
            hello["session"], session,
            "the same name rejoins the push's lease, so the token is the push's"
        );
        support::wait_for(
            || repo.json(&["status", "--json"])["lease"]["mode"] == "live",
            "the follow holds a live lease",
        );

        // The reviewer comments and asks inside the thread.
        let cookie = reviewer_cookie(&repo);
        let opened = page(
            &repo,
            &cookie,
            serde_json::json!({
                "cmd": "thread.open", "client_id": "cid-1", "ref": "task:t-a",
                "text": "why a trait here?", "blocking": true, "opened_revision": 1,
            }),
        );
        let thread = opened["assigned"].as_str().expect("an id").to_string();
        page(
            &repo,
            &cookie,
            serde_json::json!({
                "cmd": "chat.send", "client_id": "cid-2", "thread": thread,
                "text": "and is it worth it?", "opened_revision": 1,
            }),
        );

        // Section 3: a frame whose last event is chat.sent.
        let frame = follow.next_frame();
        assert_eq!(frame["format"], "artefacto.frame/1");
        assert_eq!(last_event(&frame)["type"], "chat.sent");
        assert_eq!(last_event(&frame)["data"]["thread"], thread);
        assert_eq!(
            frame["events"][0]["type"], "thread.opened",
            "the passive event before it rides along"
        );
        let chat_seq = frame["seq"].as_u64().unwrap();

        // Rule 3, "check first": status says whose message is last.
        let before = repo.json(&["status", "--json"]);
        assert_eq!(
            before["artifacts"][0]["threads"][0]["last_actor"],
            "reviewer"
        );
        repo.run(&[
            "reply",
            "--session",
            &session,
            "--thread",
            &thread,
            "so Redis can slot in",
        ])
        .success();
        let after = repo.json(&["status", "--json"]);
        assert_eq!(
            after["artifacts"][0]["threads"][0]["last_actor"], "agent",
            "a redelivered frame would be skipped on this"
        );

        // Rule 8: acknowledge after acting; and twice is harmless.
        repo.run(&["ack", "--seq", &chat_seq.to_string(), "--session", &session])
            .success();
        repo.run(&["ack", "--seq", &chat_seq.to_string(), "--session", &session])
            .success();
        assert_eq!(
            repo.json(&["status", "--json"])["cursors"]["agent"],
            chat_seq
        );
        assert!(
            follow.no_frame_within(std::time::Duration::from_millis(300)),
            "the agent's own reply is not delivered back to it"
        );

        // The reviewer sends the review.
        page(
            &repo,
            &cookie,
            serde_json::json!({
                "cmd": "review.submit", "client_id": "cid-3",
                "verdict": "request_changes", "base_revision": 1,
            }),
        );
        let frame = follow.next_frame();
        let submitted = last_event(&frame);
        assert_eq!(submitted["type"], "review.submitted");
        assert_eq!(submitted["data"]["verdict"], "request_changes");
        let feedback = &submitted["data"]["feedback"];
        assert_eq!(feedback["format"], "artefacto.feedback/1");
        assert_eq!(feedback["comments"][0]["id"], thread);
        assert_eq!(feedback["comments"][0]["status"], "open");
        let on_disk: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(submitted["data"]["path"].as_str().unwrap()).unwrap(),
        )
        .unwrap();
        assert_eq!(on_disk, *feedback, "the file is the same document");
        let submit_seq = frame["seq"].as_u64().unwrap();

        // Address it: the plan changes, and the resolutions land with it.
        revise(&plan);
        let resolutions = repo.path().join("resolutions.json");
        std::fs::write(
            &resolutions,
            serde_json::json!([
                { "thread": thread, "status": "changed", "note": "added t-b for it" }
            ])
            .to_string(),
        )
        .unwrap();
        let pushed = repo.json(&[
            "plan",
            "push",
            &plan,
            "--json",
            "--no-open",
            "--session",
            &session,
            "--base-revision",
            "1",
            "--resolutions",
            resolutions.to_str().unwrap(),
        ]);
        assert_eq!(pushed["revision"], 2);
        assert_eq!(pushed["open_threads"], 0, "resolved in the same commit");
        repo.run(&[
            "ack",
            "--seq",
            &submit_seq.to_string(),
            "--session",
            &session,
        ])
        .success();

        // Section 4, exit 7: the revision the agent last saw is behind.
        let stale = repo.run(&[
            "plan",
            "push",
            &plan,
            "--json",
            "--no-open",
            "--session",
            &session,
            "--base-revision",
            "1",
        ]);
        assert_eq!(stale.code, 7, "{}", stale.stderr);
        assert_eq!(
            repo.json(&["status", "--json"])["artifacts"][0]["revision"],
            2,
            "status says where the server is"
        );

        // Claude Code, exit 6: a killed monitor's token is dead. Re-armed
        // with --session it exits 6; re-armed without, under the same name,
        // it rejoins with nothing lost.
        follow.kill();
        support::wait_for(
            || repo.json(&["status", "--json"])["lease"].is_null(),
            "a dead follow releases the lease",
        );
        let mut dead = Follower::spawn(&repo, &["events", "--follow", "--session", &session]);
        assert_eq!(dead.wait_code(), 6, "a dead token is refused, not adopted");

        let mut follow = Follower::spawn(&repo, &args);
        let hello = follow.next_frame();
        assert_eq!(hello["format"], "artefacto.session/1");
        let fresh = hello["session"].as_str().unwrap().to_string();
        assert_ne!(fresh, session, "a new token");
        assert_eq!(
            hello["seq"], submit_seq,
            "the cursor is keyed by name: it resumes after the last acknowledged frame"
        );
        assert!(
            follow.no_frame_within(std::time::Duration::from_millis(300)),
            "everything was acknowledged, so nothing is replayed"
        );
        repo.run(&["reply", "--session", &fresh, "on it"]).success();
        assert_eq!(
            repo.json(&["status", "--json"])["artifacts"][0]["chat_last_actor"],
            "agent"
        );

        // Exit 0: the server stopped.
        repo.run(&["stop"]).success();
        assert_eq!(follow.wait_code(), 0, "a stop is not a failure");
    });
}

#[test]
fn poll_mode_runs_the_loop_the_skill_prescribes() {
    let repo = Repo::new();
    let plan = plan_in(&repo);
    let pushed = repo.json(&["plan", "push", &plan, "--json", "--no-open"]);
    with_daemon(&repo, || {
        let session = pushed["session"].as_str().unwrap().to_string();

        // "If you have no token, await under the same name rejoins the lease."
        let rejoined = repo.json(&["await", "--timeout", "1s"]);
        assert_eq!(rejoined["status"], "timeout");
        assert_eq!(rejoined["session"], session);

        let cookie = reviewer_cookie(&repo);
        page(
            &repo,
            &cookie,
            serde_json::json!({
                "cmd": "thread.open", "client_id": "cid-1", "ref": "task:t-a",
                "text": "a note", "blocking": false, "opened_revision": 1,
            }),
        );
        page(
            &repo,
            &cookie,
            serde_json::json!({
                "cmd": "chat.send", "client_id": "cid-2", "text": "hello?", "opened_revision": 1,
            }),
        );

        // Act on status, then call again with --ack.
        let first = repo.json(&["await", "--session", &session, "--timeout", "5s"]);
        assert_eq!(first["status"], "chat");
        assert_eq!(
            last_event(&first)["data"]["thread"],
            serde_json::Value::Null
        );
        assert_eq!(first["events"][0]["type"], "thread.opened");
        let chat_seq = first["seq"].as_u64().unwrap();

        // Rule 3 for page-level chat.
        assert_eq!(
            repo.json(&["status", "--json"])["artifacts"][0]["chat_last_actor"],
            "reviewer"
        );
        repo.run(&["reply", "--session", &session, "hello back"])
            .success();
        assert_eq!(
            repo.json(&["status", "--json"])["artifacts"][0]["chat_last_actor"],
            "agent"
        );

        // A call without --ack is handed the same frame again.
        let again = repo.json(&["await", "--session", &session, "--timeout", "1s"]);
        assert_eq!(again["status"], "chat");
        assert_eq!(again["seq"], chat_seq);

        // With --ack, the cursor moves and the wait finds nothing.
        let quiet = repo.json(&[
            "await",
            "--session",
            &session,
            "--timeout",
            "1s",
            "--ack",
            &chat_seq.to_string(),
        ]);
        assert_eq!(quiet["status"], "timeout");
        assert_eq!(quiet["seq"], chat_seq);
        assert_eq!(
            repo.json(&["status", "--json"])["cursors"]["agent"],
            chat_seq
        );

        // The review arrives; the agent addresses it and acknowledges by
        // passing the seq to the next call.
        page(
            &repo,
            &cookie,
            serde_json::json!({
                "cmd": "review.submit", "client_id": "cid-3",
                "verdict": "approve", "base_revision": 1,
            }),
        );
        let submitted = repo.json(&[
            "await",
            "--session",
            &session,
            "--timeout",
            "5s",
            "--ack",
            &chat_seq.to_string(),
        ]);
        assert_eq!(submitted["status"], "submitted");
        assert_eq!(
            last_event(&submitted)["data"]["feedback"]["verdict"],
            "approve"
        );
        let submit_seq = submitted["seq"].as_u64().unwrap();
        let done = repo.json(&[
            "await",
            "--session",
            &session,
            "--timeout",
            "1s",
            "--ack",
            &submit_seq.to_string(),
        ]);
        assert_eq!(done["status"], "timeout");
        assert_eq!(
            repo.json(&["status", "--json"])["artifacts"][0]["submitted"],
            true
        );
    });
}
