//! The page command protocol: ids, duplicate suppression, validation.
//!
//! The tests that carry this file are
//! `a_repeated_client_id_is_answered_with_the_same_id` and
//! `two_tabs_racing_never_receive_the_same_thread_id`. Both are about the same
//! thing: assigning an id and checking for a duplicate have to happen in the
//! same breath as the append, or two tabs race and a retry becomes a second
//! comment.

mod support;
use support::*;

fn open_thread(s: &InProcess, cookie: &str, cid: &str, target: &str) -> serde_json::Value {
    s.post_cmd(
        cookie,
        "plan:demo",
        serde_json::json!({
            "cmd": "thread.open", "client_id": cid, "ref": target,
            "text": "why?", "blocking": false, "opened_revision": 1
        }),
    )
}

#[test]
fn opening_a_thread_assigns_a_server_side_id() {
    let s = InProcess::start();
    s.seed_artifact();
    let cookie = s.session_cookie("plan:demo");

    let reply = open_thread(&s, &cookie, "cid-1", "task:t-a");
    assert_eq!(reply["ok"], true, "{reply}");
    assert_eq!(reply["assigned"], "c-1", "the page never chooses an id");
    assert_eq!(s.thread_status("c-1"), "open");
}

#[test]
fn ids_increase_and_a_deleted_one_is_not_reused() {
    let s = InProcess::start();
    s.seed_artifact();
    let cookie = s.session_cookie("plan:demo");

    assert_eq!(
        open_thread(&s, &cookie, "cid-1", "task:t-a")["assigned"],
        "c-1"
    );
    assert_eq!(
        open_thread(&s, &cookie, "cid-2", "task:t-b")["assigned"],
        "c-2"
    );
    s.post_cmd(
        &cookie,
        "plan:demo",
        serde_json::json!({ "cmd": "thread.delete", "client_id": "cid-3", "thread": "c-1" }),
    );
    assert_eq!(
        open_thread(&s, &cookie, "cid-4", "task:t-a")["assigned"],
        "c-3",
        "spec 6.6: ids are never renumbered, so c-1 is gone rather than free"
    );
}

#[test]
fn a_repeated_client_id_is_answered_with_the_same_id() {
    let s = InProcess::start();
    s.seed_artifact();
    let cookie = s.session_cookie("plan:demo");

    let first = open_thread(&s, &cookie, "cid-1", "task:t-a");
    let after_first = s.last_seq();
    let second = open_thread(&s, &cookie, "cid-1", "task:t-a");

    assert_eq!(second["ok"], true, "a retry is not an error");
    assert_eq!(
        second["assigned"], first["assigned"],
        "the same id, which is why `committed` is a map and not a set"
    );
    assert_eq!(
        s.last_seq(),
        after_first,
        "nothing was appended the second time"
    );
    assert_eq!(s.thread_count(), 1);
}

#[test]
fn duplicate_suppression_survives_a_restart() {
    let s = InProcess::start();
    s.seed_artifact();
    let cookie = s.session_cookie("plan:demo");
    open_thread(&s, &cookie, "cid-1", "task:t-a");

    // A reconnect-and-retry is exactly the case where the server may have
    // restarted in between, so in-memory dedupe would be no dedupe at all.
    let s = s.restart();
    let cookie = s.session_cookie("plan:demo");
    let again = open_thread(&s, &cookie, "cid-1", "task:t-a");
    assert_eq!(again["assigned"], "c-1");
    assert_eq!(s.thread_count(), 1);
}

#[test]
fn two_tabs_racing_never_receive_the_same_thread_id() {
    let s = InProcess::start();
    s.seed_artifact();
    let cookie = s.session_cookie("plan:demo");

    let ids: Vec<String> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..12)
            .map(|i| {
                let s = &s;
                let cookie = cookie.clone();
                scope.spawn(move || {
                    open_thread(s, &cookie, &format!("cid-{i}"), "task:t-a")["assigned"]
                        .as_str()
                        .expect("an assigned id")
                        .to_string()
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    let unique: std::collections::BTreeSet<&String> = ids.iter().collect();
    assert_eq!(unique.len(), 12, "every tab got its own id: {ids:?}");
    assert_eq!(s.thread_count(), 12);
}

// --- validation -----------------------------------------------------------

#[test]
fn a_command_without_a_client_id_is_refused() {
    let s = InProcess::start();
    s.seed_artifact();
    let cookie = s.session_cookie("plan:demo");
    let r = s.post_cmd(
        &cookie,
        "plan:demo",
        serde_json::json!({ "cmd": "thread.open", "ref": "task:t-a", "text": "x", "opened_revision": 1 }),
    );
    assert_eq!(r["ok"], false);
    assert!(r["error"].as_str().unwrap().contains("client_id"), "{r}");
}

#[test]
fn a_command_without_the_revision_it_was_opened_against_is_refused() {
    // Spec 4.3: text written against revision 3 must not arrive labelled
    // revision 4.
    let s = InProcess::start();
    s.seed_artifact();
    let cookie = s.session_cookie("plan:demo");
    let r = s.post_cmd(
        &cookie,
        "plan:demo",
        serde_json::json!({ "cmd": "thread.open", "client_id": "cid-1", "ref": "task:t-a", "text": "x" }),
    );
    assert_eq!(r["ok"], false);
    assert!(
        r["error"].as_str().unwrap().contains("opened_revision"),
        "{r}"
    );
}

#[test]
fn a_revision_that_does_not_exist_is_refused() {
    let s = InProcess::start();
    s.seed_artifact();
    let cookie = s.session_cookie("plan:demo");
    let r = s.post_cmd(
        &cookie,
        "plan:demo",
        serde_json::json!({
            "cmd": "thread.open", "client_id": "cid-1", "ref": "task:t-a",
            "text": "x", "opened_revision": 9
        }),
    );
    assert_eq!(r["ok"], false);
}

#[test]
fn an_unknown_command_is_refused_and_the_page_keeps_working() {
    let s = InProcess::start();
    s.seed_artifact();
    let cookie = s.session_cookie("plan:demo");
    let r = s.post_cmd(
        &cookie,
        "plan:demo",
        serde_json::json!({ "cmd": "drop.database", "client_id": "cid-1" }),
    );
    assert_eq!(
        r["ok"], false,
        "a refusal is a body, not a dropped connection"
    );
    assert_eq!(
        open_thread(&s, &cookie, "cid-2", "task:t-a")["assigned"],
        "c-1"
    );
}

#[test]
fn a_reply_to_a_missing_thread_is_refused() {
    let s = InProcess::start();
    s.seed_artifact();
    let cookie = s.session_cookie("plan:demo");
    let r = s.post_cmd(
        &cookie,
        "plan:demo",
        serde_json::json!({
            "cmd": "thread.reply", "client_id": "cid-1", "thread": "c-99",
            "text": "x", "opened_revision": 1
        }),
    );
    assert_eq!(r["ok"], false);
    assert!(r["error"].as_str().unwrap().contains("c-99"), "{r}");
}

#[test]
fn a_thread_on_an_element_that_does_not_exist_is_refused() {
    let s = InProcess::start();
    s.seed_artifact();
    let cookie = s.session_cookie("plan:demo");
    let r = open_thread(&s, &cookie, "cid-1", "task:t-nope");
    assert_eq!(r["ok"], false);
    assert_eq!(
        s.last_seq(),
        1,
        "the log is append-only, so junk must not reach it"
    );
}

#[test]
fn oversized_text_is_refused_rather_than_logged() {
    let s = InProcess::start();
    s.seed_artifact();
    let cookie = s.session_cookie("plan:demo");
    let before = s.last_seq();
    let r = s.post_cmd(
        &cookie,
        "plan:demo",
        serde_json::json!({
            "cmd": "thread.open", "client_id": "cid-1", "ref": "task:t-a",
            "text": "x".repeat(70_000), "opened_revision": 1
        }),
    );
    assert_eq!(r["ok"], false);
    assert_eq!(s.last_seq(), before, "nothing permanent came of it");
}

#[test]
fn a_bad_verdict_is_refused() {
    let s = InProcess::start();
    s.seed_artifact();
    let cookie = s.session_cookie("plan:demo");
    let r = s.post_cmd(
        &cookie,
        "plan:demo",
        serde_json::json!({
            "cmd": "review.submit", "client_id": "cid-1",
            "verdict": "sounds fine", "base_revision": 1
        }),
    );
    assert_eq!(r["ok"], false);
}

// --- authentication and routing -------------------------------------------

#[test]
fn a_command_without_the_cookie_is_refused() {
    let s = InProcess::start();
    s.seed_artifact();
    let r = s.post_cmd_raw(&format!("Origin: {}\r\n", s.origin()), "plan:demo", "{}");
    assert_eq!(status_of(&r), 401);
}

#[test]
fn a_command_with_a_foreign_origin_is_refused() {
    let s = InProcess::start();
    s.seed_artifact();
    let cookie = s.session_cookie("plan:demo");
    let r = s.post_cmd_raw(
        &format!("Cookie: {cookie}\r\nOrigin: http://evil.example.com\r\n"),
        "plan:demo",
        "{}",
    );
    assert_eq!(status_of(&r), 403);
}

#[test]
fn a_command_with_no_origin_at_all_is_refused() {
    let s = InProcess::start();
    s.seed_artifact();
    let cookie = s.session_cookie("plan:demo");
    let r = s.post_cmd_raw(&format!("Cookie: {cookie}\r\n"), "plan:demo", "{}");
    assert_eq!(
        status_of(&r),
        403,
        "a page write needs an exact Origin, not merely absent-or-matching"
    );
}

#[test]
fn a_command_for_an_unknown_artifact_is_refused() {
    let s = InProcess::start();
    s.seed_artifact();
    let cookie = s.session_cookie("plan:demo");
    let r = s.post_cmd(
        &cookie,
        "plan:other",
        serde_json::json!({
            "cmd": "thread.open", "client_id": "cid-1", "ref": "task:t-a",
            "text": "x", "opened_revision": 1
        }),
    );
    assert_eq!(r["ok"], false);
    assert!(r["error"].as_str().unwrap().contains("plan:other"), "{r}");
}

// --- broadcast ------------------------------------------------------------

#[test]
fn a_second_tab_sees_the_first_tabs_comment() {
    let s = InProcess::start();
    s.seed_artifact();
    let cookie = s.session_cookie("plan:demo");
    let mut a = s.connect_page();
    let mut b = s.connect_page();
    let a_id = a.hello();
    b.hello();

    s.post_cmd(
        &cookie,
        "plan:demo",
        serde_json::json!({
            "cmd": "thread.open", "client_id": "cid-1", "ref": "task:t-a",
            "text": "why?", "blocking": false, "opened_revision": 1, "page": a_id
        }),
    );

    let frame = b.next_frame();
    assert_eq!(frame["events"][0]["type"], "thread.opened");
    assert_eq!(frame["events"][0]["data"]["thread"], "c-1");
    assert!(
        a.no_frame_within(std::time::Duration::from_millis(300)),
        "the sender already has the result in its response; a broadcast too would double it"
    );
}

#[test]
fn reviewer_text_is_carried_as_data_not_markup() {
    let s = InProcess::start();
    s.seed_artifact();
    let cookie = s.session_cookie("plan:demo");
    let mut watcher = s.connect_page();
    watcher.hello();

    let payload = "<img src=x onerror=alert(1)>";
    s.post_cmd(
        &cookie,
        "plan:demo",
        serde_json::json!({
            "cmd": "thread.open", "client_id": "cid-1", "ref": "task:t-a",
            "text": payload, "blocking": false, "opened_revision": 1
        }),
    );

    // Asserted against the JSON the socket actually sends. JSON does not
    // escape `<`, so the bytes are there verbatim — the guarantee is that they
    // arrive as a *string value*, and the page renders it as text.
    let frame = watcher.next_frame();
    assert_eq!(
        frame["events"][0]["data"]["text"].as_str().unwrap(),
        payload,
        "carried verbatim as a JSON string value"
    );
}

// --- activity -------------------------------------------------------------

#[test]
fn a_ping_marks_reviewer_activity_and_appends_nothing() {
    let s = InProcess::start();
    s.seed_artifact();
    let cookie = s.session_cookie("plan:demo");
    let before = s.last_seq();

    std::thread::sleep(std::time::Duration::from_millis(30));
    let r = s.post_cmd(&cookie, "plan:demo", serde_json::json!({ "cmd": "ping" }));

    assert_eq!(r["ok"], true);
    assert_eq!(s.last_seq(), before, "a ping is not an event");
    assert!(
        s.reviewer_idle_for() < std::time::Duration::from_millis(30),
        "but it is activity"
    );
}

#[test]
fn an_agents_traffic_is_not_the_reviewers_activity() {
    // An `await` long poll is an HTTP request every 90 seconds. If one clock
    // served both, the idle nudge could never fire with an agent attached.
    let s = InProcess::start();
    s.seed_artifact();
    let cookie = s.session_cookie("plan:demo");
    s.post_cmd(&cookie, "plan:demo", serde_json::json!({ "cmd": "ping" }));

    std::thread::sleep(std::time::Duration::from_millis(60));
    let bearer = format!("Bearer {}", s.shared.secret);
    s.get("/cli/status", &[("Authorization", &bearer)]);

    assert!(
        s.reviewer_idle_for() >= std::time::Duration::from_millis(60),
        "the agent's request must not reset the reviewer's clock"
    );
}
