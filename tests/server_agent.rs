//! `await`, `events`, and `ack`, driven as the real binary.
//!
//! These run the `artefacto` process against a server this test controls, so
//! the log can be written to mid-wait. That matters: every failure mode here
//! is invisible until an agent is actually driving a review, and a test that
//! called the library directly would prove nothing about exit codes, stdout,
//! or a subprocess whose pid dies between polls.
//!
//! Spec 5's rule that shapes most of this file: `await` "always exits 0 when
//! the server answered", because agents treat a non-zero exit as a failed tool
//! call rather than as "poll again".

mod support;

use std::time::Duration;
use support::{Follower, InProcess, Repo};

/// A repository with a server on it that this test writes events into.
fn attached() -> (Repo, InProcess) {
    let repo = Repo::new();
    let server = InProcess::start_in(&repo);
    server.seed_artifact();
    (repo, server)
}

fn json_of(out: &support::Out) -> serde_json::Value {
    serde_json::from_str(&out.stdout)
        .unwrap_or_else(|e| panic!("stdout was not one JSON object: {e}\n{}", out.stdout))
}

#[test]
fn await_without_a_server_exits_4() {
    let repo = Repo::new();
    let out = repo.run(&["await", "--timeout", "1s"]);
    assert_eq!(out.code, 4, "agents branch on this code");
}

#[test]
fn the_first_await_returns_a_session_and_the_second_reuses_it() {
    // The poll loop the lease exists to support. Without `--session` on
    // `await`, the second poll of every loop exits 6.
    let (repo, _server) = attached();
    let first = json_of(
        repo.run(&["await", "--timeout", "1s", "--agent", "claude"])
            .success(),
    );
    let session = first["session"]
        .as_str()
        .expect("a session token")
        .to_string();
    assert_eq!(first["status"], "timeout");

    let second = json_of(
        repo.run(&[
            "await",
            "--timeout",
            "1s",
            "--agent",
            "claude",
            "--session",
            &session,
        ])
        .success(),
    );
    assert_eq!(second["session"], session, "the same lease, not a new one");
}

#[test]
fn a_token_from_await_still_works_after_that_process_has_exited() {
    // `await` is a short-lived subprocess. If liveness were a pid check, its
    // own token would be dead by the time the next command ran.
    let (repo, server) = attached();
    let session = json_of(repo.run(&["await", "--timeout", "1s"]).success())["session"]
        .as_str()
        .unwrap()
        .to_string();
    let chat = server.log_reviewer("chat.sent");

    let out = repo.run(&["ack", "--seq", &chat.to_string(), "--session", &session]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(
        server.cursor_of("agent"),
        chat,
        "the token minted by a process that has since exited still authorises a write"
    );
}

#[test]
fn await_returns_timeout_with_exit_zero_when_nothing_happens() {
    let (repo, _server) = attached();
    let out = repo.run(&["await", "--timeout", "1s"]);
    assert_eq!(
        out.code, 0,
        "agents treat a non-zero exit as a failed tool call, not as 'poll again'"
    );
    assert_eq!(json_of(&out)["status"], "timeout");
}

#[test]
fn await_returns_on_a_chat_event_with_the_passive_events_before_it() {
    let (repo, server) = attached();
    server.log_reviewer("thread.opened");
    server.log_reviewer("chat.sent");

    let r = json_of(repo.run(&["await", "--timeout", "5s"]).success());
    assert_eq!(r["status"], "chat");
    assert_eq!(r["events"].as_array().unwrap().len(), 2);
    assert_eq!(r["events"][0]["type"], "thread.opened");
}

#[test]
fn await_returns_at_the_earliest_active_event() {
    let (repo, server) = attached();
    server.log_reviewer("chat.sent");
    server.log_reviewer("review.submitted");

    let r = json_of(repo.run(&["await", "--timeout", "5s"]).success());
    assert_eq!(
        r["status"], "chat",
        "events are handled in the order they happened"
    );
}

#[test]
fn await_returns_back_when_the_reviewer_reconnects() {
    let (repo, server) = attached();
    server.log_reviewer("reviewer.back");

    let r = json_of(repo.run(&["await", "--timeout", "5s"]).success());
    assert_eq!(
        r["status"], "back",
        "spec 6.2 lists reviewer.back as active, so it needs a status of its own"
    );
}

#[test]
fn await_wakes_only_for_the_artifact_it_was_given_and_loses_nothing() {
    let (repo, server) = attached();
    server.log_reviewer("thread.opened");
    server.log_reviewer("chat.sent"); // on plan:demo

    let r = json_of(
        repo.run(&["await", "--timeout", "1s", "--artifact", "plan:other"])
            .success(),
    );
    assert_eq!(
        r["status"], "timeout",
        "an event for another artifact must not wake this wait"
    );
    let kinds: Vec<&str> = r["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["type"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        ["thread.opened"],
        "the timeout's tail stops before the other artifact's chat: an earlier \
         version let it ride along, and acknowledging that result skipped a \
         chat nobody was ever woken for"
    );

    // Acknowledge the timeout, and the chat is still there for a call that
    // wakes for it.
    let r = json_of(
        repo.run(&[
            "await",
            "--timeout",
            "1s",
            "--session",
            r["session"].as_str().unwrap(),
            "--ack",
            &r["seq"].to_string(),
        ])
        .success(),
    );
    assert_eq!(r["status"], "chat");
}

#[test]
fn calling_await_again_with_ack_acknowledges_the_previous_frame() {
    let (repo, server) = attached();
    server.log_reviewer("chat.sent");
    let first = json_of(repo.run(&["await", "--timeout", "5s"]).success());
    server.log_reviewer("chat.sent");
    let second = json_of(
        repo.run(&[
            "await",
            "--timeout",
            "5s",
            "--session",
            first["session"].as_str().unwrap(),
            "--ack",
            &first["seq"].to_string(),
        ])
        .success(),
    );

    assert_eq!(
        second["events"].as_array().unwrap().len(),
        1,
        "the first frame was acknowledged, so only the new event is left"
    );
    assert!(second["seq"].as_u64().unwrap() > first["seq"].as_u64().unwrap());
}

#[test]
fn calling_await_again_without_ack_hands_back_the_same_frame() {
    // Spec 16: at-least-once, and "handlers must be safe to run twice". An
    // agent that received the first frame and then restarted before acting
    // on it calls again with nothing to acknowledge, and sees it again.
    let (repo, server) = attached();
    server.log_reviewer("chat.sent");
    let first = json_of(repo.run(&["await", "--timeout", "5s"]).success());
    let second = json_of(
        repo.run(&[
            "await",
            "--timeout",
            "1s",
            "--session",
            first["session"].as_str().unwrap(),
        ])
        .success(),
    );
    assert_eq!(second["status"], "chat");
    assert_eq!(second["seq"], first["seq"]);
    assert_eq!(server.cursor_of("agent"), 0, "nothing moved the cursor");
}

#[test]
fn a_replay_followed_by_a_plain_call_does_not_fail() {
    // Spec 6.5 names `events --since SEQ` as the restart path. An earlier
    // version left a replayed frame as the session's outstanding offer and
    // then refused to acknowledge it on the next call — "does not move back"
    // — which was exit 2, a failed tool call, for doing the documented thing.
    let (repo, server) = attached();
    let early = server.log_reviewer("chat.sent");
    let late = server.log_reviewer("chat.sent");
    let first = json_of(repo.run(&["await", "--timeout", "5s"]).success());
    let session = first["session"].as_str().unwrap().to_string();
    assert_eq!(first["seq"], early);
    repo.run(&[
        "await",
        "--timeout",
        "5s",
        "--session",
        &session,
        "--ack",
        &early.to_string(),
    ])
    .success();
    repo.run(&[
        "await",
        "--timeout",
        "1s",
        "--session",
        &session,
        "--ack",
        &late.to_string(),
    ])
    .success();
    assert_eq!(server.cursor_of("agent"), late);

    let out = repo.run(&["events", "--since", "0", "--session", &session]);
    out.success();
    let lines: Vec<&str> = out.stdout.lines().filter(|l| !l.is_empty()).collect();
    let replayed: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(replayed["seq"], early);

    let after = repo.run(&[
        "await",
        "--timeout",
        "1s",
        "--session",
        &session,
        "--ack",
        &early.to_string(),
    ]);
    assert_eq!(after.code, 0, "{}", after.stderr);
    assert_eq!(json_of(&after)["status"], "timeout");
    assert_eq!(
        server.cursor_of("agent"),
        late,
        "and the cursor did not move back"
    );
}

#[test]
fn await_blocks_until_something_happens() {
    // Proves this is a real long poll. A version that returned immediately
    // would pass every other test in this file.
    let (repo, server) = attached();
    let started = std::time::Instant::now();
    let waiting = std::thread::spawn(move || repo.run(&["await", "--timeout", "20s"]));
    std::thread::sleep(Duration::from_millis(600));
    server.log_reviewer("chat.sent");

    let out = waiting.join().unwrap();
    let elapsed = started.elapsed();
    assert_eq!(json_of(out.success())["status"], "chat");
    assert!(
        elapsed > Duration::from_millis(500),
        "it returned before the event existed: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(15),
        "it waited out the whole timeout instead of waking: {elapsed:?}"
    );
}

#[test]
fn await_reconnects_when_the_server_comes_back() {
    // Spec 5: `await` "reconnects on its own: if the connection drops or the
    // server restarts mid-wait, it retries against the same cursor until its
    // absolute deadline".
    //
    // A *graceful* stop is a different case and has its own test: it answers
    // `stopped`, which is spec 5's own table. This is the other half — the
    // server going away without saying so, which is all a crashed or killed
    // one looks like from the client's side.
    let repo = Repo::new();
    let first = InProcess::start_in(&repo);
    first.seed_artifact();
    let port = first.port;
    drop(first);

    let waiting = {
        let path = repo.path().to_path_buf();
        let state = repo.state_root();
        std::thread::spawn(move || {
            let out = std::process::Command::new(support::bin())
                .args(["await", "--timeout", "20s"])
                .current_dir(&path)
                .env("XDG_STATE_HOME", &state)
                .output()
                .expect("running artefacto");
            (
                out.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&out.stdout).to_string(),
            )
        })
    };

    // It is now retrying against a port nothing answers on.
    std::thread::sleep(Duration::from_millis(500));
    let server = InProcess::resume_in(&repo);
    assert_eq!(server.port, port, "a restart rebinds the recorded port");
    server.log_reviewer("chat.sent");

    let (code, stdout) = waiting.join().unwrap();
    assert_eq!(code, 0, "a restart is not a failed tool call");
    let r: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    assert_eq!(
        r["status"], "chat",
        "it retried against the same cursor rather than surfacing the restart"
    );
}

#[test]
fn a_second_agent_without_takeover_exits_6_and_names_the_holder() {
    let (repo, _server) = attached();
    repo.run(&["await", "--timeout", "1s", "--agent", "claude"])
        .success();

    let out = repo.run(&["await", "--timeout", "1s", "--agent", "codex"]);
    assert_eq!(out.code, 6);
    assert!(
        out.stderr.contains("claude"),
        "the refusal names the holder: {}",
        out.stderr
    );
    assert!(out.stderr.contains("--takeover"), "{}", out.stderr);
}

#[test]
fn takeover_wins_the_lease_and_supersedes_the_old_token() {
    let (repo, _server) = attached();
    let first = json_of(
        repo.run(&["await", "--timeout", "1s", "--agent", "claude"])
            .success(),
    );
    repo.run(&["await", "--timeout", "1s", "--agent", "codex", "--takeover"])
        .success();

    let out = repo.run(&[
        "await",
        "--timeout",
        "1s",
        "--agent",
        "claude",
        "--session",
        first["session"].as_str().unwrap(),
    ]);
    assert_eq!(out.code, 6, "a superseded token cannot come back");
}

#[test]
fn an_ack_with_a_superseded_token_exits_6() {
    let (repo, server) = attached();
    let stale = json_of(
        repo.run(&["await", "--timeout", "1s", "--agent", "claude"])
            .success(),
    );
    repo.run(&["await", "--timeout", "1s", "--agent", "codex", "--takeover"])
        .success();
    server.log_reviewer("chat.sent");

    let out = repo.run(&[
        "ack",
        "--seq",
        "1",
        "--session",
        stale["session"].as_str().unwrap(),
    ]);
    assert_eq!(out.code, 6);
    assert_eq!(server.cursor_of("claude"), 0, "and it wrote nothing");
}

#[test]
fn events_prints_the_backlog_as_ndjson_and_exits() {
    let (repo, server) = attached();
    server.log_reviewer("chat.sent");
    server.log_reviewer("thread.opened");
    server.log_reviewer("chat.sent");

    let out = repo.run(&["events", "--since", "0"]);
    out.success();
    let lines: Vec<&str> = out.stdout.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines.len(),
        3,
        "the session line, then one frame per active event: {:?}",
        lines
    );
    let head: serde_json::Value = serde_json::from_str(lines[0]).expect("json");
    assert_eq!(head["format"], "artefacto.session/1");
    assert!(
        head["session"].as_str().is_some_and(|t| !t.is_empty()),
        "spec 5: events returns the token in its result; without it a monitor-mode \
         agent has nothing to reply with"
    );
    for line in &lines[1..] {
        let v: serde_json::Value = serde_json::from_str(line).expect("one JSON frame per line");
        assert_eq!(v["format"], "artefacto.frame/1");
    }
    let last: serde_json::Value = serde_json::from_str(lines[2]).unwrap();
    assert_eq!(
        last["events"].as_array().unwrap().len(),
        2,
        "the second frame carries the passive event that preceded it"
    );
}

#[test]
fn events_follow_streams_frames_and_holds_a_live_lease() {
    let (repo, server) = attached();
    let mut follow = Follower::spawn(&repo, &["events", "--follow", "--agent", "claude"]);

    support::wait_for(
        || server.lease_mode().as_deref() == Some("live"),
        "--follow should take a live lease",
    );
    let hello = follow.next_frame();
    assert_eq!(
        hello["format"], "artefacto.session/1",
        "the token comes first"
    );
    let token = hello["session"]
        .as_str()
        .expect("a monitor-mode agent needs a token to reply with")
        .to_string();

    server.log_reviewer("chat.sent");
    let frame = follow.next_frame();
    assert_eq!(frame["format"], "artefacto.frame/1");
    assert_eq!(
        frame["events"].as_array().unwrap().last().unwrap()["type"],
        "chat.sent"
    );
    // And that token works for a write. Spec 7 rule 3 has the monitor agent
    // answer chat; an earlier version gave it no token to do so with.
    repo.run(&["reply", "--session", &token, "reading it now"])
        .success();

    server.log_reviewer("thread.opened");
    server.log_reviewer("review.submitted");
    let second = follow.next_frame();
    assert_eq!(
        second["events"].as_array().unwrap().len(),
        2,
        "the next frame starts after the one already printed — the follow's \
         own read position, not the cursor"
    );
    assert_eq!(
        server.cursor_of("claude"),
        0,
        "a follow acknowledges nothing on its own; the agent runs `ack` after acting"
    );

    follow.kill();
    support::wait_for(
        || server.lease_mode().is_none(),
        "spec 4.2: a --follow disconnect releases the lease immediately",
    );
}

#[test]
fn events_follow_exits_zero_when_the_server_stops() {
    let (repo, server) = attached();
    let mut follow = Follower::spawn(&repo, &["events", "--follow", "--agent", "claude"]);
    support::wait_for(
        || server.lease_mode().is_some(),
        "--follow should have attached",
    );

    server.shared.request_stop();
    assert_eq!(
        follow.wait_code(),
        0,
        "a shutdown is not a failed tool call"
    );
}

#[test]
fn await_returns_stopped_when_the_server_is_shutting_down() {
    let (repo, server) = attached();
    let waiting = {
        let path = repo.path().to_path_buf();
        let state = repo.state_root();
        std::thread::spawn(move || {
            let out = std::process::Command::new(support::bin())
                .args(["await", "--timeout", "20s"])
                .current_dir(&path)
                .env("XDG_STATE_HOME", &state)
                .output()
                .expect("running artefacto");
            String::from_utf8_lossy(&out.stdout).to_string()
        })
    };
    std::thread::sleep(Duration::from_millis(400));
    server.shared.request_stop();

    let stdout = waiting.join().unwrap();
    let r: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    assert_eq!(r["status"], "stopped");
}
