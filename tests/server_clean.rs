//! `artefacto clean` (spec 6.7, 4.2, 8, 4.4): sent reviews leave the log,
//! open ones stay at their numbers, the secret turns over, and the index
//! keeps every row. Against the real daemon, because clean stops it.

mod support;

use std::path::Path;
use support::{get, raw, status_of, Repo};

fn plan_in(repo: &Repo, fixture: &str, as_name: &str) -> String {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/plan")
        .join(fixture);
    let target = repo.path().join(as_name);
    std::fs::copy(source, &target).expect("copy the fixture");
    target.display().to_string()
}

/// The page cookie, the way a browser gets it: a fresh link from `open`,
/// walked once.
fn cookie_via_open(repo: &Repo, artifact: &str) -> String {
    let out = repo.json(&["open", "--json", "--artifact", artifact]);
    let port = repo.port();
    let path = out["url"]
        .as_str()
        .unwrap()
        .strip_prefix(&format!("http://127.0.0.1:{port}"))
        .expect("this server")
        .to_string();
    let response = get(port, &path);
    assert_eq!(status_of(&response), 302, "{response}");
    response
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("set-cookie:"))
        .and_then(|l| l.split_once(": ").map(|(_, v)| v))
        .and_then(|v| v.split(';').next())
        .expect("a cookie")
        .trim()
        .to_string()
}

fn post_cmd(port: u16, cookie: &str, artifact: &str, body: serde_json::Value) -> String {
    let payload = body.to_string();
    raw(
        port,
        &format!(
            "POST /a/{artifact}/cmd HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nCookie: {cookie}\r\n\
             Origin: http://127.0.0.1:{port}\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{payload}",
            payload.len()
        ),
    )
}

fn get_with(port: u16, path: &str, header: &str) -> String {
    raw(
        port,
        &format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{header}\r\nConnection: close\r\n\r\n"),
    )
}

/// Two pushed plans: the demo's review sent and approved, the other's open
/// with one thread on it.
struct Reviewed {
    repo: Repo,
    port: u16,
    secret: String,
    cookie: String,
    last_seq: u64,
}

fn reviewed() -> Reviewed {
    let repo = Repo::new();
    let demo = plan_in(&repo, "minimal.json", "demo.json");
    let sink = plan_in(&repo, "kitchen-sink.json", "sink.json");
    let first = repo.json(&["plan", "push", &demo, "--json", "--no-open"]);
    let session = first["session"].as_str().unwrap().to_string();
    repo.json(&[
        "plan",
        "push",
        &sink,
        "--json",
        "--no-open",
        "--session",
        &session,
    ]);
    let port = repo.port();
    let cookie = cookie_via_open(&repo, "plan:demo");
    let sent = post_cmd(
        port,
        &cookie,
        "plan:demo",
        serde_json::json!({
            "cmd": "review.submit", "client_id": "c1", "verdict": "approve", "base_revision": 1,
        }),
    );
    assert_eq!(status_of(&sent), 200, "{sent}");
    let opened = post_cmd(
        port,
        &cookie,
        "plan:auth-refactor",
        serde_json::json!({
            "cmd": "thread.open", "client_id": "c2", "ref": "task:t-redis",
            "text": "why redis", "opened_revision": 1,
        }),
    );
    assert_eq!(status_of(&opened), 200, "{opened}");
    let status = repo.json(&["status", "--json"]);
    Reviewed {
        secret: repo.secret(),
        last_seq: status["last_seq"].as_u64().unwrap(),
        repo,
        port,
        cookie,
    }
}

#[test]
fn clean_drops_sent_reviews_keeps_open_ones_and_rotates_the_secret() {
    let r = reviewed();
    let log_before = std::fs::read_to_string(r.repo.state_dir().join("events.ndjson")).unwrap();
    assert!(log_before.contains("\"artifact\":\"plan:demo\""));

    let out = r.repo.json(&["clean", "--json"]);
    assert_eq!(out["ok"], true);
    assert_eq!(out["server_stopped"], true, "{out}");
    assert_eq!(out["removed"], serde_json::json!(["plan:demo"]));
    assert_eq!(out["kept"], serde_json::json!(["plan:auth-refactor"]));
    assert!(
        out["events_removed"].as_u64().unwrap() >= 2,
        "the revision and the submit at least: {out}"
    );
    assert_eq!(out["secret_rotated"], true);

    // The server is gone, the port is kept, the secret is new.
    assert_eq!(r.repo.pid(), 0, "no live pid recorded");
    assert_eq!(r.repo.port(), r.port);
    assert_ne!(r.repo.secret(), r.secret);
    let status = r.repo.run(&["status"]);
    assert_eq!(status.code, 4, "no server after clean");

    // The log holds only the open review, at its old numbers.
    let log_after = std::fs::read_to_string(r.repo.state_dir().join("events.ndjson")).unwrap();
    assert!(
        !log_after.contains("\"artifact\":\"plan:demo\""),
        "{log_after}"
    );
    assert!(log_after.contains("\"artifact\":\"plan:auth-refactor\""));
    assert!(
        log_after.contains("\"type\":\"lease.taken\""),
        "lease records stay"
    );
    let old_seqs: Vec<u64> = log_before
        .lines()
        .filter(|l| l.contains("plan:auth-refactor"))
        .map(|l| {
            serde_json::from_str::<serde_json::Value>(l).unwrap()["seq"]
                .as_u64()
                .unwrap()
        })
        .collect();
    let new_seqs: Vec<u64> = log_after
        .lines()
        .filter(|l| l.contains("plan:auth-refactor"))
        .map(|l| {
            serde_json::from_str::<serde_json::Value>(l).unwrap()["seq"]
                .as_u64()
                .unwrap()
        })
        .collect();
    assert_eq!(old_seqs, new_seqs, "never renumbered");
    let last_line: serde_json::Value =
        serde_json::from_str(log_after.lines().last().unwrap()).unwrap();
    assert_eq!(last_line["type"], "log.cleaned");
    assert_eq!(
        last_line["seq"],
        r.last_seq + 1,
        "numbered past the old high-water mark"
    );
    assert_eq!(
        last_line["data"]["removed"],
        serde_json::json!(["plan:demo"])
    );

    // The index keeps every row, the cleaned review at its last state.
    let listed = r.repo.json(&["list", "--json"]);
    let rows = listed["artifacts"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{listed}");
    let demo = rows
        .iter()
        .find(|row| row["id"] == "plan:demo")
        .expect("the cleaned row");
    assert_eq!(demo["verdict"], "approve");
    assert_eq!(demo["submitted"], true);
    assert_eq!(demo["revision"], 1);
    assert!(
        Path::new(demo["poster"].as_str().unwrap()).exists(),
        "the poster too"
    );
    let sink = rows
        .iter()
        .find(|row| row["id"] == "plan:auth-refactor")
        .unwrap();
    assert_eq!(sink["open_threads"], 1);

    // A restart serves the open review and continues past the old mark.
    r.repo.run(&["serve", "--no-open"]).success();
    let status = r.repo.json(&["status", "--json"]);
    let ids: Vec<&str> = status["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["plan:auth-refactor"]);
    assert_eq!(status["artifacts"][0]["open_threads"], 1);
    assert_eq!(status["last_seq"], r.last_seq + 1);
    assert_eq!(r.repo.port(), r.port, "rebound on the recorded port");

    // Every old credential is dead; the new bearer already worked above.
    let old_page = get_with(
        r.port,
        "/a/plan:auth-refactor",
        &format!("Cookie: {}", r.cookie),
    );
    assert_eq!(status_of(&old_page), 401, "the old cookie: {old_page}");
    let old_bearer = get_with(
        r.port,
        "/cli/status",
        &format!("Authorization: Bearer {}", r.secret),
    );
    assert_eq!(status_of(&old_bearer), 401, "the old bearer: {old_bearer}");

    // The cleaned artifact cannot be opened; the kept one can, and the index
    // shows the cleaned one as not on this server, with its Remove.
    let gone = r
        .repo
        .run(&["open", "--no-open", "--artifact", "plan:demo"]);
    assert_eq!(gone.code, 2, "{}", gone.stdout);
    assert!(gone.stderr.contains("plan:demo"), "{}", gone.stderr);
    let cookie = cookie_via_open(&r.repo, "plan:auth-refactor");
    let index = get_with(r.port, "/", &format!("Cookie: {cookie}"));
    assert_eq!(status_of(&index), 200, "{index}");
    let demo_row = index
        .split("<li class=\"")
        .find(|s| s.contains("plan:demo"))
        .unwrap();
    assert!(!demo_row.starts_with("ix-row is-live"), "{demo_row}");
    assert!(
        demo_row.contains("Not on this server; push it again"),
        "{demo_row}"
    );
    assert!(demo_row.contains("Remove from index"));
    let sink_row = index
        .split("<li class=\"")
        .find(|s| s.contains("plan:auth-refactor"))
        .unwrap();
    assert!(sink_row.starts_with("ix-row is-live"), "{sink_row}");

    // In text mode, the same facts.
    r.repo.stop();
    let text = r.repo.run(&["clean"]);
    text.success();
    assert!(
        text.stdout.contains("no sent reviews to remove"),
        "{}",
        text.stdout
    );
    assert!(
        text.stdout
            .contains("kept 1 open review: plan:auth-refactor"),
        "{}",
        text.stdout
    );
    assert!(
        text.stdout.contains("rotated the session secret"),
        "{}",
        text.stdout
    );
}

#[test]
fn clean_with_nothing_sent_leaves_the_log_alone_and_still_rotates() {
    let repo = Repo::new();
    let demo = plan_in(&repo, "minimal.json", "demo.json");
    repo.json(&["plan", "push", &demo, "--json", "--no-open"]);
    let log_path = repo.state_dir().join("events.ndjson");
    let before = std::fs::read(&log_path).unwrap();
    let secret = repo.secret();

    let out = repo.json(&["clean", "--json"]);
    assert_eq!(out["server_stopped"], true);
    assert_eq!(out["removed"], serde_json::json!([]));
    assert_eq!(out["kept"], serde_json::json!(["plan:demo"]));
    assert_eq!(out["events_removed"], 0);
    assert_eq!(out["secret_rotated"], true);
    assert_eq!(std::fs::read(&log_path).unwrap(), before, "byte for byte");
    assert_ne!(repo.secret(), secret);

    repo.run(&["serve", "--no-open"]).success();
    let status = repo.json(&["status", "--json"]);
    assert_eq!(status["artifacts"][0]["id"], "plan:demo");
    repo.stop();
}

#[test]
fn clean_with_no_state_is_nothing_to_do() {
    let repo = Repo::new();
    let out = repo.json(&["clean", "--json"]);
    assert_eq!(out["ok"], true);
    assert_eq!(out["server_stopped"], false);
    assert_eq!(out["removed"], serde_json::json!([]));
    assert_eq!(out["secret_rotated"], false);
    assert!(!repo.state_dir().exists(), "clean creates nothing");
    let text = repo.run(&["clean"]);
    text.success();
    assert!(
        text.stdout.contains("no session secret to rotate"),
        "{}",
        text.stdout
    );
}

#[test]
fn a_cursor_survives_clean_and_new_events_number_past_the_old_mark() {
    let repo = Repo::new();
    let demo = plan_in(&repo, "minimal.json", "demo.json");
    let first = repo.json(&["plan", "push", &demo, "--json", "--no-open"]);
    let session = first["session"].as_str().unwrap().to_string();
    let port = repo.port();
    let cookie = cookie_via_open(&repo, "plan:demo");
    post_cmd(
        port,
        &cookie,
        "plan:demo",
        serde_json::json!({
            "cmd": "review.submit", "client_id": "c1", "verdict": "approve", "base_revision": 1,
        }),
    );
    // The agent hears the review and acknowledges it.
    let frame = repo.json(&["await", "--timeout", "5s", "--session", &session]);
    assert_eq!(frame["status"], "submitted", "{frame}");
    let seq = frame["seq"].as_u64().unwrap();
    repo.run(&["ack", "--seq", &seq.to_string(), "--session", &session])
        .success();
    let before = repo.json(&["status", "--json"]);
    assert_eq!(before["cursors"]["agent"], seq);
    let last_seq = before["last_seq"].as_u64().unwrap();

    let out = repo.json(&["clean", "--json"]);
    assert_eq!(out["removed"], serde_json::json!(["plan:demo"]));

    repo.run(&["serve", "--no-open"]).success();
    let after = repo.json(&["status", "--json"]);
    assert_eq!(
        after["cursors"]["agent"], seq,
        "the cursor record named no artifact and stayed"
    );
    assert_eq!(after["last_seq"], last_seq + 1);
    assert!(after["artifacts"].as_array().unwrap().is_empty());

    // A fresh push numbers past everything the old log ever held, so the
    // kept cursor still points below it.
    let sink = plan_in(&repo, "kitchen-sink.json", "sink.json");
    let pushed = repo.json(&["plan", "push", &sink, "--json", "--no-open"]);
    assert!(
        pushed["revision_seq"].as_u64().unwrap() > last_seq,
        "{pushed}"
    );
    repo.stop();
}

#[test]
fn clean_keeps_a_static_render_out_of_it() {
    // A render never touches the log; clean has nothing to say about it and
    // must leave its row and poster where they are.
    let repo = Repo::new();
    let demo = plan_in(&repo, "minimal.json", "demo.json");
    repo.json(&[
        "plan",
        "render",
        &demo,
        "--out",
        "demo.html",
        "--no-open",
        "--json",
    ]);
    let out = repo.json(&["clean", "--json"]);
    assert_eq!(out["removed"], serde_json::json!([]));
    assert_eq!(out["secret_rotated"], false, "no server has ever run here");
    let listed = repo.json(&["list", "--json"]);
    assert_eq!(listed["artifacts"].as_array().unwrap().len(), 1);
    assert!(repo.path().join("demo.html").exists());
}

// ---------------------------------------------------------------------------
// The log's numbering rule that clean relies on.
// ---------------------------------------------------------------------------

fn write_log(dir: &Path, seqs: &[u64]) {
    use artefacto::server::event::{Actor, Event, EVENT_FORMAT};
    let mut text = String::new();
    for seq in seqs {
        let event = Event {
            format: EVENT_FORMAT.to_string(),
            seq: *seq,
            ts: "2026-09-11T00:00:00Z".to_string(),
            artifact: "plan:x".to_string(),
            revision: 1,
            actor: Actor::Reviewer,
            r#type: "thread.opened".to_string(),
            data: serde_json::json!({ "thread": format!("c-{seq}") }),
            batch: None,
        };
        text.push_str(&serde_json::to_string(&event).unwrap());
        text.push('\n');
    }
    std::fs::write(dir.join("events.ndjson"), text).unwrap();
}

#[test]
fn a_log_with_gaps_replays_and_continues_past_its_highest_seq() {
    use artefacto::server::event::Actor;
    use artefacto::server::log::EventLog;
    let dir = tempfile::tempdir().unwrap();
    write_log(dir.path(), &[1, 4, 9]);
    let mut log = EventLog::open(dir.path()).expect("gaps are history, not corruption");
    assert_eq!(log.last_seq(), 9);
    assert_eq!(log.since(4).len(), 1, "seq 9 only");
    assert_eq!(log.since(3)[0].seq, 4);
    let next = log
        .append(
            "plan:x",
            1,
            Actor::Reviewer,
            "thread.opened",
            serde_json::json!({}),
        )
        .unwrap();
    assert_eq!(
        next.seq, 10,
        "continues past the highest number, not the count"
    );
}

#[test]
fn a_seq_that_does_not_go_up_is_corruption() {
    use artefacto::server::log::EventLog;
    for seqs in [&[1, 3, 3][..], &[2, 1][..], &[5, 5][..]] {
        let dir = tempfile::tempdir().unwrap();
        write_log(dir.path(), seqs);
        let err = EventLog::open(dir.path()).expect_err("refused");
        assert!(
            format!("{err:#}").contains("not above"),
            "{seqs:?}: {err:#}"
        );
    }
}
