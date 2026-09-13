//! The page, in a real browser.
//!
//! Every other server test drives the socket with a fake client, which proves
//! the server's half and nothing about the page's. These run the served page
//! in a headless Chromium over the DevTools protocol: the bootstrap redirect
//! sets the cookie, the nonce CSP lets the script run, the socket connects,
//! and what the server sends is what the reviewer sees.
//!
//! Without a Chromium these print a skip line and pass; see `support::browser`.

mod support;

use std::path::Path;
use support::browser::Browser;
use support::{InProcess, Repo};

struct Served {
    repo: Repo,
    server: InProcess,
    url: String,
    session: String,
    /// The served artifact's id, `plan:<meta.id>`, whatever the fixture.
    artifact: String,
}

impl Served {
    fn server(&self) -> &InProcess {
        &self.server
    }

    /// The page's tokenless address, for a second tab or a reload.
    fn page_url(&self) -> String {
        format!(
            "http://127.0.0.1:{}/a/{}",
            self.server().port,
            self.artifact
        )
    }

    /// Push the repository's plan.json as the next revision.
    fn push(&self, base: u32, extra: &[&str]) -> serde_json::Value {
        let plan = self.repo.path().join("plan.json");
        let base = base.to_string();
        let mut args = vec![
            "plan",
            "push",
            plan.to_str().unwrap(),
            "--json",
            "--no-open",
            "--base-revision",
            &base,
        ];
        args.extend_from_slice(extra);
        let out = self.repo.run(&args);
        assert_eq!(out.code, 0, "push failed: {}", out.stderr);
        serde_json::from_str(&out.stdout).expect("json")
    }

    /// Rewrite plan.json in place.
    fn edit_plan(&self, from: &str, to: &str) {
        let plan = self.repo.path().join("plan.json");
        let text = std::fs::read_to_string(&plan).unwrap();
        assert!(text.contains(from), "{from:?} is not in the plan");
        std::fs::write(&plan, text.replace(from, to)).unwrap();
    }
}

/// A repository with one pushed plan, and the bootstrap URL the push minted.
fn served(fixture: &str) -> Served {
    let repo = Repo::new();
    let server = InProcess::start_in(&repo);
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/plan")
        .join(fixture);
    let plan = repo.path().join("plan.json");
    std::fs::copy(&source, &plan).expect("copy the fixture");
    let out = repo.run(&[
        "plan",
        "push",
        plan.to_str().unwrap(),
        "--json",
        "--no-open",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let v: serde_json::Value = serde_json::from_str(&out.stdout).expect("json");
    Served {
        repo,
        server,
        url: v["url"].as_str().expect("a bootstrap url").to_string(),
        session: v["session"].as_str().expect("a session").to_string(),
        artifact: v["artifact"].as_str().expect("an artifact id").to_string(),
    }
}

#[test]
fn the_served_page_runs_its_script_under_the_nonce_policy() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);

    assert_eq!(
        page.text("location.pathname"),
        "/a/plan:demo",
        "the bootstrap redirect landed on the tokenless address"
    );
    assert_eq!(
        page.text("document.body.dataset.artefactoArtifact"),
        "plan:demo"
    );
    assert_eq!(
        page.eval("typeof window.artefactoPlan"),
        "object",
        "the inline script ran, so the nonce in the header matched the one on the tag"
    );
    assert_eq!(
        page.eval(
            "!!document.querySelector('.pv-dock') && !document.querySelector('.feedback-bar')"
        ),
        true,
        "the page mounted its controls: the conversation panel, no bottom bar"
    );
    let banner = page.text("document.querySelector('.pv-banner').textContent");
    assert!(
        banner.contains("send your review"),
        "a served page ends in Send review: {banner}"
    );
    assert!(
        !banner.contains("copy"),
        "not in copying feedback: {banner}"
    );
    assert_eq!(
        page.text("JSON.parse(document.getElementById('plan-data').textContent).meta.id"),
        "demo"
    );
    let errors = page.errors();
    assert!(
        errors.is_empty(),
        "the page must load with no console errors and no CSP refusals:\n{}",
        errors.join("\n")
    );
    let _ = (s.server(), &s.session);
}

fn debug(page: &mut support::browser::Page) -> serde_json::Value {
    page.eval("window.artefactoPlan.debug()")
}

fn connected(page: &mut support::browser::Page) {
    page.wait_until(
        "!!window.artefactoPlan && window.artefactoPlan.debug() && window.artefactoPlan.debug().connected && !window.artefactoPlan.debug().syncing",
        "the socket to connect and the state to load",
    );
}

/// Open the comment composer on `target`, type, and add. Returns once the
/// composer has closed, which is the page's own signal that the reply came.
fn comment(page: &mut support::browser::Page, target: &str, text: &str) {
    page.click(&format!("[data-plan-ref=\"{target}\"] .comment-btn"));
    page.type_into(
        &format!("[data-plan-ref=\"{target}\"] .pv-composers .composer textarea"),
        text,
    );
    page.click(&format!(
        "[data-plan-ref=\"{target}\"] .pv-composers .composer .composer-send"
    ));
    page.wait_until(
        &format!("!document.querySelector('[data-plan-ref=\"{target}\"] .pv-composers .composer')"),
        "the composer to close after the server accepted the comment",
    );
}

#[test]
fn the_page_connects_and_shows_who_holds_the_lease() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    // The push that served this page took the lease before the page
    // existed, and presence is announced on change only. The hello frame
    // is what tells a late page who is here.
    assert_eq!(
        page.text("document.querySelector('.pv-presence').textContent"),
        "agent waiting"
    );
    let d = debug(&mut page);
    assert_eq!(d["revision"], 1);
    assert!(d["page"].is_u64(), "hello handed the page its id");

    // Expiry reaches the page as a change.
    s.server()
        .age_lease(artefacto::server::lease::TTL + std::time::Duration::from_secs(1));
    page.wait_until(
        "document.querySelector('.pv-presence').textContent === 'no agent'",
        "the pill to say the agent is gone",
    );
    assert_eq!(
        page.text("document.querySelector('.pv-presence').dataset.mode"),
        "off"
    );
    assert_eq!(
        page.eval("document.querySelector('.pv-presence .ag-mark').classList.contains('is-off')"),
        true,
        "the mark hollows out when nobody holds the lease"
    );

    // And a new agent polling: the pill changes without the page doing anything.
    s.repo
        .run(&[
            "await",
            "--timeout",
            "1s",
            "--agent",
            "claude",
            "--takeover",
        ])
        .success();
    page.wait_until(
        "document.querySelector('.pv-presence').textContent === 'agent waiting'",
        "the pill to say the agent is waiting",
    );
    assert_eq!(
        page.text("document.querySelector('.pv-presence').dataset.mode"),
        "waiting"
    );
    assert_eq!(
        page.text("document.querySelector('.pv-presence').title"),
        "claude holds the lease"
    );
    assert_eq!(
        page.eval(
            "document.querySelector('.pv-presence .ag-mark').classList.contains('is-waiting')"
        ),
        true,
        "the mark turns amber for a polling agent"
    );
    let errors = page.errors();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

#[test]
fn a_comment_typed_into_the_page_is_logged_and_shown_once() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    // The composer's foot carries the blocking toggle beside the actions.
    page.click("[data-plan-ref=\"task:t-a\"] .comment-btn");
    assert_eq!(
        page.eval("!!document.querySelector('[data-plan-ref=\"task:t-a\"] .composer .comment-box-foot .comment-box-blocking') && \
                   !!document.querySelector('[data-plan-ref=\"task:t-a\"] .composer .comment-box-foot .composer-send.pv-btn')"),
        true,
        "one foot row: the toggle, then the actions as pv-btns"
    );
    page.click("[data-plan-ref=\"task:t-a\"] .composer .composer-cancel");

    comment(&mut page, "task:t-a", "why a trait here?");

    let logged = s.server().last_event_of_type("thread.opened");
    assert_eq!(logged["data"]["ref"], "task:t-a");
    assert_eq!(logged["data"]["text"], "why a trait here?");
    assert_eq!(logged["data"]["opened_revision"], 1);
    assert_eq!(logged["data"]["thread"], "c-1");
    assert_eq!(
        page.eval("document.querySelectorAll('.thread[data-thread=\"c-1\"]').length"),
        1,
        "the page applied its own reply once, and the broadcast skipped it"
    );
    assert_eq!(
        page.text(
            "document.querySelector('.thread[data-thread=\"c-1\"] .thread-text').textContent"
        ),
        "why a trait here?"
    );
    assert_eq!(
        page.text("document.querySelector('.feedback-bar-count').textContent"),
        "1 thread"
    );

    // A reply is not idempotent the way opening a thread is: applied from
    // the reply and again from a broadcast, it would show twice.
    reply(&mut page, "c-1", "and a follow-up");
    assert_eq!(
        page.eval("document.querySelectorAll('.thread[data-thread=\"c-1\"] .thread-msg').length"),
        2,
        "the page applied its own reply once, and the broadcast skipped it"
    );
    let errors = page.errors();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

/// Reply inside a thread from the page, and wait for the reply to land.
fn reply(page: &mut support::browser::Page, thread: &str, text: &str) {
    page.click(&format!(".thread[data-thread=\"{thread}\"] .thread-reply"));
    page.type_into(
        &format!(".thread[data-thread=\"{thread}\"] .composer textarea"),
        text,
    );
    page.click(&format!(
        ".thread[data-thread=\"{thread}\"] .composer .composer-send"
    ));
    page.wait_until(
        &format!("!document.querySelector('.thread[data-thread=\"{thread}\"] .composer')"),
        "the reply to be accepted",
    );
}

/// Make the page's `fetch` treat `path` specially: `mode` is "hold-response"
/// (the request goes out, the response waits `ms`), "hold-request" (the
/// request itself waits `ms`), "drop-response" (the request goes out and
/// the response never arrives), "hold-first" (only the first matching
/// response waits `ms`, so two replies arrive in reverse order), or
/// "hold-release" / "hold-first-release" (the response waits until the
/// test calls `release`, so no timing assumption is made). Shims chain, so
/// two paths can be shaped at once.
fn shape_fetch(page: &mut support::browser::Page, path: &str, mode: &str, ms: u64) {
    page.eval(&format!(
        "(function(){{ const prev = window.fetch; let heldFirst = false; \
          window.fetch = function (u, o) {{ \
            if (!String(u).endsWith({path})) return prev(u, o); \
            if ({mode} === 'hold-request') return new Promise(function (r) {{ setTimeout(r, {ms}); }}).then(function () {{ return prev(u, o); }}); \
            const p = prev(u, o); \
            if ({mode} === 'drop-response') return p.then(function () {{ return new Promise(function () {{}}); }}); \
            if ({mode} === 'hold-first' || {mode} === 'hold-first-release') {{ if (heldFirst) return p; heldFirst = true; }} \
            if ({mode} === 'hold-release' || {mode} === 'hold-first-release') \
              return p.then(function (r) {{ return new Promise(function (res) {{ (window.__releasers = window.__releasers || []).push({{ path: {path}, fn: function () {{ res(r); }} }}); }}); }}); \
            return p.then(function (r) {{ return new Promise(function (res) {{ setTimeout(function () {{ res(r); }}, {ms}); }}); }}); \
          }}; return true; }})()",
        path = serde_json::to_string(path).unwrap(),
        mode = serde_json::to_string(mode).unwrap(),
    ));
}

/// Let every response held by a "-release" shim through.
fn release(page: &mut support::browser::Page) {
    page.eval("(function(){ const r = window.__releasers || []; window.__releasers = []; r.forEach(function (h) { h.fn(); }); return true; })()");
}

/// Let through only the held responses for `path`.
fn release_path(page: &mut support::browser::Page, path: &str) {
    page.eval(&format!(
        "(function(){{ const all = window.__releasers || []; window.__releasers = all.filter(function (h) {{ return h.path !== {path}; }}); \
          all.filter(function (h) {{ return h.path === {path}; }}).forEach(function (h) {{ h.fn(); }}); return true; }})()",
        path = serde_json::to_string(path).unwrap(),
    ));
}

/// Make the page's `fetch` answer `path` with `status` and an empty body,
/// `times` times, then pass requests through; counts calls in `window.__calls`.
fn answer_with(page: &mut support::browser::Page, path: &str, status: u16, times: u64) {
    page.eval(&format!(
        "(function(){{ const prev = window.fetch; let left = {times}; window.__calls = 0; \
          window.fetch = function (u, o) {{ \
            if (!String(u).endsWith({path})) return prev(u, o); \
            window.__calls++; \
            if (left <= 0) return prev(u, o); left--; \
            return Promise.resolve(new Response('{{}}', {{ status: {status} }})); \
          }}; return true; }})()",
        path = serde_json::to_string(path).unwrap(),
    ));
}

#[test]
fn an_agents_reply_appears_in_the_thread_without_a_reload() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    comment(&mut page, "task:t-a", "is it worth it?");

    s.repo
        .run(&[
            "reply",
            "--session",
            &s.session,
            "--thread",
            "c-1",
            "so Redis can slot in",
        ])
        .success();
    page.wait_until(
        "document.querySelector('.thread[data-thread=\"c-1\"] .thread-msg[data-actor=\"agent\"]')",
        "the agent's reply to land in the thread",
    );
    assert_eq!(
        page.text("document.querySelector('.thread[data-thread=\"c-1\"] .thread-msg[data-actor=\"agent\"] .thread-text').textContent"),
        "so Redis can slot in"
    );

    // Resolving it flips the status and adds the note, still no reload.
    s.repo
        .run(&[
            "resolve",
            "c-1",
            "--session",
            &s.session,
            "--changed",
            "--note",
            "swapped it",
        ])
        .success();
    page.wait_until(
        "document.querySelector('.thread[data-thread=\"c-1\"]').dataset.status === 'changed'",
        "the thread to show as changed",
    );
    assert_eq!(
        page.eval("document.querySelectorAll('.thread[data-thread=\"c-1\"] .thread-msg').length"),
        2,
        "comment and reply stay turns of the conversation"
    );
    assert_eq!(
        page.eval(
            "!document.querySelector('.thread[data-thread=\"c-1\"] .thread-resolution').hidden"
        ),
        true,
        "the note is the resolution line"
    );
    assert_eq!(
        page.text("document.querySelector('.thread[data-thread=\"c-1\"] .thread-resolution .pv-chip').textContent"),
        "changed"
    );
    assert_eq!(
        page.text("document.querySelector('.thread[data-thread=\"c-1\"] .thread-resolution .thread-text').textContent"),
        "swapped it"
    );
}

#[test]
fn a_second_tab_sees_the_first_tabs_comment_exactly_once() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut a = browser.new_page();
    a.navigate(&s.url);
    connected(&mut a);
    // The bootstrap link is spent; the second tab rides the cookie.
    let mut b = browser.new_page();
    b.navigate(&s.page_url());
    connected(&mut b);

    comment(&mut a, "task:t-a", "from tab a");
    b.wait_until(
        "document.querySelector('.thread[data-thread=\"c-1\"]')",
        "tab b to receive the broadcast",
    );
    assert_eq!(b.eval("document.querySelectorAll('.thread').length"), 1);
    assert_eq!(a.eval("document.querySelectorAll('.thread').length"), 1);

    comment(&mut b, "task:t-a", "from tab b");
    a.wait_until(
        "document.querySelector('.thread[data-thread=\"c-2\"]')",
        "tab a to receive tab b's comment",
    );
    assert_eq!(a.eval("document.querySelectorAll('.thread').length"), 2);
    assert_eq!(b.eval("document.querySelectorAll('.thread').length"), 2);
    assert_eq!(s.server().thread_count(), 2);
}

#[test]
fn the_reviewer_approves_and_the_agent_receives_the_document() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    comment(&mut page, "task:t-a", "fine by me");

    page.click(".feedback-bar-approve");
    page.wait_until(
        "document.querySelector('.feedback-bar-sent').textContent.indexOf('review sent') === 0",
        "the bar to confirm the review was sent",
    );

    let out = s
        .repo
        .run(&["await", "--timeout", "5s", "--session", &s.session]);
    let r: serde_json::Value = serde_json::from_str(out.success().stdout.trim()).unwrap();
    assert_eq!(r["status"], "submitted");
    let last = r["events"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["data"]["verdict"], "approve");
    assert_eq!(last["data"]["base_revision"], 1);
    assert_eq!(
        last["data"]["feedback"]["comments"][0]["text"],
        "fine by me"
    );
}

#[test]
fn an_answer_to_an_open_question_arrives_as_data() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("kitchen-sink.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);

    page.click("[data-plan-ref=\"question:q-ttl\"] .comment-btn");
    page.type_into(
        "[data-plan-ref=\"question:q-ttl\"] .composer textarea",
        "an hour",
    );
    page.click("[data-plan-ref=\"question:q-ttl\"] .composer .composer-send");
    page.wait_until(
        "!document.querySelector('[data-plan-ref=\"question:q-ttl\"] .composer')",
        "the answer to be accepted",
    );
    let logged = s.server().last_event_of_type("question.answered");
    assert_eq!(logged["data"]["question"], "q-ttl");
    assert_eq!(logged["data"]["text"], "an hour");
    assert_eq!(
        page.text("document.querySelector('.pv-answer[data-answer-for=\"q-ttl\"] .pv-answer-text').textContent"),
        "an hour"
    );
    assert_eq!(
        page.eval("document.querySelector('.pv-answer[data-answer-for=\"q-ttl\"]').hidden"),
        false
    );
}

#[test]
fn marking_reviewed_reaches_the_log_and_survives_a_reload() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);

    page.click("[data-plan-ref=\"task:t-a\"] .reviewed-toggle input");
    support::wait_for(
        || s.server().count_events("element.reviewed") == 1,
        "the mark to be logged",
    );
    let logged = s.server().last_event_of_type("element.reviewed");
    assert_eq!(logged["data"]["ref"], "task:t-a");
    assert_eq!(logged["data"]["on"], true);
    page.wait_until(
        "document.querySelector('.feedback-bar-reviewed').textContent === '1/1 reviewed'",
        "the count to update",
    );

    page.navigate(&s.page_url());
    connected(&mut page);
    assert_eq!(
        page.eval(
            "document.querySelector('[data-plan-ref=\"task:t-a\"] .reviewed-toggle input').checked"
        ),
        true,
        "the server is the store: nothing was read from localStorage"
    );
    assert_eq!(
        page.eval("window.localStorage.length"),
        0,
        "and nothing was written there either"
    );
}

#[test]
fn asking_the_agent_wakes_it_from_the_page_and_from_a_thread() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);

    // Page level: the handle opens the panel, the composer is aimed at the
    // plan as a whole.
    page.click(".feedback-bar-chat");
    assert_eq!(
        page.eval("document.querySelector('.pv-panel-target').hidden"),
        true,
        "no target: the whole plan"
    );
    page.type_into(".pv-panel-composer textarea", "is this the whole plan?");
    enter(&mut page, ".pv-panel-composer textarea");
    page.wait_until(
        "document.querySelectorAll('.pv-chat-msg').length === 1",
        "the chat message to show",
    );
    let out = s
        .repo
        .run(&["await", "--timeout", "5s", "--session", &s.session]);
    let r: serde_json::Value = serde_json::from_str(out.success().stdout.trim()).unwrap();
    assert_eq!(r["status"], "chat");
    let seq = r["seq"].to_string();

    // Inside a comment thread: Ask the agent aims the panel at that thread,
    // and the question joins the thread on the element.
    comment(&mut page, "task:t-a", "and this?");
    page.click(".thread[data-thread=\"c-1\"] .thread-ask");
    assert_eq!(
        page.eval("!document.querySelector('.pv-panel-target').hidden"),
        true,
        "aimed at the thread"
    );
    page.type_into(".pv-panel-composer textarea", "really?");
    enter(&mut page, ".pv-panel-composer textarea");
    page.wait_until(
        "document.querySelectorAll('.thread[data-thread=\"c-1\"] .thread-msg').length === 2",
        "the question to join the thread",
    );
    let out = s.repo.run(&[
        "await",
        "--timeout",
        "5s",
        "--session",
        &s.session,
        "--ack",
        &seq,
    ]);
    let r: serde_json::Value = serde_json::from_str(out.success().stdout.trim()).unwrap();
    assert_eq!(r["status"], "chat");
    let last = r["events"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["data"]["thread"], "c-1");
    assert_eq!(last["data"]["text"], "really?");
}

#[test]
fn a_nudge_and_the_stop_show_as_notices() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);

    s.repo
        .run(&[
            "reply",
            "--session",
            &s.session,
            "--nudge",
            "have a look at phase one",
        ])
        .success();
    page.wait_until(
        "document.querySelector('.pv-notice[data-kind=\"nudge\"]')",
        "the nudge banner",
    );
    assert_eq!(
        page.text(
            "document.querySelector('.pv-notice[data-kind=\"nudge\"] .pv-notice-text').textContent"
        ),
        "have a look at phase one"
    );
    assert_eq!(
        page.eval("!!document.querySelector('.pv-notice[data-kind=\"nudge\"] .ag-mark') && document.querySelector('.pv-notice[data-kind=\"nudge\"] .pv-notice-kicker').textContent === 'The agent'"),
        true,
        "a nudge speaks as the agent: the mark and the kicker"
    );
    page.click(".pv-notice[data-kind=\"nudge\"] .pv-notice-dismiss");
    assert_eq!(
        page.eval("!!document.querySelector('.pv-notice[data-kind=\"nudge\"]')"),
        false
    );

    s.server().shared.request_stop();
    page.wait_until(
        "document.querySelector('.pv-notice[data-kind=\"stopping\"]')",
        "the stopping banner",
    );
}

#[test]
fn the_static_export_selftest_passes_in_a_real_browser() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let repo = Repo::new();
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plan/kitchen-sink.json");
    let out = repo.path().join("plan.html");
    repo.run(&[
        "plan",
        "render",
        source.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--no-open",
    ])
    .success();
    let mut page = browser.new_page();
    page.navigate(&format!("file://{}#selftest", out.display()));
    page.wait_until(
        "document.getElementById('selftest-result')",
        "the selftest to finish",
    );
    let result = page.text("document.getElementById('selftest-result').textContent");
    assert!(
        result.starts_with("ARTEFACTO_SELFTEST_PASS"),
        "the page's own harness failed:\n{result}"
    );
    assert_eq!(
        page.eval("!!document.querySelector('.feedback-bar-copy.pv-btn')"),
        true,
        "the static export keeps the clipboard flow, as a button of the same family"
    );
    // The static editor is the served composer's twin: one foot row with the
    // toggle and the actions as pv-btns, and no mark anywhere (no agent).
    page.click("[data-plan-ref=\"task:t-session-store\"] .comment-btn");
    assert_eq!(
        page.eval("!!document.querySelector('[data-plan-ref=\"task:t-session-store\"] .comment-box .comment-box-foot .comment-box-blocking') && \
                   !!document.querySelector('[data-plan-ref=\"task:t-session-store\"] .comment-box .comment-box-foot .composer-send.pv-btn')"),
        true,
        "the static editor has the served composer's foot"
    );
    assert_eq!(
        page.eval("document.querySelectorAll('.ag-mark, .ask-btn, .el-actions').length"),
        0,
        "nothing of the agent on a static page"
    );
    page.screenshot(&screenshot_path("static-export"));
    assert_eq!(
        page.eval("!!document.querySelector('.pv-presence')"),
        false,
        "and has no presence pill: nothing to be present"
    );
}

// --- the races a fake client cannot reach (spec 14) -------------------------

#[test]
fn a_push_while_typing_keeps_the_draft_and_sends_the_revision_it_opened_against() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);

    page.click("[data-plan-ref=\"task:t-a\"] .comment-btn");
    page.type_into(
        "[data-plan-ref=\"task:t-a\"] .composer textarea",
        "half a thought",
    );
    let composer_id = page
        .text("document.querySelector('[data-plan-ref=\"task:t-a\"] .composer').dataset.composer");

    s.edit_plan("Demo plan", "Demo plan, revised");
    s.push(1, &[]);
    page.wait_until(
        "document.body.dataset.artefactoRevision === '2'",
        "the page to swap in revision 2",
    );
    assert_eq!(
        page.text("document.querySelector('.pv-head h1').textContent"),
        "Demo plan, revised",
        "the body is the new revision's"
    );
    page.wait_until(
        "document.querySelector('.pv-notice[data-kind=\"revision\"]')",
        "the revision banner",
    );
    assert!(
        page.text("document.querySelector('.pv-notice[data-kind=\"revision\"] .pv-notice-text').textContent")
            .starts_with("Revision 2 pushed"),
        "the banner names the revision"
    );
    assert_eq!(
        page.text("document.querySelector('.pv-notice[data-kind=\"revision\"]').title"),
        "Previously: Demo plan",
        "the previous title on hover"
    );

    // The composer is still there, with its text, under the same id.
    let composer = "document.querySelector('[data-plan-ref=\"task:t-a\"] .composer')";
    assert_eq!(
        page.text(&format!("{composer}.dataset.composer")),
        composer_id
    );
    assert_eq!(
        page.text(&format!("{composer}.querySelector('textarea').value")),
        "half a thought"
    );
    assert_eq!(page.text(&format!("{composer}.dataset.revision")), "1");

    page.click("[data-plan-ref=\"task:t-a\"] .composer .composer-send");
    page.wait_until(
        "!document.querySelector('[data-plan-ref=\"task:t-a\"] .composer')",
        "the comment to be accepted",
    );
    let logged = s.server().last_event_of_type("thread.opened");
    assert_eq!(
        logged["revision"], 2,
        "committed against the current revision"
    );
    assert_eq!(
        logged["data"]["opened_revision"], 1,
        "but labelled with the one the composer opened against"
    );
    let errors = page.errors();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

#[test]
fn a_draft_and_a_thread_whose_element_was_removed_land_in_the_recovery_panel() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    comment(&mut page, "task:t-a", "keep me");
    page.click("[data-plan-ref=\"task:t-a\"] .comment-btn");
    page.type_into("[data-plan-ref=\"task:t-a\"] .composer textarea", "unsent");

    // Revision 2 replaces t-a with t-b.
    s.edit_plan("\"id\": \"t-a\"", "\"id\": \"t-b\"");
    s.push(1, &[]);
    page.wait_until(
        "document.body.dataset.artefactoRevision === '2'",
        "revision 2",
    );
    page.wait_until(
        "document.querySelector('.pv-recovery')",
        "the recovery panel",
    );
    assert_eq!(
        page.text(
            "document.querySelector('.pv-recovery .thread[data-thread=\"c-1\"]').dataset.status"
        ),
        "unanchored"
    );
    assert_eq!(
        page.text("document.querySelector('.pv-recovery .thread[data-thread=\"c-1\"] .thread-text').textContent"),
        "keep me"
    );
    assert_eq!(
        page.eval("!!document.querySelector('[data-plan-ref=\"task:t-b\"] .thread')"),
        false,
        "not shown under a different element"
    );
    assert_eq!(
        page.text("document.querySelector('.pv-orphan-draft .thread-text').textContent"),
        "unsent"
    );
    assert_eq!(
        s.server().thread_status("c-1"),
        "unanchored",
        "the server agrees"
    );
    assert!(
        page.text("document.querySelector('.pv-notice[data-kind=\"revision\"] .pv-notice-text').textContent")
            .contains("1 thread lost its element"),
        "the banner says so"
    );

    // Revision 3 brings t-a back: the thread re-anchors, the panel empties.
    s.edit_plan("\"id\": \"t-b\"", "\"id\": \"t-a\"");
    s.push(2, &[]);
    page.wait_until(
        "document.body.dataset.artefactoRevision === '3'",
        "revision 3",
    );
    page.wait_until(
        "document.querySelector('[data-plan-ref=\"task:t-a\"] .thread[data-thread=\"c-1\"]')",
        "the thread back under its element",
    );
    assert_eq!(
        page.text("document.querySelector('[data-plan-ref=\"task:t-a\"] .thread[data-thread=\"c-1\"]').dataset.status"),
        "open"
    );
    assert_eq!(
        page.text(
            "document.querySelector('[data-plan-ref=\"task:t-a\"] .composer textarea').value"
        ),
        "unsent",
        "the orphaned draft came back with its element"
    );
    page.click("[data-plan-ref=\"task:t-a\"] .composer .composer-cancel");
    assert_eq!(page.eval("!!document.querySelector('.pv-recovery')"), false);
}

#[test]
fn focus_and_caret_survive_a_push() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    // A reviewer types into a composer they can see: the phase is open.
    page.eval("(function(){ document.querySelectorAll('details.phase').forEach(function(d){ d.open = true; }); return true; })()");
    page.click("[data-plan-ref=\"task:t-a\"] .comment-btn");
    page.type_into(
        "[data-plan-ref=\"task:t-a\"] .composer textarea",
        "hello world",
    );
    page.eval(
        "(function(){ const t = document.querySelector('[data-plan-ref=\"task:t-a\"] .composer textarea'); t.focus(); t.setSelectionRange(5, 5); return true; })()",
    );
    assert_eq!(page.eval("document.activeElement.tagName"), "TEXTAREA");

    s.edit_plan("Demo plan", "Demo plan, revised");
    s.push(1, &[]);
    page.wait_until(
        "document.body.dataset.artefactoRevision === '2'",
        "revision 2",
    );
    assert_eq!(
        page.eval("document.activeElement === document.querySelector('[data-plan-ref=\"task:t-a\"] .composer textarea')"),
        true,
        "focus came back to the composer"
    );
    assert_eq!(
        page.eval("document.activeElement.selectionStart"),
        5,
        "and so did the caret"
    );
    assert_eq!(page.eval("document.activeElement.selectionEnd"), 5);
}

#[test]
fn the_scroll_position_is_anchored_to_an_element_across_a_push() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("kitchen-sink.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);

    // Open every phase, then put a late task at a known place on screen.
    page.eval(
        "(function(){ document.querySelectorAll('details.phase').forEach(function(d){ d.open = true; }); return true; })()",
    );
    // Instant: the stylesheet asks for smooth scrolling, and a headless
    // page never advances that animation.
    page.eval(
        "(function(){ const el = document.querySelector('[data-plan-ref=\"task:t-cleanup\"]'); window.scrollTo({ top: el.getBoundingClientRect().top + window.scrollY - 40, behavior: 'instant' }); return true; })()",
    );
    let before = page.eval(
        "document.querySelector('[data-plan-ref=\"task:t-cleanup\"]').getBoundingClientRect().top",
    );
    let before = before.as_f64().unwrap();
    assert!(
        page.eval("window.scrollY").as_f64().unwrap() > 100.0,
        "the page is scrolled"
    );
    assert!(
        before < 400.0,
        "the task is near the top of the viewport: {before}"
    );
    let open_before = page.eval("document.querySelectorAll('details.phase[open]').length");

    // Revision 2 puts a whole new phase above everything, so every pixel
    // offset below it is wrong and only an element anchor can be right.
    s.edit_plan(
        "\"phases\": [",
        "\"phases\": [\n    { \"id\": \"p-new\", \"title\": \"A new phase first\", \"summary_md\": \"Inserted above.\", \"tasks\": [ { \"id\": \"t-new-a\", \"title\": \"New A\" }, { \"id\": \"t-new-b\", \"title\": \"New B\" } ] },",
    );
    s.push(1, &[]);
    page.wait_until(
        "document.body.dataset.artefactoRevision === '2'",
        "revision 2",
    );
    assert_eq!(
        page.eval("!!document.querySelector('[data-plan-ref=\"phase:p-new\"]')"),
        true
    );
    let after = page.eval(
        "document.querySelector('[data-plan-ref=\"task:t-cleanup\"]').getBoundingClientRect().top",
    );
    let after = after.as_f64().unwrap();
    assert!(
        (after - before).abs() <= 2.0,
        "the task the reviewer was reading moved from {before} to {after}"
    );
    // Disclosure is restored by element id: the phases that were open still
    // are, whatever moved around them.
    assert_eq!(
        page.eval("document.querySelector('[data-plan-ref=\"phase:p-core\"]').open"),
        true
    );
    assert_eq!(
        page.eval("document.querySelector('[data-plan-ref=\"phase:p-backend\"]').open"),
        true
    );
    assert_eq!(
        page.eval("document.querySelectorAll('details.phase[open]').length"),
        open_before,
        "and nothing else was opened for the reader"
    );
}

#[test]
fn a_reconnect_catches_up_without_duplicating_anything() {
    // Against a real daemon, because only a process exit closes the idle
    // HTTP connections a browser keeps alive. An in-process restart leaves
    // them attached to a server nobody is running, and a fetch on one hangs
    // forever, which is a fact about the harness rather than the page.
    let Some(browser) = Browser::launch() else {
        return;
    };
    let repo = Repo::new();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plan/minimal.json");
    let plan = repo.path().join("plan.json");
    std::fs::copy(&source, &plan).unwrap();
    let out = repo.run(&[
        "plan",
        "push",
        plan.to_str().unwrap(),
        "--json",
        "--no-open",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let v: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    let url = v["url"].as_str().unwrap().to_string();
    let page_url = format!("http://127.0.0.1:{}/a/plan:demo", repo.port());

    let mut a = browser.new_page();
    a.navigate(&url);
    connected(&mut a);
    comment(&mut a, "task:t-a", "before the restart");
    let mut b = browser.new_page();
    b.navigate(&page_url);
    connected(&mut b);

    // The daemon stops: both pages hear it and lose their sockets.
    repo.stop();
    a.wait_until(
        "!window.artefactoPlan.debug().connected && window.artefactoPlan.debug().notices.indexOf('stopping') >= 0",
        "tab a to notice the stop",
    );
    repo.run(&["serve", "--no-open"]).success();

    // A comment and a reply from the other tab land while tab a may still
    // be in its backoff. Its POSTs retry onto a fresh connection; tab a's
    // socket was closed, so it never hears the broadcasts. The reply is the
    // one that matters: opening a thread is idempotent by id, a reply is not.
    comment(&mut b, "task:t-a", "while it was away");
    reply(&mut b, "c-1", "replied while away");

    a.wait_until(
        "window.artefactoPlan.debug().reconnects >= 1 && window.artefactoPlan.debug().connected && !window.artefactoPlan.debug().syncing",
        "tab a to reconnect and catch up",
    );
    a.wait_until(
        "document.querySelectorAll('.thread').length === 2",
        "the comment made while away to appear",
    );
    assert_eq!(
        a.eval("document.querySelectorAll('.thread[data-thread=\"c-1\"]').length"),
        1
    );
    assert_eq!(
        a.eval("document.querySelectorAll('.thread[data-thread=\"c-2\"]').length"),
        1
    );
    assert_eq!(
        a.eval("document.querySelectorAll('.thread[data-thread=\"c-1\"] .thread-msg').length"),
        2,
        "the reply made while away appears exactly once"
    );
    assert_eq!(
        a.eval("!!document.querySelector('.pv-notice[data-kind=\"stopping\"]')"),
        false,
        "the stopping notice clears once the page is back"
    );

    // And the live path works again on the new socket.
    b.wait_until(
        "window.artefactoPlan.debug().connected && !window.artefactoPlan.debug().syncing",
        "tab b to be back too",
    );
    b.click(".thread[data-thread=\"c-1\"] .thread-reply");
    b.type_into(".thread[data-thread=\"c-1\"] .composer textarea", "after");
    b.click(".thread[data-thread=\"c-1\"] .composer .composer-send");
    a.wait_until(
        "document.querySelectorAll('.thread[data-thread=\"c-1\"] .thread-msg').length === 3",
        "the reply to arrive on the new socket",
    );
    assert_eq!(a.eval("document.querySelectorAll('.thread').length"), 2);
    let errors = a.errors();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
    repo.stop();
}

#[test]
fn activity_pings_are_throttled_and_mark_the_reviewer_active() {
    // Spec 6.2: activity is a throttled page ping on scroll, keys, pointer
    // and visibility, at most one per 30 seconds. The window is shortened
    // here so the throttle itself can be watched.
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    assert_eq!(
        s.server().ping_count(),
        0,
        "arriving is activity; no ping is needed for it"
    );

    page.eval("window.artefactoPlan.settings.pingEveryMs = 400");
    // Arrival started the clock; the first ping waits for the window.
    std::thread::sleep(std::time::Duration::from_millis(450));
    s.server().mark_reviewer_activity_at(-1_000_000);
    let scroll = "(function(){ document.dispatchEvent(new Event('scroll')); return true; })()";
    page.eval(scroll);
    support::wait_for(|| s.server().ping_count() == 1, "the first ping");
    assert!(
        s.server().reviewer_idle_for() < std::time::Duration::from_secs(5),
        "a scroll is activity"
    );

    // Inside the window: nothing more, however much the reviewer moves.
    for _ in 0..5 {
        page.eval("(function(){ document.dispatchEvent(new KeyboardEvent('keydown')); document.dispatchEvent(new Event('pointermove')); return true; })()");
    }
    std::thread::sleep(std::time::Duration::from_millis(150));
    assert_eq!(s.server().ping_count(), 1, "one ping per window");

    // Past it: one more.
    std::thread::sleep(std::time::Duration::from_millis(300));
    page.eval(scroll);
    support::wait_for(|| s.server().ping_count() == 2, "the next window's ping");
}

// --- the page's own writes around a catch-up ------------------------------

/// Cut the page's socket so it reconnects and catches up, with `/state`
/// shaped by `mode` so a write lands on a chosen side of the snapshot.
fn catch_up_with(s: &Served, page: &mut support::browser::Page, mode: &str) {
    shape_fetch(page, "/state", mode, 1500);
    artefacto::server::socket::close_all(&s.server().shared);
    page.wait_until(
        "window.artefactoPlan.debug().reconnects >= 1 && window.artefactoPlan.debug().connected && window.artefactoPlan.debug().syncing",
        "the page to reconnect and start catching up",
    );
}

/// Make the page's `fetch` answer `path` with `status` and `body` (JSON),
/// every time; counts calls in `window.__calls`.
fn answer_json(page: &mut support::browser::Page, path: &str, status: u16, body: &str) {
    page.eval(&format!(
        "(function(){{ const prev = window.fetch; window.__calls = 0; \
          window.fetch = function (u, o) {{ \
            if (!String(u).endsWith({path})) return prev(u, o); \
            window.__calls++; \
            return Promise.resolve(new Response({body}, {{ status: {status}, headers: {{ 'Content-Type': 'application/json' }} }})); \
          }}; return true; }})()",
        path = serde_json::to_string(path).unwrap(),
        body = serde_json::to_string(body).unwrap(),
    ));
}

/// Cut the page's socket so it reconnects and catches up, holding the
/// `/state` response until `release`.
fn catch_up_held(s: &Served, page: &mut support::browser::Page) {
    shape_fetch(page, "/state", "hold-release", 0);
    artefacto::server::socket::close_all(&s.server().shared);
    page.wait_until(
        "window.artefactoPlan.debug().reconnects >= 1 && window.artefactoPlan.debug().connected && window.artefactoPlan.debug().syncing",
        "the page to reconnect and start catching up",
    );
}

#[test]
fn a_write_made_while_catching_up_is_kept_whichever_side_of_the_snapshot_it_lands() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    for (mode, text) in [
        ("hold-response", "snapshot taken before this"),
        ("hold-request", "snapshot taken after this"),
    ] {
        let s = served("minimal.json");
        let mut page = browser.new_page();
        page.navigate(&s.url);
        connected(&mut page);
        comment(&mut page, "task:t-a", "before");
        catch_up_with(&s, &mut page, mode);

        // A reply, not a comment: opening a thread is idempotent by id and
        // would hide a double apply.
        reply(&mut page, "c-1", text);
        connected(&mut page);
        assert_eq!(
            page.eval("document.querySelectorAll('.thread').length"),
            1,
            "{mode}: one thread"
        );
        assert_eq!(
            page.eval(
                "document.querySelectorAll('.thread[data-thread=\"c-1\"] .thread-msg').length"
            ),
            2,
            "{mode}: the page's own write survives the snapshot, once"
        );
        assert_eq!(
            debug(&mut page)["threads"][0]["messages"][1],
            format!("reviewer: {text}")
        );
        let errors = page.errors();
        assert!(errors.is_empty(), "{}", errors.join("\n"));
    }
}

#[test]
fn a_frame_received_while_catching_up_is_applied_once() {
    // The snapshot's last_seq is the cursor: a logged event the snapshot
    // already holds is skipped when the buffered frames drain.
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    comment(&mut page, "task:t-a", "before");
    catch_up_with(&s, &mut page, "hold-request");

    // Another tab replies while the snapshot request is being held: the
    // frame is buffered, and the snapshot, taken after, holds the reply.
    let cookie = s.server().session_cookie("plan:demo");
    s.server().post_cmd(
        &cookie,
        "plan:demo",
        serde_json::json!({
            "cmd": "thread.reply", "client_id": "cid-other", "thread": "c-1",
            "text": "meanwhile", "opened_revision": 1,
        }),
    );
    connected(&mut page);
    assert_eq!(
        page.eval("document.querySelectorAll('.thread[data-thread=\"c-1\"] .thread-msg').length"),
        2,
        "in the snapshot and in a buffered frame, shown once"
    );
}

#[test]
fn a_frame_carrying_the_pages_own_write_is_not_applied_twice() {
    // After a reconnect a write in flight may name the old socket, and the
    // server then broadcasts it to the new one. The page recognises its
    // own client id.
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    comment(&mut page, "task:t-a", "mine");
    reply(&mut page, "c-1", "my reply");
    let logged = s.server().last_event_of_type("thread.replied");
    assert!(
        logged["data"]["client_id"].is_string(),
        "the event carries the client id"
    );
    let frame = serde_json::json!({
        "format": "artefacto.frame/1", "seq": logged["seq"], "events": [logged],
    });
    page.eval(&format!("window.artefactoPlan.injectFrame({frame})"));
    assert_eq!(
        page.eval("document.querySelectorAll('.thread[data-thread=\"c-1\"] .thread-msg').length"),
        2,
        "the page's own reply, delivered back to it, is not a second reply"
    );
}

// --- sends that overlap a swap or a reload ----------------------------------

#[test]
fn a_push_during_a_send_does_not_leave_a_composer_that_sends_twice() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    shape_fetch(&mut page, "/cmd", "hold-response", 1500);
    page.click("[data-plan-ref=\"task:t-a\"] .comment-btn");
    page.type_into(
        "[data-plan-ref=\"task:t-a\"] .composer textarea",
        "slow to land",
    );
    page.click("[data-plan-ref=\"task:t-a\"] .composer .composer-send");
    support::wait_for(|| s.server().thread_count() == 1, "the server has it");

    s.edit_plan("Demo plan", "Demo plan, revised");
    s.push(1, &[]);
    page.wait_until(
        "document.body.dataset.artefactoRevision === '2'",
        "revision 2",
    );
    page.wait_until(
        "!document.querySelector('.composer')",
        "the reply to close the composer the swap re-created",
    );
    assert_eq!(
        page.eval("Object.keys(window.artefactoPlan.debug().drafts).length"),
        0
    );
    assert_eq!(page.eval("document.querySelectorAll('.thread').length"), 1);
    assert_eq!(s.server().thread_count(), 1);
}

#[test]
fn a_send_repeated_after_a_reload_is_the_same_command() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    shape_fetch(&mut page, "/cmd", "drop-response", 0);
    page.click("[data-plan-ref=\"task:t-a\"] .comment-btn");
    page.type_into(
        "[data-plan-ref=\"task:t-a\"] .composer textarea",
        "sent into the void",
    );
    page.click("[data-plan-ref=\"task:t-a\"] .composer .composer-send");
    support::wait_for(|| s.server().thread_count() == 1, "the server has it");

    // The reply never came; the draft is still stored, with its client id.
    page.navigate(&s.page_url());
    connected(&mut page);
    assert_eq!(
        page.text(
            "document.querySelector('[data-plan-ref=\"task:t-a\"] .composer textarea').value"
        ),
        "sent into the void"
    );
    page.click("[data-plan-ref=\"task:t-a\"] .composer .composer-send");
    page.wait_until(
        "!document.querySelector('.composer')",
        "the repeat to be answered",
    );
    assert_eq!(
        s.server().thread_count(),
        1,
        "a repeat of the same client id is not a second comment"
    );
    page.wait_until(
        "document.querySelectorAll('.thread').length === 1",
        "the thread shown once",
    );
}

// --- what the review found in the DOM ---------------------------------------

#[test]
fn a_thread_shows_once_however_many_rows_share_its_element() {
    // Every acceptance row of a task carries the task's ref. Each row keeps
    // its comment button and its own quote; the thread lives on the task.
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("kitchen-sink.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    let buttons = page.eval(
        "document.querySelectorAll('[data-plan-ref=\"task:t-session-store\"] .comment-btn').length",
    );
    assert!(
        buttons.as_u64().unwrap() >= 2,
        "the task and its acceptance rows: {buttons}"
    );
    comment(&mut page, "task:t-session-store", "on the task");
    assert_eq!(
        page.eval("document.querySelectorAll('.thread[data-thread=\"c-1\"]').length"),
        1
    );

    // A row's button quotes the row, not the task heading.
    page.click("li[data-plan-ref=\"task:t-session-store\"] .comment-btn");
    page.type_into(
        "[data-plan-ref=\"task:t-session-store\"] .composer textarea",
        "on a criterion",
    );
    page.click("[data-plan-ref=\"task:t-session-store\"] .composer .composer-send");
    page.wait_until("!document.querySelector('.composer')", "the second comment");
    let quote = s.server().last_event_of_type("thread.opened")["data"]["quote"]
        .as_str()
        .unwrap()
        .to_string();
    let heading = page.text("document.querySelector('[data-plan-ref=\"task:t-session-store\"] h3, [data-plan-ref=\"task:t-session-store\"] h4') ? document.querySelector('[data-plan-ref=\"task:t-session-store\"] h3, [data-plan-ref=\"task:t-session-store\"] h4').textContent : ''");
    assert!(!quote.is_empty());
    assert_ne!(
        quote,
        heading.trim(),
        "quoted the row, not the task heading"
    );
    assert_eq!(page.eval("document.querySelectorAll('.thread').length"), 2);
}

#[test]
fn a_reply_draft_survives_a_reload() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    comment(&mut page, "task:t-a", "a thread");
    page.click(".thread[data-thread=\"c-1\"] .thread-reply");
    page.type_into(
        ".thread[data-thread=\"c-1\"] .composer textarea",
        "half a reply",
    );

    page.navigate(&s.page_url());
    connected(&mut page);
    page.wait_until(
        "document.querySelector('.thread[data-thread=\"c-1\"] .composer textarea')",
        "the reply composer back inside its thread",
    );
    assert_eq!(
        page.text(
            "document.querySelector('.thread[data-thread=\"c-1\"] .composer textarea').value"
        ),
        "half a reply"
    );
    assert_eq!(
        page.eval("!!document.querySelector('.pv-recovery')"),
        false,
        "not orphaned"
    );
}

#[test]
fn the_chat_panel_stays_open_and_focused_across_a_push() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    page.click(".feedback-bar-chat");
    page.type_into(".pv-panel-composer textarea", "half a question");
    page.eval("(function(){ document.querySelector('.pv-panel-composer textarea').focus(); return true; })()");

    s.edit_plan("Demo plan", "Demo plan, revised");
    s.push(1, &[]);
    page.wait_until(
        "document.body.dataset.artefactoRevision === '2'",
        "revision 2",
    );
    assert_eq!(
        page.eval("document.querySelector('.pv-dock').classList.contains('is-hidden')"),
        false,
        "still open"
    );
    assert_eq!(
        page.text("document.querySelector('.pv-panel-composer textarea').value"),
        "half a question"
    );
    assert_eq!(
        page.eval(
            "document.activeElement === document.querySelector('.pv-panel-composer textarea')"
        ),
        true
    );
}

#[test]
fn a_reply_on_a_thread_that_lost_its_element_still_sends() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    comment(&mut page, "task:t-a", "orphan me");
    s.edit_plan("\"id\": \"t-a\"", "\"id\": \"t-b\"");
    s.push(1, &[]);
    page.wait_until(
        "document.querySelector('.pv-recovery .thread[data-thread=\"c-1\"]')",
        "the recovery panel",
    );

    page.click(".pv-recovery .thread[data-thread=\"c-1\"] .thread-reply");
    page.wait_until(
        "document.querySelector('.pv-recovery .thread[data-thread=\"c-1\"] .composer')",
        "a composer on the orphaned thread",
    );
    page.type_into(
        ".pv-recovery .thread[data-thread=\"c-1\"] .composer textarea",
        "still here",
    );
    page.click(".pv-recovery .thread[data-thread=\"c-1\"] .composer .composer-send");
    page.wait_until(
        "!document.querySelector('.composer')",
        "the reply to be accepted",
    );
    assert_eq!(
        page.eval("document.querySelectorAll('.pv-recovery .thread[data-thread=\"c-1\"] .thread-msg').length"),
        2
    );
    assert_eq!(
        s.server().last_event_of_type("thread.replied")["data"]["text"],
        "still here"
    );
}

#[test]
fn a_lost_cookie_reads_as_signed_out_not_as_a_gone_server() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    page.eval("window.artefactoPlan.settings.backoffMs = [50, 50, 50]");
    page.clear_cookies();
    artefacto::server::socket::close_all(&s.server().shared);
    page.wait_until(
        "window.artefactoPlan.debug().lost",
        "the page to learn it is signed out",
    );
    assert_eq!(
        page.text("document.querySelector('.pv-presence').textContent"),
        "signed out"
    );
    assert_eq!(
        page.eval("!!document.querySelector('.pv-notice[data-kind=\"lost\"]')"),
        true
    );
    assert_eq!(
        page.eval("!!document.querySelector('.pv-notice[data-kind=\"gone\"]')"),
        false
    );
    assert_eq!(
        page.eval("document.querySelector('.feedback-bar-send').disabled"),
        true
    );
    assert_eq!(
        page.text("document.querySelector('.pv-notice[data-kind=\"lost\"] code').textContent"),
        "artefacto open",
        "the command to run is set as code, not as literal backticks"
    );
}

// --- the fix slice, reviewed fresh --------------------------------------------

#[test]
fn another_artifacts_events_do_not_touch_this_page() {
    // The socket carries every artifact's frames. A comment on another
    // plan must not count here, and its push must not swap this body.
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let other = s.repo.path().join("other.json");
    std::fs::write(
        &other,
        std::fs::read_to_string(s.repo.path().join("plan.json"))
            .unwrap()
            .replace("\"id\": \"demo\"", "\"id\": \"other\"")
            .replace("Demo plan", "Other plan"),
    )
    .unwrap();
    let out = s.repo.run(&[
        "plan",
        "push",
        other.to_str().unwrap(),
        "--json",
        "--no-open",
        "--session",
        &s.session,
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);

    let cookie = s.server().session_cookie("plan:other");
    s.server().post_cmd(
        &cookie,
        "plan:other",
        serde_json::json!({
            "cmd": "thread.open", "client_id": "cid-other", "ref": "task:t-a",
            "text": "on the other plan", "opened_revision": 1,
        }),
    );
    std::fs::write(
        &other,
        std::fs::read_to_string(&other)
            .unwrap()
            .replace("Other plan", "Other plan, revised"),
    )
    .unwrap();
    let out = s.repo.run(&[
        "plan",
        "push",
        other.to_str().unwrap(),
        "--json",
        "--no-open",
        "--session",
        &s.session,
        "--base-revision",
        "1",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);

    // The page has seen both frames go by: its high-water mark moved.
    page.wait_until(
        &format!(
            "window.artefactoPlan.debug().lastSeq >= {}",
            s.server().last_seq()
        ),
        "the frames to have been delivered",
    );
    assert_eq!(page.eval("document.querySelectorAll('.thread').length"), 0);
    assert_eq!(
        page.text("document.querySelector('.feedback-bar-count').textContent"),
        "0 threads"
    );
    assert_eq!(
        page.text("document.querySelector('.pv-head h1').textContent"),
        "Demo plan"
    );
    assert_eq!(page.text("document.body.dataset.artefactoRevision"), "1");
    assert_eq!(debug(&mut page)["revision"], 1);
    assert_eq!(
        page.eval("!!document.querySelector('.pv-notice[data-kind=\"revision\"]')"),
        false
    );
}

#[test]
fn a_phase_comment_opens_on_the_phase_not_its_first_task() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("kitchen-sink.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    // A task composer first, so a lookup by descent would find it.
    page.click("[data-plan-ref=\"task:t-config-flag\"] .comment-btn");
    page.click("[data-plan-ref=\"phase:p-core\"] .comment-btn");
    assert_eq!(
        page.eval("document.querySelectorAll('.composer').length"),
        2
    );
    assert_eq!(
        page.eval("document.querySelector('.pv-composers[data-composers-for=\"phase:p-core\"] .composer') !== null"),
        true,
        "the phase's composer is in the phase's own host"
    );
    assert_eq!(
        page.eval("document.querySelector('.pv-composers[data-composers-for=\"phase:p-core\"] .composer').closest('.task')"),
        serde_json::Value::Null,
        "and not inside a task card"
    );
    page.type_into(
        ".pv-composers[data-composers-for=\"phase:p-core\"] .composer textarea",
        "on the phase",
    );
    page.click(".pv-composers[data-composers-for=\"phase:p-core\"] .composer .composer-send");
    page.wait_until(
        "document.querySelectorAll('.composer').length === 1",
        "the phase comment sent",
    );
    assert_eq!(
        s.server().last_event_of_type("thread.opened")["data"]["ref"],
        "phase:p-core"
    );
}

#[test]
fn a_reviewed_mark_in_flight_survives_a_push() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    shape_fetch(&mut page, "/cmd", "hold-response", 2500);
    page.click("[data-plan-ref=\"task:t-a\"] .reviewed-toggle input");
    support::wait_for(
        || s.server().count_events("element.reviewed") == 1,
        "the mark landed",
    );

    s.edit_plan("Demo plan", "Demo plan, revised");
    s.push(1, &[]);
    page.wait_until(
        "document.body.dataset.artefactoRevision === '2'",
        "revision 2",
    );
    let toggle = "document.querySelector('[data-plan-ref=\"task:t-a\"] .reviewed-toggle')";
    assert_eq!(
        page.eval(&format!("{toggle}.querySelector('input').checked")),
        true,
        "the reviewer's choice, across the swap"
    );
    assert_eq!(
        page.text(&format!(
            "{toggle}.querySelector('.reviewed-toggle-text').textContent"
        )),
        "Reviewed"
    );

    // And once the held reply lands, still.
    std::thread::sleep(std::time::Duration::from_millis(3000));
    assert_eq!(
        page.eval(&format!("{toggle}.querySelector('input').checked")),
        true
    );
    assert_eq!(debug(&mut page)["reviewed"][0], "task:t-a");
}

#[test]
fn a_push_does_not_reset_the_activity_clock() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    page.eval("window.artefactoPlan.settings.pingEveryMs = 400");
    std::thread::sleep(std::time::Duration::from_millis(450));

    s.edit_plan("Demo plan", "Demo plan, revised");
    s.push(1, &[]);
    page.wait_until(
        "document.body.dataset.artefactoRevision === '2'",
        "revision 2",
    );
    page.eval("(function(){ document.dispatchEvent(new Event('scroll')); return true; })()");
    std::thread::sleep(std::time::Duration::from_millis(150));
    assert_eq!(
        s.server().ping_count(),
        1,
        "the window had passed before the push; the swap does not restart it"
    );
}

#[test]
fn a_request_that_never_answers_is_reported_and_the_write_still_appears() {
    // The fetch deadline turns a wedged request into an error the reviewer
    // can see; and since the write may have landed, the page looks.
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    page.eval("window.artefactoPlan.settings.fetchTimeoutMs = 200");
    // The request goes out; the response never comes back; only the
    // deadline's abort settles the promise.
    page.eval(
        "(function(){ const orig = window.fetch; window.fetch = function (u, o) { \
           if (!String(u).endsWith('/cmd')) return orig(u, o); \
           orig(u, Object.assign({}, o, { signal: undefined })).catch(function () {}); \
           return new Promise(function (res, rej) { if (o && o.signal) o.signal.addEventListener('abort', function () { rej(new DOMException('aborted', 'AbortError')); }); }); \
         }; return true; })()",
    );
    page.click("[data-plan-ref=\"task:t-a\"] .comment-btn");
    page.type_into(
        "[data-plan-ref=\"task:t-a\"] .composer textarea",
        "into a black hole",
    );
    page.click("[data-plan-ref=\"task:t-a\"] .composer .composer-send");
    page.wait_until(
        "document.querySelector('.composer .pv-error')",
        "the composer to report the failure",
    );
    assert!(page
        .text("document.querySelector('.composer .pv-error').textContent")
        .starts_with("Not sent"));
    assert_eq!(s.server().thread_count(), 1, "the write landed");
    page.wait_until(
        "document.querySelectorAll('.thread[data-thread=\"c-1\"]').length === 1",
        "the page to find the write it could not hear about",
    );
}

#[test]
fn send_review_cannot_be_sent_twice_while_in_flight() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    shape_fetch(&mut page, "/cmd", "hold-response", 1500);
    // The second click forces the button back on, the way a frame arriving
    // mid-flight once did.
    page.eval(
        "(function(){ const b = document.querySelector('.feedback-bar-send'); b.click(); b.disabled = false; b.click(); return true; })()",
    );
    support::wait_for(
        || s.server().count_events("review.submitted") == 1,
        "one submission",
    );
    std::thread::sleep(std::time::Duration::from_millis(2000));
    assert_eq!(
        s.server().count_events("review.submitted"),
        1,
        "and only one"
    );
    page.wait_until(
        "document.querySelector('.feedback-bar-send').classList.contains('is-filled')",
        "the button to settle after its pulse",
    );
}

#[test]
fn a_revision_learned_from_a_snapshot_reports_what_it_addressed() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    comment(&mut page, "task:t-a", "please change this");
    catch_up_with(&s, &mut page, "hold-request");

    // The push lands while the snapshot request is held, so the page
    // learns of revision 2 from the snapshot and skips the buffered frame.
    let resolutions = s.repo.path().join("resolutions.json");
    std::fs::write(
        &resolutions,
        serde_json::json!([{ "thread": "c-1", "status": "changed", "note": "done" }]).to_string(),
    )
    .unwrap();
    s.edit_plan("Demo plan", "Demo plan, revised");
    s.push(1, &["--resolutions", resolutions.to_str().unwrap()]);
    connected(&mut page);
    page.wait_until(
        "document.body.dataset.artefactoRevision === '2'",
        "revision 2",
    );
    let banner = page.text(
        "document.querySelector('.pv-notice[data-kind=\"revision\"] .pv-notice-text').textContent",
    );
    assert!(banner.starts_with("Revision 2 pushed"), "{banner}");
    assert!(banner.contains("1 addressed"), "{banner}");
    assert_eq!(
        page.text("document.querySelector('.thread[data-thread=\"c-1\"]').dataset.status"),
        "changed"
    );
    assert_eq!(
        page.eval("document.querySelectorAll('.pv-notice[data-kind=\"revision\"]').length"),
        1,
        "one banner, from the snapshot, not a second from the skipped frame"
    );
}

#[test]
fn a_chat_draft_is_visible_after_a_reload() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    page.click(".feedback-bar-chat");
    page.type_into(".pv-panel-composer textarea", "half a question");
    page.navigate(&s.page_url());
    connected(&mut page);
    assert_eq!(
        page.eval("document.querySelector('.pv-dock').classList.contains('is-hidden')"),
        false,
        "the panel opens for its draft"
    );
    assert_eq!(
        page.text("document.querySelector('.pv-panel-composer textarea').value"),
        "half a question"
    );
}

// --- the round-seven fix slice, reviewed fresh --------------------------------

#[test]
fn the_chat_panel_stays_closed_once_the_reviewer_closes_it() {
    // A draft opens the panel once, when the page first finds it. After
    // the reviewer closes the panel, no frame, reply, or render reopens it.
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    page.click(".feedback-bar-chat");
    page.type_into(".pv-panel-composer textarea", "kept but closed");
    page.click(".feedback-bar-chat");
    assert_eq!(
        page.eval("document.querySelector('.pv-dock').classList.contains('is-hidden')"),
        true
    );
    page.eval(
        "window.artefactoPlan.injectFrame({ format: 'artefacto.frame/1', seq: 999, events: [] })",
    );
    assert_eq!(
        page.eval("document.querySelector('.pv-dock').classList.contains('is-hidden')"),
        true,
        "a frame does not reopen it"
    );
    comment(&mut page, "task:t-a", "a comment");
    assert_eq!(
        page.eval("document.querySelector('.pv-dock').classList.contains('is-hidden')"),
        true,
        "an own reply does not reopen it"
    );
    assert_eq!(
        page.text("document.querySelector('.pv-panel-composer textarea').value"),
        "kept but closed",
        "the draft is kept"
    );

    // Closing with nothing written keeps nothing.
    page.click(".feedback-bar-chat");
    page.click(".feedback-bar-chat");
    page.click(".feedback-bar-chat");
    page.type_into(".pv-panel-composer textarea", "");
    page.click(".feedback-bar-chat");
    page.navigate(&s.page_url());
    connected(&mut page);
    assert_eq!(
        page.eval("document.querySelector('.pv-dock').classList.contains('is-hidden')"),
        true
    );
    assert_eq!(
        page.eval("Object.keys(window.artefactoPlan.debug().drafts).length"),
        0,
        "an empty chat draft is not stored"
    );
}

#[test]
fn reviewed_mark_replies_out_of_order_settle_on_the_servers_value() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    // The first reply (on) is held until released; the second (off)
    // arrives first.
    let applied_before = debug(&mut page)["applied"].as_u64().unwrap();
    shape_fetch(&mut page, "/cmd", "hold-first-release", 0);
    page.click("[data-plan-ref=\"task:t-a\"] .reviewed-toggle input");
    support::wait_for(
        || s.server().count_events("element.reviewed") == 1,
        "on landed",
    );
    page.click("[data-plan-ref=\"task:t-a\"] .reviewed-toggle input");
    support::wait_for(
        || s.server().count_events("element.reviewed") == 2,
        "off landed",
    );
    assert_eq!(
        s.server().last_event_of_type("element.reviewed")["data"]["on"],
        false
    );
    page.wait_until(
        &format!(
            "window.artefactoPlan.debug().applied === {}",
            applied_before + 1
        ),
        "the off reply to be in",
    );
    release(&mut page);
    page.wait_until(
        "window.artefactoPlan.debug().pending === 0",
        "the on reply to be in",
    );
    assert_eq!(
        page.eval("window.artefactoPlan.debug().reviewed.length"),
        0,
        "the later write wins, whichever reply came last"
    );
    assert_eq!(
        page.eval(
            "document.querySelector('[data-plan-ref=\"task:t-a\"] .reviewed-toggle input').checked"
        ),
        false
    );
}

#[test]
fn a_reviewed_mark_answered_during_a_resync_does_not_flicker() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    catch_up_held(&s, &mut page);
    page.click("[data-plan-ref=\"task:t-a\"] .reviewed-toggle input");
    support::wait_for(
        || s.server().count_events("element.reviewed") == 1,
        "the mark landed",
    );
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert_eq!(
        page.eval("window.artefactoPlan.debug().syncing"),
        true,
        "still catching up"
    );
    assert_eq!(
        page.eval(
            "document.querySelector('[data-plan-ref=\"task:t-a\"] .reviewed-toggle input').checked"
        ),
        true,
        "the reviewer's choice holds while the reply waits in the buffer"
    );
    release(&mut page);
    connected(&mut page);
    assert_eq!(
        page.eval(
            "document.querySelector('[data-plan-ref=\"task:t-a\"] .reviewed-toggle input').checked"
        ),
        true
    );
    assert_eq!(debug(&mut page)["reviewed"][0], "task:t-a");
}

#[test]
fn a_reply_answered_after_the_snapshot_that_held_it_is_not_applied_twice() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    comment(&mut page, "task:t-a", "a thread");
    shape_fetch(&mut page, "/cmd", "hold-release", 0);
    page.click(".thread[data-thread=\"c-1\"] .thread-reply");
    page.type_into(
        ".thread[data-thread=\"c-1\"] .composer textarea",
        "slow reply",
    );
    page.click(".thread[data-thread=\"c-1\"] .composer .composer-send");
    support::wait_for(
        || s.server().count_events("thread.replied") == 1,
        "the reply landed",
    );
    // A resync completes with the reply inside the snapshot, then the
    // held reply is let through.
    artefacto::server::socket::close_all(&s.server().shared);
    page.wait_until(
        "window.artefactoPlan.debug().reconnects >= 1 && window.artefactoPlan.debug().connected && !window.artefactoPlan.debug().syncing",
        "the page to catch up",
    );
    release(&mut page);
    page.wait_until(
        "!document.querySelector('.composer')",
        "the released reply to close the composer",
    );
    assert_eq!(
        page.eval("document.querySelectorAll('.thread[data-thread=\"c-1\"] .thread-msg').length"),
        2,
        "the snapshot already held it"
    );
}

/// Replace the page's WebSocket with one that fails `times` times, then
/// hands back the real one.
fn failing_sockets(page: &mut support::browser::Page, times: u64) {
    page.eval(&format!(
        "(function(){{ const Real = window.__realWebSocket || window.WebSocket; window.__realWebSocket = Real; \
          let left = {times}; \
          window.WebSocket = function (url) {{ \
            if (left <= 0) return new Real(url); \
            left--; const s = {{ close: function () {{}} }}; \
            setTimeout(function () {{ if (s.onclose) s.onclose({{}}); }}, 10); return s; \
          }}; return true; }})()"
    ));
}

#[test]
fn a_transient_socket_failure_with_http_alive_recovers_through_the_probe() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    page.eval("window.artefactoPlan.settings.backoffMs = [40, 40, 40]");
    failing_sockets(&mut page, 4);
    artefacto::server::socket::close_all(&s.server().shared);
    // A write lands while the socket is down; the probe's snapshot has it.
    let cookie = s.server().session_cookie("plan:demo");
    s.server().post_cmd(
        &cookie,
        "plan:demo",
        serde_json::json!({
            "cmd": "thread.open", "client_id": "cid-outage", "ref": "task:t-a",
            "text": "during the outage", "opened_revision": 1,
        }),
    );
    page.wait_until(
        "window.artefactoPlan.debug().connected && !window.artefactoPlan.debug().syncing && window.artefactoPlan.debug().reconnects >= 4",
        "the page to get through",
    );
    assert_eq!(page.eval("window.artefactoPlan.debug().gone"), false);
    page.wait_until(
        "document.querySelectorAll('.thread').length === 1",
        "the write made during the outage",
    );
}

#[test]
fn a_socket_that_keeps_failing_while_http_answers_still_gives_up() {
    // Spec 4.3: a clear "server gone" state after a bounded number of
    // retries, even when /state keeps answering.
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    page.eval("window.artefactoPlan.settings.backoffMs = [30, 30, 30]");
    failing_sockets(&mut page, 1_000_000);
    artefacto::server::socket::close_all(&s.server().shared);
    page.wait_until("window.artefactoPlan.debug().gone", "the page to give up");
    assert_eq!(
        page.eval("!!document.querySelector('.pv-notice[data-kind=\"gone\"] .pv-notice-action')"),
        true,
        "with a Retry"
    );
    assert_eq!(
        page.text("document.querySelector('.pv-presence').textContent"),
        "server gone"
    );
}

// --- the round-eight fix slice, reviewed fresh --------------------------------

#[test]
fn an_older_own_reply_does_not_win_over_a_newer_frame_or_a_newer_buffered_reply() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    // Two tabs: a's reply is held; b turns the mark off meanwhile, and a
    // hears that as a frame. a's older reply, released after, must lose.
    let s = served("minimal.json");
    let mut a = browser.new_page();
    a.navigate(&s.url);
    connected(&mut a);
    let mut b = browser.new_page();
    b.navigate(&s.page_url());
    connected(&mut b);
    shape_fetch(&mut a, "/cmd", "hold-release", 0);
    a.click("[data-plan-ref=\"task:t-a\"] .reviewed-toggle input");
    b.wait_until(
        "window.artefactoPlan.debug().reviewed.length === 1",
        "b to hear a's mark",
    );
    b.click("[data-plan-ref=\"task:t-a\"] .reviewed-toggle input");
    support::wait_for(
        || s.server().count_events("element.reviewed") == 2,
        "off landed",
    );
    a.wait_until(
        "window.artefactoPlan.debug().reviewed.length === 0",
        "a to hear b's frame",
    );
    release(&mut a);
    a.wait_until(
        "window.artefactoPlan.debug().pending === 0",
        "a's own reply to be in",
    );
    assert_eq!(
        a.eval("window.artefactoPlan.debug().reviewed.length"),
        0,
        "the frame's newer value stands"
    );
    assert_eq!(
        a.eval(
            "document.querySelector('[data-plan-ref=\"task:t-a\"] .reviewed-toggle input').checked"
        ),
        false
    );

    // One tab, catching up: both replies are buffered, the older one last.
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    catch_up_held(&s, &mut page);
    shape_fetch(&mut page, "/cmd", "hold-first-release", 0);
    page.click("[data-plan-ref=\"task:t-a\"] .reviewed-toggle input");
    support::wait_for(
        || s.server().count_events("element.reviewed") == 1,
        "on landed",
    );
    page.click("[data-plan-ref=\"task:t-a\"] .reviewed-toggle input");
    support::wait_for(
        || s.server().count_events("element.reviewed") == 2,
        "off landed",
    );
    page.wait_until(
        "window.artefactoPlan.debug().buffered === 1",
        "the off reply to be buffered",
    );
    release_path(&mut page, "/cmd");
    page.wait_until(
        "window.artefactoPlan.debug().buffered === 2",
        "the on reply to be buffered after it",
    );
    release_path(&mut page, "/state");
    connected(&mut page);
    assert_eq!(
        page.eval("window.artefactoPlan.debug().reviewed.length"),
        0,
        "drained in log order"
    );
}

#[test]
fn a_write_during_the_end_of_backoff_probe_is_kept() {
    // The socket keeps failing, so nothing but the probe's own snapshot
    // can put the mark on the page: the assertion is taken right after
    // that snapshot is applied.
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    page.eval("window.artefactoPlan.settings.backoffMs = [30, 30, 30]");
    failing_sockets(&mut page, 1_000_000);
    shape_fetch(&mut page, "/state", "hold-release", 0);
    artefacto::server::socket::close_all(&s.server().shared);
    page.wait_until(
        "(window.__releasers || []).length > 0",
        "the probe's snapshot request to be in flight",
    );
    page.click("[data-plan-ref=\"task:t-a\"] .reviewed-toggle input");
    support::wait_for(
        || s.server().count_events("element.reviewed") == 1,
        "the mark landed",
    );
    page.wait_until(
        "window.artefactoPlan.debug().buffered === 1",
        "the reply to be buffered",
    );
    release(&mut page);
    page.wait_until(
        "!window.artefactoPlan.debug().syncing",
        "the probe's snapshot to be applied",
    );
    assert_eq!(
        page.eval("window.artefactoPlan.debug().connected"),
        false,
        "still no socket"
    );
    assert_eq!(
        debug(&mut page)["reviewed"][0],
        "task:t-a",
        "the write made during the probe survives its snapshot"
    );
}

#[test]
fn a_toggles_error_line_cannot_be_clicked_into_a_second_mark() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    answer_with(&mut page, "/cmd", 500, 1_000);
    page.click("[data-plan-ref=\"task:t-a\"] .reviewed-toggle input");
    page.wait_until(
        "document.querySelector('[data-plan-ref=\"task:t-a\"] .reviewed-toggle + .pv-error')",
        "the error line beside the label",
    );
    let calls = page.eval("window.__calls");
    let checked = page.eval(
        "document.querySelector('[data-plan-ref=\"task:t-a\"] .reviewed-toggle input').checked",
    );
    page.click("[data-plan-ref=\"task:t-a\"] .reviewed-toggle + .pv-error");
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert_eq!(
        page.eval("window.__calls"),
        calls,
        "reading the error is not a click on the mark"
    );
    assert_eq!(
        page.eval(
            "document.querySelector('[data-plan-ref=\"task:t-a\"] .reviewed-toggle input').checked"
        ),
        checked
    );
    assert_eq!(
        page.eval("!!document.querySelector('.reviewed-toggle .pv-error')"),
        false,
        "and never inside the label"
    );
    let _ = s;
}

#[test]
fn an_error_line_clears_on_the_next_success() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    // Every attempt of the first send fails (the page retries four times).
    answer_with(&mut page, "/cmd", 500, 4);
    page.click("[data-plan-ref=\"task:t-a\"] .reviewed-toggle input");
    page.wait_until(
        "document.querySelector('.pv-error')",
        "the first send fails",
    );
    page.click("[data-plan-ref=\"task:t-a\"] .reviewed-toggle input");
    support::wait_for(
        || s.server().count_events("element.reviewed") == 1,
        "the second lands",
    );
    page.wait_until(
        "!document.querySelector('.pv-error')",
        "the error line to clear",
    );
}

#[test]
fn a_gone_artifact_found_by_the_probe_reads_as_lost_everywhere() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    page.eval("window.artefactoPlan.settings.backoffMs = [30, 30, 30]");
    failing_sockets(&mut page, 1_000_000);
    answer_with(&mut page, "/state", 404, 1_000);
    artefacto::server::socket::close_all(&s.server().shared);
    page.wait_until(
        "window.artefactoPlan.debug().lost",
        "the page to learn the artifact is gone",
    );
    assert_eq!(
        page.text("document.querySelector('.pv-presence').textContent"),
        "signed out"
    );
    assert_eq!(
        page.eval("document.querySelector('.feedback-bar-send').disabled"),
        true
    );
    assert!(page
        .text(
            "document.querySelector('.pv-notice[data-kind=\"lost\"] .pv-notice-text').textContent"
        )
        .contains("no longer on the server"));
    assert_eq!(
        page.eval("window.artefactoPlan.debug().syncing"),
        false,
        "a lost page is not left catching up with a buffer nobody empties"
    );
}

#[test]
fn a_thread_that_goes_from_changed_to_declined_between_snapshots_is_counted() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    comment(&mut page, "task:t-a", "please");
    let resolutions = s.repo.path().join("resolutions.json");
    std::fs::write(
        &resolutions,
        serde_json::json!([{ "thread": "c-1", "status": "changed" }]).to_string(),
    )
    .unwrap();
    s.edit_plan("Demo plan", "Demo plan, revised");
    s.push(1, &["--resolutions", resolutions.to_str().unwrap()]);
    page.wait_until(
        "document.body.dataset.artefactoRevision === '2'",
        "revision 2 by frame",
    );

    catch_up_with(&s, &mut page, "hold-request");
    std::fs::write(
        &resolutions,
        serde_json::json!([{ "thread": "c-1", "status": "declined" }]).to_string(),
    )
    .unwrap();
    s.edit_plan("Demo plan, revised", "Demo plan, revised again");
    s.push(2, &["--resolutions", resolutions.to_str().unwrap()]);
    connected(&mut page);
    page.wait_until(
        "document.body.dataset.artefactoRevision === '3'",
        "revision 3 by snapshot",
    );
    let banner = page.text(
        "document.querySelector('.pv-notice[data-kind=\"revision\"] .pv-notice-text').textContent",
    );
    assert!(banner.contains("1 declined"), "{banner}");
}

#[test]
fn a_socket_that_opens_and_closes_at_once_still_gives_up() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    page.eval("window.artefactoPlan.settings.backoffMs = [30, 30, 30]; window.artefactoPlan.settings.stableAfterMs = 100000");
    // A socket that completes its handshake and is closed at once.
    page.eval(
        "(function(){ window.WebSocket = function () { const s = { close: function () {} }; \
           setTimeout(function () { if (s.onopen) s.onopen({}); }, 5); \
           setTimeout(function () { if (s.onclose) s.onclose({}); }, 15); return s; }; return true; })()",
    );
    artefacto::server::socket::close_all(&s.server().shared);
    page.wait_until("window.artefactoPlan.debug().gone", "the page to give up");
    assert!(page
        .text(
            "document.querySelector('.pv-notice[data-kind=\"gone\"] .pv-notice-text').textContent"
        )
        .contains("socket will not connect"));
}

#[test]
fn a_sent_chat_message_leaves_a_place_for_the_next() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    page.click(".feedback-bar-chat");
    page.type_into(".pv-panel-composer textarea", "first");
    page.click(".pv-panel-composer .thread-composer-send");
    page.wait_until(
        "document.querySelectorAll('.pv-chat-msg').length === 1",
        "the message shown",
    );
    assert_eq!(
        page.eval("document.querySelector('.pv-dock').classList.contains('is-hidden')"),
        false
    );
    assert_eq!(
        page.eval("!!document.querySelector('.pv-panel-composer textarea')"),
        true,
        "a fresh composer"
    );
    assert_eq!(
        page.text("document.querySelector('.pv-panel-composer textarea').value"),
        ""
    );
    let _ = s;
    assert_eq!(
        page.eval("Object.keys(window.artefactoPlan.debug().drafts).length"),
        0,
        "a composer nobody has typed into is not a draft"
    );
}

// --- the round-nine fix slice, reviewed fresh ---------------------------------

#[test]
fn a_resync_that_overtakes_the_probe_does_not_read_as_gone() {
    // A write's repeated reply starts a resync of its own; if that one
    // overtakes the probe's, HTTP has answered, and the page must keep
    // trying the socket rather than say the server is gone.
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    page.eval("window.artefactoPlan.settings.backoffMs = [30, 30, 30]");
    failing_sockets(&mut page, 3);
    shape_fetch(&mut page, "/state", "hold-release", 0);
    artefacto::server::socket::close_all(&s.server().shared);
    page.wait_until(
        "(window.__releasers || []).length > 0",
        "the probe's snapshot request in flight",
    );
    // A retried write: the server says it already did this.
    answer_json(
        &mut page,
        "/cmd",
        200,
        "{\"ok\":true,\"client_id\":\"x\",\"assigned\":null,\"seq\":0,\"error\":null}",
    );
    page.click("[data-plan-ref=\"task:t-a\"] .reviewed-toggle input");
    page.wait_until(
        "(window.__releasers || []).length >= 2",
        "the overtaking resync in flight",
    );
    release(&mut page);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    loop {
        release(&mut page);
        let d = debug(&mut page);
        if d["connected"] == true && d["syncing"] == false {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the page did not get through: {d}"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert_eq!(
        page.eval("window.artefactoPlan.debug().gone"),
        false,
        "HTTP answered; not gone"
    );
    assert_eq!(
        page.eval("!!document.querySelector('.pv-notice[data-kind=\"gone\"]')"),
        false
    );
}

#[test]
fn a_phases_error_line_does_not_toggle_the_phase() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("kitchen-sink.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    answer_with(&mut page, "/cmd", 500, 1_000);
    page.click("[data-plan-ref=\"phase:p-core\"] summary .reviewed-toggle input");
    page.wait_until(
        "document.querySelector('[data-plan-ref=\"phase:p-core\"] summary .reviewed-toggle + .pv-error')",
        "the error line in the phase's summary",
    );
    let open = page.eval("document.querySelector('[data-plan-ref=\"phase:p-core\"]').open");
    page.click("[data-plan-ref=\"phase:p-core\"] summary .reviewed-toggle + .pv-error");
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert_eq!(
        page.eval("document.querySelector('[data-plan-ref=\"phase:p-core\"]').open"),
        open,
        "reading the error is not a click on the phase"
    );
    let _ = s;
}

#[test]
fn buffered_replies_drain_in_log_order() {
    // Set-valued writes settle by seq whatever the order; appended
    // messages would show in arrival order without the sort.
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    comment(&mut page, "task:t-a", "the comment");
    catch_up_held(&s, &mut page);
    shape_fetch(&mut page, "/cmd", "hold-first-release", 0);
    // A reply (its reply held) and then a question (its reply immediate):
    // both buffered, the later one first.
    page.click(".thread[data-thread=\"c-1\"] .thread-reply");
    page.type_into(
        ".thread[data-thread=\"c-1\"] .composer[data-kind=\"reply\"] textarea",
        "one",
    );
    page.click(".thread[data-thread=\"c-1\"] .composer[data-kind=\"reply\"] .composer-send");
    support::wait_for(
        || s.server().count_events("thread.replied") == 1,
        "the reply landed",
    );
    page.click(".thread[data-thread=\"c-1\"] .thread-ask");
    page.type_into(".pv-panel-composer textarea", "two");
    enter(&mut page, ".pv-panel-composer textarea");
    support::wait_for(
        || s.server().count_events("chat.sent") == 1,
        "the question landed",
    );
    page.wait_until(
        "document.querySelector('.pv-panel-composer textarea').value === ''",
        "the question's reply to be in",
    );
    release_path(&mut page, "/cmd");
    page.wait_until(
        "!document.querySelector('.composer[data-kind=\"reply\"]')",
        "the reply's reply to be in",
    );
    assert_eq!(
        page.eval("window.artefactoPlan.debug().syncing"),
        true,
        "both buffered, nothing drained yet"
    );
    release_path(&mut page, "/state");
    connected(&mut page);
    let messages = debug(&mut page)["threads"][0]["messages"].clone();
    assert_eq!(
        messages,
        serde_json::json!(["reviewer: the comment", "reviewer: one", "reviewer: two"]),
        "log order, not arrival order"
    );
}

// --- the round-ten fix slice, reviewed fresh ----------------------------------

#[test]
fn a_push_after_a_sent_chat_message_leaves_the_panel_with_a_composer() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    page.click(".feedback-bar-chat");
    page.type_into(".pv-panel-composer textarea", "first");
    page.click(".pv-panel-composer .thread-composer-send");
    page.wait_until(
        "document.querySelectorAll('.pv-chat-msg').length === 1",
        "the message shown",
    );
    assert_eq!(
        page.eval("Object.keys(window.artefactoPlan.debug().drafts).length"),
        0
    );

    s.edit_plan("Demo plan", "Demo plan, revised");
    s.push(1, &[]);
    page.wait_until(
        "document.body.dataset.artefactoRevision === '2'",
        "revision 2",
    );
    assert_eq!(
        page.eval("document.querySelector('.pv-dock').classList.contains('is-hidden')"),
        false,
        "still open"
    );
    assert_eq!(
        page.eval("!!document.querySelector('.pv-panel-composer textarea')"),
        true,
        "and still somewhere to write"
    );
    assert_eq!(
        page.eval("document.querySelectorAll('.pv-panel-composer').length"),
        1
    );
}

#[test]
fn a_probe_never_runs_on_a_live_socket() {
    // A socket-only outage, a push while it is down, and a retried write's
    // reply during the probe: the superseding resync swaps the body, whose
    // mount reconnects. Nothing may then probe on that live socket and read
    // a failed snapshot as the server being gone.
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    page.eval("window.artefactoPlan.settings.backoffMs = [30, 30, 300]; window.artefactoPlan.settings.stableAfterMs = 100000");
    failing_sockets(&mut page, 3);
    shape_fetch(&mut page, "/state", "hold-release", 0);
    artefacto::server::socket::close_all(&s.server().shared);
    page.wait_until(
        "(window.__releasers || []).length > 0",
        "the probe's snapshot request in flight",
    );
    s.edit_plan("Demo plan", "Demo plan, revised");
    s.push(1, &[]);
    answer_json(
        &mut page,
        "/cmd",
        200,
        "{\"ok\":true,\"client_id\":\"x\",\"assigned\":null,\"seq\":0,\"error\":null}",
    );
    page.click("[data-plan-ref=\"task:t-a\"] .reviewed-toggle input");
    page.wait_until(
        "(window.__releasers || []).length >= 2",
        "the overtaking resync in flight",
    );
    release(&mut page);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    loop {
        release(&mut page);
        let d = debug(&mut page);
        if d["connected"] == true && d["syncing"] == false && d["revision"] == 2 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the page did not get through: {d}"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    // Every later snapshot fails. Connected, nothing should ask for one.
    answer_with(&mut page, "/state", 500, 1_000);
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let d = debug(&mut page);
    assert_eq!(d["connected"], true);
    assert_eq!(d["gone"], false, "no probe ran on the live socket: {d}");
    assert_eq!(
        page.eval("!!document.querySelector('.pv-notice[data-kind=\"gone\"]')"),
        false
    );
    assert_eq!(page.eval("window.__calls"), 0, "no snapshot was requested");
}

#[test]
fn a_lost_page_is_not_left_catching_up() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    page.eval("window.artefactoPlan.settings.backoffMs = [30, 30, 30]");
    failing_sockets(&mut page, 1_000_000);
    // The probe's snapshot is a 404, and a slow one.
    answer_with(&mut page, "/state", 404, 1_000);
    shape_fetch(&mut page, "/state", "hold-release", 0);
    artefacto::server::socket::close_all(&s.server().shared);
    page.wait_until(
        "(window.__releasers || []).length > 0",
        "the probe's request in flight",
    );
    page.click("[data-plan-ref=\"task:t-a\"] .reviewed-toggle input");
    page.wait_until(
        "window.artefactoPlan.debug().buffered === 1",
        "the reply buffered while catching up",
    );
    release(&mut page);
    page.wait_until(
        "window.artefactoPlan.debug().lost",
        "the page to learn it is lost",
    );
    let d = debug(&mut page);
    assert_eq!(d["syncing"], false, "{d}");
    assert_eq!(d["buffered"], 0, "{d}");
    assert_eq!(d["pending"], 0, "{d}");
    assert_eq!(
        page.eval(
            "document.querySelector('[data-plan-ref=\"task:t-a\"] .reviewed-toggle input').checked"
        ),
        true,
        "the write the server accepted stays on the page"
    );
    assert_eq!(d["reviewed"][0], "task:t-a");
}

// --- the round-eleven fix slice, reviewed fresh --------------------------------

#[test]
fn focus_in_the_panel_composer_survives_a_push() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    page.click(".feedback-bar-chat");
    page.type_into(".pv-panel-composer textarea", "first");
    page.click(".pv-panel-composer .thread-composer-send");
    page.wait_until(
        "document.querySelectorAll('.pv-chat-msg').length === 1",
        "the message to land",
    );
    // Nothing typed since: the cursor sits in the empty composer.
    page.eval("(function(){ document.querySelector('.pv-panel-composer textarea').focus(); return true; })()");
    s.edit_plan("Demo plan", "Demo plan, revised");
    s.push(1, &[]);
    page.wait_until(
        "document.body.dataset.artefactoRevision === '2'",
        "revision 2",
    );
    assert_eq!(
        page.eval("!document.querySelector('.pv-dock').classList.contains('is-hidden')"),
        true,
        "the panel is still open"
    );
    assert_eq!(
        page.eval(
            "document.activeElement === document.querySelector('.pv-panel-composer textarea')"
        ),
        true,
        "and the cursor is back in its composer"
    );
    assert_eq!(
        page.eval("document.querySelectorAll('.pv-chat-msg').length"),
        1,
        "the log kept its message"
    );
}

#[test]
fn retry_clears_the_notice_and_the_pill() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    page.eval("window.artefactoPlan.settings.backoffMs = [30, 30, 30]");
    failing_sockets(&mut page, 1_000_000);
    artefacto::server::socket::close_all(&s.server().shared);
    page.wait_until("window.artefactoPlan.debug().gone", "the page to give up");
    page.click(".pv-notice[data-kind=\"gone\"] .pv-notice-action");
    assert_eq!(
        page.eval("!!document.querySelector('.pv-notice[data-kind=\"gone\"]')"),
        false
    );
    assert_ne!(
        page.text("document.querySelector('.pv-presence').textContent"),
        "server gone",
        "the pill follows the retry at once"
    );
    assert_eq!(page.eval("window.artefactoPlan.debug().gone"), false);
}

#[test]
fn hiding_the_panel_keeps_what_was_typed() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    page.click(".feedback-bar-chat");
    page.type_into(".pv-panel-composer textarea", "never mind");
    page.click(".pv-panel-hide");
    assert_eq!(
        page.eval("document.querySelector('.pv-dock').classList.contains('is-hidden')"),
        true,
        "the X hides the panel"
    );
    page.eval(
        "window.artefactoPlan.injectFrame({ format: 'artefacto.frame/1', seq: 999, events: [] })",
    );
    assert_eq!(
        page.eval("document.querySelector('.pv-dock').classList.contains('is-hidden')"),
        true,
        "and it stays hidden"
    );
    page.navigate(&s.page_url());
    connected(&mut page);
    page.click(".feedback-bar-chat");
    assert_eq!(
        page.text("document.querySelector('.pv-panel-composer textarea').value"),
        "never mind",
        "what was typed is still there after a reload"
    );
    let _ = s;
}

// --- what real use found ------------------------------------------------------

#[test]
fn a_phases_comment_sits_under_its_header_not_after_its_last_task() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("kitchen-sink.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    page.eval("(function(){ document.querySelectorAll('details.phase').forEach(function(d){ d.open = true; }); return true; })()");
    comment(&mut page, "phase:p-core", "on the phase itself");
    let thread_top = page
        .eval("document.querySelector('[data-plan-ref=\"phase:p-core\"] .thread[data-thread=\"c-1\"]').getBoundingClientRect().top")
        .as_f64()
        .unwrap();
    let first_task_top = page
        .eval("document.querySelector('[data-plan-ref=\"phase:p-core\"] .task').getBoundingClientRect().top")
        .as_f64()
        .unwrap();
    assert!(
        thread_top < first_task_top,
        "the phase's comment ({thread_top}) is above its first task ({first_task_top})"
    );
    assert_eq!(
        page.eval("!!document.querySelector('[data-plan-ref=\"phase:p-core\"] .task .thread')"),
        false,
        "and not inside any task card"
    );
}

#[test]
fn a_sent_review_is_unmistakable() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    comment(&mut page, "task:t-a", "fine");
    page.click(".feedback-bar-send");
    page.wait_until(
        "document.querySelector('.pv-notice[data-kind=\"sent\"]')",
        "the sent notice",
    );
    let text = page.text(
        "document.querySelector('.pv-notice[data-kind=\"sent\"] .pv-notice-text').textContent",
    );
    assert!(
        text.starts_with("Review sent for revision 1: 1 comment"),
        "{text}"
    );
    assert!(text.contains("The agent has it"), "{text}");
    page.wait_until(
        "document.querySelector('.feedback-bar-send').classList.contains('is-filled')",
        "the button to say a second click is a second send",
    );
    assert!(page
        .text("document.querySelector('.feedback-bar-sent').textContent")
        .starts_with("review sent · rev 1 · "));
    assert_eq!(s.server().count_events("review.submitted"), 1);

    // A new revision reopens the review, and the notice goes with it.
    s.edit_plan("Demo plan", "Demo plan, revised");
    s.push(1, &[]);
    page.wait_until(
        "document.body.dataset.artefactoRevision === '2'",
        "revision 2",
    );
    assert_eq!(
        page.eval("!!document.querySelector('.pv-notice[data-kind=\"sent\"]')"),
        false
    );
    assert_eq!(
        page.eval("document.querySelector('.feedback-bar-send').classList.contains('is-filled') || document.querySelector('.pv-panel-foot').classList.contains('is-sent')"),
        false,
        "a new revision reopens the review: no verdict is filled"
    );
}

#[test]
fn an_answer_can_be_edited_and_removed() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("kitchen-sink.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    page.click("[data-plan-ref=\"question:q-ttl\"] .comment-btn");
    page.type_into(
        "[data-plan-ref=\"question:q-ttl\"] .composer textarea",
        "an hour",
    );
    page.click("[data-plan-ref=\"question:q-ttl\"] .composer .composer-send");
    page.wait_until(
        "!document.querySelector('[data-plan-ref=\"question:q-ttl\"] .composer')",
        "answered",
    );

    page.click(".pv-answer[data-answer-for=\"q-ttl\"] .pv-answer-edit");
    assert_eq!(
        page.text(
            "document.querySelector('[data-plan-ref=\"question:q-ttl\"] .composer textarea').value"
        ),
        "an hour",
        "Edit opens the composer with the current answer"
    );
    page.type_into(
        "[data-plan-ref=\"question:q-ttl\"] .composer textarea",
        "two hours",
    );
    page.click("[data-plan-ref=\"question:q-ttl\"] .composer .composer-send");
    page.wait_until(
        "document.querySelector('.pv-answer[data-answer-for=\"q-ttl\"] .pv-answer-text').textContent === 'two hours'",
        "the edited answer",
    );

    page.click(".pv-answer[data-answer-for=\"q-ttl\"] .pv-answer-remove");
    page.wait_until(
        "document.querySelector('.pv-answer[data-answer-for=\"q-ttl\"]').hidden",
        "the answer to be removed",
    );
    assert_eq!(
        s.server().last_event_of_type("question.answered")["data"]["text"],
        ""
    );

    // And a removed answer is not in the review.
    page.click(".feedback-bar-send");
    page.wait_until(
        "document.querySelector('.pv-notice[data-kind=\"sent\"]')",
        "sent",
    );
    let doc = s.server().last_event_of_type("review.submitted")["data"]["feedback"].clone();
    assert_eq!(doc["answers"].as_array().unwrap().len(), 0, "{doc}");
}

// ---------------------------------------------------------------------------
// The artifact index at `/` (spec 4.4), in a real browser.
// ---------------------------------------------------------------------------

/// Where this suite leaves its screenshots, for a person to look at.
fn screenshot_path(name: &str) -> std::path::PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/screenshots");
    std::fs::create_dir_all(&dir).expect("screenshots dir");
    dir.join(format!("{name}.png"))
}

#[test]
fn the_index_page_lists_artifacts_and_removes_a_row_in_a_real_browser() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let repo = Repo::new();
    let server = InProcess::start_in(&repo);
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plan");

    // A static render under its own id, then two pushed plans, so the index
    // has a row that is not live and `open` lands on the index.
    let static_plan = repo.path().join("static.json");
    let text = std::fs::read_to_string(fixtures.join("minimal.json")).unwrap();
    std::fs::write(
        &static_plan,
        text.replace("\"id\": \"demo\"", "\"id\": \"static-demo\"")
            .replace("Demo plan", "A static render"),
    )
    .unwrap();
    let out = repo.run(&[
        "plan",
        "render",
        static_plan.to_str().unwrap(),
        "--out",
        "static.html",
        "--no-open",
    ]);
    assert_eq!(out.code, 0, "{}", out.stderr);
    for (fixture, name) in [("kitchen-sink.json", "a.json"), ("minimal.json", "b.json")] {
        let plan = repo.path().join(name);
        std::fs::copy(fixtures.join(fixture), &plan).unwrap();
        let out = repo.run(&[
            "plan",
            "push",
            plan.to_str().unwrap(),
            "--json",
            "--no-open",
        ]);
        assert_eq!(out.code, 0, "{}", out.stderr);
    }
    let opened = repo.json(&["open", "--json"]);
    assert_eq!(opened["index"], true, "{opened}");

    let mut page = browser.new_page();
    page.navigate(opened["url"].as_str().unwrap());
    assert_eq!(
        page.text("location.pathname"),
        "/",
        "the link lands on the index"
    );
    assert_eq!(page.eval("document.querySelectorAll('.ix-row').length"), 3);
    assert_eq!(
        page.eval("document.querySelectorAll('.ix-row.is-live').length"),
        2,
        "the two pushed plans are live"
    );
    assert_eq!(
        page.eval("document.querySelectorAll('.ix-remove').length"),
        1,
        "only the static render can be removed"
    );
    assert_eq!(
        page.eval("document.querySelectorAll('.ix-poster svg').length"),
        3,
        "a poster per row"
    );
    assert_eq!(
        page.text("document.querySelector('.ix-count').textContent"),
        "3 artifacts"
    );
    assert!(page.errors().is_empty(), "{:?}", page.errors());
    page.screenshot(&screenshot_path("index"));

    // Remove the static row from the page.
    page.click(".ix-remove");
    page.wait_until(
        "document.querySelectorAll('.ix-row').length === 2",
        "the removed row is gone from the page",
    );
    assert_eq!(
        page.text("document.querySelector('.ix-count').textContent"),
        "2 artifacts"
    );
    let listed = repo.json(&["list", "--json"]);
    assert_eq!(
        listed["artifacts"].as_array().unwrap().len(),
        2,
        "and from the registry: {listed}"
    );
    assert!(static_plan.exists(), "the user's file is untouched");
    page.screenshot(&screenshot_path("index-after-remove"));

    // A live row leads to its page, and the page leads back.
    // Each navigation waits for the element the next step needs, not for
    // the path alone: the path changes when the navigation commits, before
    // the new document has finished parsing.
    page.click(".ix-row.is-live .ix-title a");
    page.wait_until(
        "location.pathname.startsWith('/a/plan:') && !!document.querySelector('.pv-topbar-link')",
        "the plan page, with its link to the index",
    );
    page.screenshot(&screenshot_path("plan-page-with-index-link"));
    page.click(".pv-topbar-link");
    page.wait_until(
        "location.pathname === '/' && document.querySelectorAll('.ix-row').length === 2",
        "back at the index, with its two rows",
    );
    assert!(page.errors().is_empty(), "{:?}", page.errors());
    drop(server);
}

#[test]
fn asking_on_an_element_with_no_comment_opens_a_question_thread() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);

    // Every element that offers Comment offers the mark beside it, labelled
    // until the first question on this browser.
    assert_eq!(
        page.eval(
            "document.querySelectorAll('.el-actions .ask-btn').length > 0 && \
             document.querySelectorAll('.el-actions .ask-btn').length === document.querySelectorAll('.comment-btn').length"
        ),
        true,
        "one mark per comment button"
    );
    assert_eq!(
        page.eval("document.querySelectorAll('[data-plan-ref] .ask-btn.is-labelled').length > 0"),
        true,
        "the ask control is labelled before the first ask"
    );

    // The mark opens the panel aimed at the task; Enter opens the thread.
    page.click("[data-plan-ref=\"task:t-a\"] .ask-btn");
    page.wait_until(
        "!document.querySelector('.pv-dock').classList.contains('is-hidden')",
        "the panel to open",
    );
    assert_eq!(
        page.text("document.querySelector('.pv-panel-target .pv-ctx').title"),
        "task:t-a",
        "the composer is aimed at the task"
    );
    assert_eq!(
        page.eval(
            "document.activeElement === document.querySelector('.pv-panel-composer textarea')"
        ),
        true,
        "and focused"
    );
    page.type_into(".pv-panel-composer textarea", "is this the whole plan?");
    enter(&mut page, ".pv-panel-composer textarea");
    page.wait_until(
        "document.querySelectorAll('.pv-panel-msg[data-thread=\"c-1\"]:not(.thread-working)').length === 1",
        "the question to open its thread in the log",
    );
    assert_eq!(
        page.eval("document.querySelector('.pv-panel-target').hidden"),
        true,
        "the target clears after the send"
    );
    assert_eq!(
        page.text("document.querySelector('.pv-panel-composer textarea').value"),
        "",
        "and so does the text"
    );
    assert_eq!(
        page.text("document.querySelector('.pv-panel-msg[data-thread=\"c-1\"] .pv-ctx').title"),
        "task:t-a",
        "the message carries its context chip"
    );
    assert_eq!(
        page.eval("!!document.querySelector('.thread[data-thread=\"c-1\"]')"),
        false,
        "a question thread is not rendered on the element"
    );

    // From the question until the answer: the working row in the log, the
    // pill at work, the mark tinted and counted, the element spined, and the
    // labels gone from the element rows.
    assert_eq!(
        page.text("document.querySelector('.pv-panel-msg.thread-working[data-thread=\"c-1\"] .thread-text').textContent"),
        "thinking\u{2026}"
    );
    assert_eq!(
        page.text("document.querySelector('.pv-presence').dataset.mode"),
        "working"
    );
    assert_eq!(
        page.eval("document.querySelector('[data-plan-ref=\"task:t-a\"] .ask-btn').classList.contains('has-thread')"),
        true
    );
    assert_eq!(page.text("document.querySelector('[data-plan-ref=\"task:t-a\"] .ask-btn .ask-count').textContent"), "1");
    assert_eq!(page.eval("document.querySelector('[data-plan-ref=\"task:t-a\"]').classList.contains('is-discussed')"), true);
    assert_eq!(
        page.eval("document.querySelectorAll('[data-plan-ref] .ask-btn.is-labelled').length"),
        0,
        "after the first ask the mark alone is the control"
    );

    let out = s
        .repo
        .run(&["await", "--timeout", "5s", "--session", &s.session]);
    let r: serde_json::Value = serde_json::from_str(out.success().stdout.trim()).unwrap();
    assert_eq!(r["status"], "chat");
    let seq = r["seq"].to_string();
    let last = r["events"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["data"]["thread"], "c-1");
    assert_eq!(last["data"]["ref"], "task:t-a");
    assert_eq!(last["data"]["text"], "is this the whole plan?");

    s.repo
        .run(&[
            "reply",
            "--session",
            &s.session,
            "--thread",
            "c-1",
            "yes, all of it",
        ])
        .success();
    page.wait_until(
        "document.querySelectorAll('.pv-panel-msg[data-thread=\"c-1\"]:not(.thread-working)').length === 2",
        "the answer to join the log",
    );
    assert_eq!(
        page.eval("!document.querySelector('.pv-panel-msg.thread-working')"),
        true,
        "the answer ends the working row"
    );
    assert_eq!(
        page.text("document.querySelector('.pv-panel-msg[data-thread=\"c-1\"]').dataset.actor"),
        "reviewer",
        "the question comes before its answer, even within the same second"
    );
    assert_eq!(
        page.text("document.querySelector('.pv-presence').dataset.mode"),
        "waiting"
    );
    assert_eq!(
        page.text(
            "document.querySelector('[data-plan-ref=\"task:t-a\"] .ask-preview-text').textContent"
        ),
        "yes, all of it",
        "the element previews the last message"
    );
    assert_eq!(page.text("document.querySelector('[data-plan-ref=\"task:t-a\"] .ask-btn .ask-count').textContent"), "2");
    assert_eq!(
        page.eval("!!document.querySelector('.pv-panel-msg[data-thread=\"c-1\"][data-actor=\"agent\"] .pv-avatar .ag-mark') && !!document.querySelector('.pv-panel-msg[data-thread=\"c-1\"][data-actor=\"reviewer\"] .pv-avatar.is-you')"),
        true,
        "circle for the agent, square for the reviewer"
    );
    assert_eq!(
        page.eval("Array.from(document.querySelectorAll('.pv-panel-log *')).some(function (n) { return n.children.length === 0 && /c-[0-9]+/.test(n.textContent); })"),
        false,
        "no thread id is shown in the log"
    );
    page.screenshot(&screenshot_path("ask-on-a-task"));

    // The chip is a link into the page.
    page.click(".pv-panel-msg[data-thread=\"c-1\"] .pv-ctx");
    assert_eq!(page.eval("document.querySelector('[data-plan-ref=\"task:t-a\"]').classList.contains('is-jumped')"), true, "the chip jumps to its element");

    // A reload shows the same log from the server's state, and the mark
    // aims a follow-up at the same thread.
    page.navigate(&s.page_url());
    connected(&mut page);
    page.click(".pv-panel-link");
    assert_eq!(
        page.eval("document.querySelectorAll('.pv-panel-msg[data-thread=\"c-1\"]').length"),
        2
    );
    page.click("[data-plan-ref=\"task:t-a\"] .ask-btn");
    assert_eq!(
        page.eval("!document.querySelector('.pv-panel-target').hidden"),
        true,
        "aimed at the existing thread"
    );
    page.type_into(".pv-panel-composer textarea", "and the CLI layer?");
    enter(&mut page, ".pv-panel-composer textarea");
    page.wait_until(
        "document.querySelectorAll('.pv-panel-msg[data-thread=\"c-1\"]:not(.thread-working)').length === 3",
        "the follow-up to join the thread",
    );
    let out = s.repo.run(&[
        "await",
        "--timeout",
        "5s",
        "--session",
        &s.session,
        "--ack",
        &seq,
    ]);
    let r: serde_json::Value = serde_json::from_str(out.success().stdout.trim()).unwrap();
    assert_eq!(r["status"], "chat", "the follow-up woke the agent: {r}");
    let last = r["events"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(
        last["data"]["thread"], "c-1",
        "a second question on the element goes to the same thread"
    );
    assert_eq!(last["data"]["text"], "and the CLI layer?");
}

#[test]
fn the_hint_line_shows_until_dismissed_and_stores_nothing_before() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);

    page.click(".pv-panel-handle");
    let hint = page.text("document.querySelector('.feedback-bar-hint-text').textContent");
    assert!(
        hint.contains("Comments wait") && hint.contains("reaches the agent now"),
        "the hint names both behaviours: {hint}"
    );
    let banner = page.text("document.querySelector('.pv-banner-text').textContent");
    assert!(
        banner.contains("reaches it now") && !banner.contains("hears you"),
        "the banner says which of the two reaches the agent now: {banner}"
    );
    assert_eq!(
        page.eval("window.localStorage.getItem('artefacto.hint.ask')"),
        serde_json::Value::Null,
        "nothing is stored for the hint until the reviewer dismisses it"
    );
    page.click(".feedback-bar-hint-dismiss");
    assert_eq!(
        page.eval("!document.querySelector('.feedback-bar-hint')"),
        true,
        "dismissed"
    );

    page.navigate(&s.page_url());
    connected(&mut page);
    page.click(".pv-panel-handle");
    assert_eq!(
        page.eval("!document.querySelector('.feedback-bar-hint')"),
        true,
        "dismissed stays dismissed across a reload"
    );
    assert_eq!(
        page.eval("!!document.querySelector('.feedback-bar-chat')"),
        true,
        "the rest of the bar is untouched"
    );
}

#[test]
fn the_ask_button_shares_a_row_with_comment_on_every_kind_of_element() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("kitchen-sink.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    // The first control under the phases heading is "expand all".
    page.click("#phases-actions .pv-btn");

    // Wherever a comment button is, the ask button is beside it, on the same
    // line, whether the host is a heading row or a prose column.
    assert_eq!(
        page.eval(
            "document.querySelectorAll('.el-actions').length > 0 && \
             document.querySelectorAll('.el-actions').length === document.querySelectorAll('.comment-btn').length"
        ),
        true,
        "every comment button is in an actions row with its ask button"
    );
    let misaligned = page.eval(
        "Array.from(document.querySelectorAll('.el-actions')).filter(function (row) { \
           const c = row.querySelector('.comment-btn'), a = row.querySelector('.ask-btn'); \
           if (!c || !a) return true; \
           const cb = c.getBoundingClientRect(), ab = a.getBoundingClientRect(); \
           const mid = function (r) { return (r.top + r.bottom) / 2; }; \
           return Math.abs(mid(cb) - mid(ab)) > 2 || ab.left <= cb.right; \
         }).map(function (row) { return row.closest('[data-plan-ref]').getAttribute('data-plan-ref'); })",
    );
    assert_eq!(
        misaligned,
        serde_json::json!([]),
        "every ask button sits right of its comment button on one line"
    );

    ask(
        &mut page,
        "task:t-session-store",
        "why a trait rather than a plain struct here?",
        "c-1",
    );
    s.repo
        .run(&[
            "reply",
            "--session",
            &s.session,
            "--thread",
            "c-1",
            "So Redis can slot in without touching the call sites.",
        ])
        .success();
    page.wait_until(
        "document.querySelectorAll('.pv-panel-msg[data-thread=\"c-1\"]:not(.thread-working)').length === 2",
        "the answer to join the log",
    );
    // Seed one thread of every other kind and status, so the screenshots
    // show the card in each of its states: a blocking comment left open, a
    // comment resolved as changed, one declined.
    page.click("[data-plan-ref=\"task:t-redis\"] .comment-btn");
    page.type_into(
        "[data-plan-ref=\"task:t-redis\"] .pv-composers .composer textarea",
        "This needs a rollback step before it ships.",
    );
    page.click(
        "[data-plan-ref=\"task:t-redis\"] .pv-composers .composer .comment-box-blocking input",
    );
    page.click("[data-plan-ref=\"task:t-redis\"] .pv-composers .composer .composer-send");
    page.wait_until(
        "!!document.querySelector('.thread[data-thread=\"c-2\"].is-blocking')",
        "the blocking thread",
    );
    comment(
        &mut page,
        "task:t-cleanup",
        "Fold this into the trait task.",
    );
    comment(
        &mut page,
        "task:t-bench",
        "Do we still need benchmarks at all?",
    );
    for (thread, verdict, note) in [
        (
            "c-3",
            "--changed",
            "Folded into t-session-store; this task is gone in the next revision.",
        ),
        (
            "c-4",
            "--declined",
            "Kept: the benchmarks are what tell us the trait costs nothing.",
        ),
    ] {
        s.repo
            .run(&[
                "resolve",
                thread,
                "--session",
                &s.session,
                verdict,
                "--note",
                note,
            ])
            .success();
    }
    page.wait_until(
        "document.querySelector('.thread[data-thread=\"c-4\"]').dataset.status === 'declined' && document.querySelector('.thread[data-thread=\"c-3\"]').dataset.status === 'changed'",
        "the resolutions to land",
    );
    // With threads, composers, and the bar all on the page and no verdict
    // sent, nothing is a filled control.
    assert_eq!(
        page.eval("document.querySelectorAll('.pv-btn.is-filled').length"),
        0,
        "only the chosen verdict is ever filled"
    );
    // The panel open beside the sheet, scrolled to the blocking comment.
    page.click(".pv-panel-link");
    // The panel open beside the sheet, scrolled to the blocking comment.
    page.click(".pv-panel-link");
    page.eval("document.querySelector('.thread[data-thread=\"c-2\"]').scrollIntoView({ block: 'center' })");
    page.screenshot(&screenshot_path("ask-on-kitchen-sink"));
    // The same page in the dark theme, from the toggle, so both palettes are
    // looked at whenever the page changes.
    page.click("[data-theme-set=\"dark\"]");
    page.wait_until(
        "document.documentElement.getAttribute('data-theme') === 'dark' && !document.documentElement.classList.contains('theme-anim')",
        "the dark theme to apply and its crossfade to end",
    );
    page.screenshot(&screenshot_path("ask-on-kitchen-sink-dark"));
    // And the third theme, which also has to survive a reload.
    page.click("[data-theme-set=\"vibe\"]");
    page.wait_until(
        "document.documentElement.getAttribute('data-theme') === 'vibe' && !document.documentElement.classList.contains('theme-anim')",
        "the vibe theme to apply",
    );
    page.screenshot(&screenshot_path("ask-on-kitchen-sink-vibe"));
    page.navigate(&s.page_url());
    connected(&mut page);
    assert_eq!(
        page.text("document.documentElement.getAttribute('data-theme')"),
        "vibe",
        "the choice survives a reload"
    );
    page.click("[data-theme-set=\"\"]");
}

#[test]
fn the_panel_says_when_no_agent_will_hear_it() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);

    ask(&mut page, "phase:p-one", "how long?", "c-1");
    assert_eq!(
        page.eval("!!document.querySelector('.pv-panel-msg.thread-working[data-thread=\"c-1\"]')"),
        true,
        "the question waits"
    );
    assert_eq!(
        page.text("document.querySelector('.pv-panel-composer .thread-composer-hint').textContent"),
        "Enter to send"
    );
    assert_eq!(
        page.text("document.querySelector('.pv-panel-head .pv-chat-hint').textContent"),
        "The agent hears this at once."
    );
    assert_eq!(
        page.eval("!!document.querySelector('.pv-notice[data-kind=\"noagent\"]')"),
        false,
        "an agent holds the lease"
    );

    // The lease expires while the question waits: the composer, the head,
    // the working row, and a notice all say so.
    s.server()
        .age_lease(artefacto::server::lease::TTL + std::time::Duration::from_secs(1));
    page.wait_until(
        "document.querySelector('.pv-panel-composer .thread-composer-hint').textContent === 'waits for an agent'",
        "the composer to say the question will wait",
    );
    assert_eq!(
        page.text("document.querySelector('.pv-panel-head .pv-chat-hint').textContent"),
        "No agent is attached. Your message will wait for one."
    );
    assert_eq!(
        page.text("document.querySelector('.pv-panel-msg.thread-working[data-thread=\"c-1\"] .thread-text').textContent"),
        "waiting for an agent\u{2026}"
    );
    assert_eq!(
        page.text("document.querySelector('.pv-notice[data-kind=\"noagent\"] .pv-notice-text').textContent"),
        "No agent is attached. Your question waits for one."
    );

    // An agent arriving restores the promise everywhere.
    s.repo
        .run(&[
            "await",
            "--timeout",
            "1s",
            "--agent",
            "claude",
            "--takeover",
        ])
        .success();
    page.wait_until(
        "document.querySelector('.pv-panel-composer .thread-composer-hint').textContent === 'Enter to send'",
        "the composer to say the agent hears it",
    );
    assert_eq!(
        page.eval("!!document.querySelector('.pv-notice[data-kind=\"noagent\"]')"),
        false,
        "the notice goes with the agent's arrival"
    );
    assert_eq!(
        page.text("document.querySelector('.pv-panel-msg.thread-working[data-thread=\"c-1\"] .thread-text').textContent"),
        "thinking\u{2026}"
    );
}

#[test]
fn two_verdicts_one_filled_and_leaving_is_not_losing() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    let banner = page.text("document.querySelector('.pv-banner-steps').textContent");
    assert!(banner.contains("come back later"), "{banner}");

    // While a write is on its way the line says Saving; once answered, Saved.
    shape_fetch(&mut page, "/cmd", "hold-response", 900);
    page.click("[data-plan-ref=\"task:t-a\"] .comment-btn");
    page.type_into(
        "[data-plan-ref=\"task:t-a\"] .pv-composers .composer textarea",
        "needs a rollback step",
    );
    page.click("[data-plan-ref=\"task:t-a\"] .pv-composers .composer .composer-send");
    assert_eq!(
        page.text("document.querySelector('.feedback-bar-state-text').textContent"),
        "Saving\u{2026}"
    );
    assert_eq!(
        page.eval("document.querySelector('.feedback-bar-state').classList.contains('is-saving')"),
        true
    );
    page.wait_until(
        "!document.querySelector('[data-plan-ref=\"task:t-a\"] .pv-composers .composer')",
        "the comment to be accepted",
    );
    assert_eq!(
        page.text("document.querySelector('.feedback-bar-state-text').textContent"),
        "Saved \u{b7} rev 1"
    );
    // Leaving with unsent work: the line says nothing is lost, for a moment.
    page.eval("window.dispatchEvent(new Event('pagehide'))");
    assert_eq!(
        page.text("document.querySelector('.feedback-bar-state-text').textContent"),
        "Saved. The agent sees your notes when you send them."
    );

    // Request changes is one verdict, Approve the other; the sent one fills.
    page.click(".feedback-bar-send");
    page.wait_until(
        "document.querySelector('.pv-panel-foot').classList.contains('is-sent')",
        "the bar to show the review as sent",
    );
    assert_eq!(
        s.server().last_event_of_type("review.submitted")["data"]["verdict"],
        "request_changes"
    );
    assert_eq!(
        page.eval("document.querySelector('.feedback-bar-send').classList.contains('is-filled') && !document.querySelector('.feedback-bar-approve').classList.contains('is-filled')"),
        true,
        "Request changes is the filled control"
    );
    assert_eq!(
        page.eval("document.querySelectorAll('.pv-btn.is-filled').length"),
        1,
        "and the only one"
    );
    assert_eq!(
        page.eval("!!document.querySelector('.feedback-bar-approve input')"),
        false,
        "no Approve checkbox"
    );

    // A new revision reopens the review; Approve sends the other verdict.
    s.edit_plan("Demo plan", "Demo plan, revised");
    s.push(1, &[]);
    page.wait_until(
        "document.body.dataset.artefactoRevision === '2' && !document.querySelector('.pv-panel-foot').classList.contains('is-sent')",
        "revision 2 to reopen the review",
    );
    page.click(".feedback-bar-approve");
    page.wait_until(
        "document.querySelector('.feedback-bar-approve').classList.contains('is-filled')",
        "Approve to be the filled control",
    );
    assert_eq!(
        s.server().last_event_of_type("review.submitted")["data"]["verdict"],
        "approve"
    );
    assert_eq!(
        page.eval("document.querySelectorAll('.pv-btn.is-filled').length === 1 && !document.querySelector('.feedback-bar-send').classList.contains('is-filled')"),
        true,
        "the other verdict is not filled"
    );
    assert!(page
        .text(
            "document.querySelector('.pv-notice[data-kind=\"sent\"] .pv-notice-text').textContent"
        )
        .starts_with("Approval sent for revision 2"),);

    // The verdict survives a reload: it comes from the server's snapshot.
    page.navigate(&s.page_url());
    connected(&mut page);
    assert_eq!(
        page.eval(
            "document.querySelector('.feedback-bar-approve').classList.contains('is-filled')"
        ),
        true,
        "the snapshot carries the verdict"
    );
}

/// Ask a question about `target` from the panel and wait for it in the log
/// as thread `c-N`. The element's mark aims the panel; Enter sends.
fn ask(page: &mut support::browser::Page, target: &str, text: &str, thread: &str) {
    page.click(&format!("[data-plan-ref=\"{target}\"] .ask-btn"));
    page.wait_until(
        "!document.querySelector('.pv-dock').classList.contains('is-hidden') && !document.querySelector('.pv-panel-target').hidden",
        "the panel to open, aimed at the element",
    );
    page.type_into(".pv-panel-composer textarea", text);
    enter(page, ".pv-panel-composer textarea");
    page.wait_until(
        &format!("document.querySelectorAll('.pv-panel-msg[data-thread=\"{thread}\"]:not(.thread-working)').length === 1"),
        "the question to appear in the log",
    );
}

/// Press Enter in an input, the way the panel's composer sends.
fn enter(page: &mut support::browser::Page, selector: &str) {
    page.eval(&format!(
        "document.querySelector({sel}).dispatchEvent(new KeyboardEvent('keydown', {{ key: 'Enter', bubbles: true }}))",
        sel = serde_json::to_string(selector).unwrap()
    ));
}

#[test]
fn the_working_state_follows_the_servers_state() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    let working = "!!document.querySelector('.pv-panel-msg.thread-working[data-thread=\"c-1\"]')";
    ask(&mut page, "task:t-a", "is this the whole plan?", "c-1");
    assert_eq!(page.eval(working), true);

    // A reload shows the question still waiting: the state says so.
    page.navigate(&s.page_url());
    connected(&mut page);
    page.click(".pv-panel-link");
    assert_eq!(
        page.eval(working),
        true,
        "the working row is derived from the state, not from this page's memory"
    );
    assert_eq!(
        page.text("document.querySelector('.pv-presence').dataset.mode"),
        "working"
    );

    // The answer arrives while the page is offline, so it comes back in a
    // snapshot rather than as a frame: the working state ends all the same.
    page.set_offline(true);
    s.repo
        .run(&[
            "reply",
            "--session",
            &s.session,
            "--thread",
            "c-1",
            "yes, all of it",
        ])
        .success();
    page.set_offline(false);
    page.wait_until(
        "document.querySelectorAll('.pv-panel-msg[data-thread=\"c-1\"]:not(.thread-working)').length === 2",
        "the answer to arrive in the snapshot",
    );
    assert_eq!(
        page.eval(working),
        false,
        "an answer carried by a snapshot ends the working row"
    );
    assert_eq!(
        page.text("document.querySelector('.pv-presence').dataset.mode"),
        "waiting"
    );

    // A second question, resolved by the agent without an answer: resolving
    // it ends the working state too, shows as a chip, and no no-agent
    // notice follows.
    ask(&mut page, "phase:p-one", "how long?", "c-2");
    assert_eq!(
        page.eval("!!document.querySelector('.pv-panel-msg.thread-working[data-thread=\"c-2\"]')"),
        true
    );
    s.repo
        .run(&[
            "resolve",
            "c-2",
            "--session",
            &s.session,
            "--declined",
            "--note",
            "Answered in the summary.",
        ])
        .success();
    page.wait_until(
        "!!document.querySelector('.pv-panel-msg[data-thread=\"c-2\"] .pv-ctx.is-resolution')",
        "the resolution to land in the log",
    );
    assert_eq!(page.text("document.querySelector('.pv-panel-msg[data-thread=\"c-2\"] .pv-ctx.is-resolution').textContent"), "declined");
    assert_eq!(
        page.eval("!!document.querySelector('.pv-panel-msg.thread-working[data-thread=\"c-2\"]')"),
        false
    );
    assert_eq!(
        page.text("document.querySelector('.pv-presence').dataset.mode"),
        "waiting"
    );
    s.server()
        .age_lease(artefacto::server::lease::TTL + std::time::Duration::from_secs(1));
    page.wait_until(
        "document.querySelector('.pv-presence').dataset.mode === 'off'",
        "the lease to expire",
    );
    assert_eq!(
        page.eval("!!document.querySelector('.pv-notice[data-kind=\"noagent\"]')"),
        false,
        "nothing is waiting"
    );
}

#[test]
fn a_resolution_stays_the_resolution_after_a_reply() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    comment(&mut page, "task:t-a", "needs a rollback step");
    s.repo
        .run(&[
            "resolve",
            "c-1",
            "--session",
            &s.session,
            "--changed",
            "--note",
            "Added t-rollback.",
        ])
        .success();
    page.wait_until(
        "document.querySelector('.thread[data-thread=\"c-1\"]').dataset.status === 'changed'",
        "the resolution",
    );
    // The reviewer replies after the resolution; the agent replies again.
    reply(&mut page, "c-1", "thanks");
    s.repo
        .run(&[
            "reply",
            "--session",
            &s.session,
            "--thread",
            "c-1",
            "any time",
        ])
        .success();
    page.wait_until(
        "document.querySelectorAll('.thread[data-thread=\"c-1\"] .thread-msg').length === 3",
        "comment, thanks, any time",
    );
    assert_eq!(
        page.text("document.querySelector('.thread[data-thread=\"c-1\"] .thread-resolution .thread-text').textContent"),
        "Added t-rollback.",
        "the note is the resolution because the server marked it, not because it was last"
    );
    // And after a reload, from the snapshot.
    page.navigate(&s.page_url());
    connected(&mut page);
    assert_eq!(
        page.text("document.querySelector('.thread[data-thread=\"c-1\"] .thread-resolution .thread-text').textContent"),
        "Added t-rollback."
    );
    let status = s.repo.json(&["status", "--json"]);
    let notes: Vec<bool> = status["artifacts"][0]["threads"][0]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["note"].as_bool().unwrap_or(false))
        .collect();
    assert_eq!(
        notes,
        vec![false, true, false, false],
        "only the note is a note"
    );
}

#[test]
fn the_panel_composer_sends_once_and_keeps_its_place() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    ask(&mut page, "task:t-a", "is this the whole plan?", "c-1");
    s.repo
        .run(&["reply", "--session", &s.session, "--thread", "c-1", "yes"])
        .success();
    page.wait_until(
        "document.querySelectorAll('.pv-panel-msg[data-thread=\"c-1\"]:not(.thread-working)').length === 2",
        "the answer",
    );
    let input = ".pv-panel-composer textarea";

    // Aimed at the thread, with text and a caret: all three survive a swap,
    // and the text and the aim survive a reload.
    page.click("[data-plan-ref=\"task:t-a\"] .ask-btn");
    page.type_into(input, "and the CLI layer?");
    page.eval(&format!("(function(){{ const t = document.querySelector('{input}'); t.focus(); t.setSelectionRange(4, 7); return true; }})()"));
    let plan = s.repo.path().join("plan.json");
    s.repo
        .run(&[
            "plan",
            "push",
            plan.to_str().unwrap(),
            "--json",
            "--session",
            &s.session,
            "--base-revision",
            "1",
        ])
        .success();
    page.wait_until(
        "window.artefactoPlan.debug().revision === 2 && !window.artefactoPlan.debug().syncing",
        "revision 2",
    );
    assert_eq!(
        page.eval(&format!("document.activeElement === document.querySelector('{input}') && document.activeElement.selectionStart === 4 && document.activeElement.selectionEnd === 7")),
        true,
        "focus and caret came back after the swap"
    );
    assert_eq!(
        page.text("document.querySelector('.pv-panel-target .pv-ctx').title"),
        "task:t-a",
        "still aimed"
    );
    assert_eq!(
        page.eval("!!document.querySelector('.pv-panel-event.is-revision')"),
        true,
        "the revision is in the log"
    );
    page.navigate(&s.page_url());
    connected(&mut page);
    assert_eq!(
        page.text(&format!("document.querySelector('{input}').value")),
        "and the CLI layer?",
        "the draft survived a reload"
    );
    assert_eq!(
        page.text("document.querySelector('.pv-panel-target .pv-ctx').title"),
        "task:t-a",
        "and so did the aim"
    );

    // A swap while the send is in flight: it sends once, and the live input
    // is cleared and re-enabled when the reply lands.
    shape_fetch(&mut page, "/cmd", "hold-response", 1200);
    enter(&mut page, input);
    assert_eq!(
        page.eval("document.querySelector('.pv-panel-composer .thread-composer-send').disabled"),
        true,
        "disabled in flight"
    );
    s.repo
        .run(&[
            "plan",
            "push",
            plan.to_str().unwrap(),
            "--json",
            "--session",
            &s.session,
            "--base-revision",
            "2",
        ])
        .success();
    page.wait_until(
        "window.artefactoPlan.debug().revision === 3 && !window.artefactoPlan.debug().syncing",
        "revision 3",
    );
    assert_eq!(
        page.eval("document.querySelector('.pv-panel-composer .thread-composer-send').disabled"),
        true,
        "still disabled after the swap"
    );
    enter(&mut page, input);
    page.wait_until(
        "document.querySelectorAll('.pv-panel-msg[data-thread=\"c-1\"]:not(.thread-working)').length === 3",
        "the follow-up to land",
    );
    std::thread::sleep(std::time::Duration::from_millis(1500));
    assert_eq!(
        s.server().count_events("chat.sent"),
        2,
        "the question and one follow-up, not two"
    );
    assert_eq!(
        page.text(&format!("document.querySelector('{input}').value")),
        "",
        "the live input was cleared"
    );
    assert_eq!(
        page.eval("document.querySelector('.pv-panel-composer .thread-composer-send').disabled"),
        false
    );

    // Aimed at a thread that is then deleted: the aim is dropped, the text
    // stays, and it sends to the plan as a whole.
    page.click("[data-plan-ref=\"task:t-a\"] .ask-btn");
    page.type_into(input, "one more thing");
    let cookie = s.server().session_cookie("plan:demo");
    s.server().post_cmd(
        &cookie,
        "plan:demo",
        serde_json::json!({ "cmd": "thread.delete", "client_id": "cid-del", "thread": "c-1" }),
    );
    page.wait_until(
        "!document.querySelector('.pv-panel-msg[data-thread=\"c-1\"]')",
        "the thread to go from the log",
    );
    assert_eq!(
        page.eval("document.querySelector('.pv-panel-target').hidden"),
        true,
        "the aim is dropped with the thread"
    );
    assert_eq!(
        page.text(&format!("document.querySelector('{input}').value")),
        "one more thing",
        "the text stays"
    );
    enter(&mut page, input);
    page.wait_until(
        "document.querySelectorAll('.pv-chat-msg').length === 1",
        "it went to the plan as a whole",
    );
}

#[test]
fn the_conversation_panel_starts_closed_docks_and_comes_back() {
    let Some(browser) = Browser::launch() else {
        return;
    };
    let s = served("minimal.json");
    let mut page = browser.new_page();
    page.navigate(&s.url);
    connected(&mut page);
    let closed = "document.querySelector('.pv-dock').classList.contains('is-hidden') && !document.querySelector('.pv-panel-handle').hidden";
    assert_eq!(
        page.eval(closed),
        true,
        "closed by default, with the handle in reach"
    );
    assert_eq!(
        page.text("document.querySelector('.pv-panel-handle .pv-panel-count').textContent"),
        "0"
    );
    assert_eq!(
        page.eval("!document.querySelector('.feedback-bar')"),
        true,
        "no bottom bar on a served page"
    );

    page.click(".pv-panel-handle");
    assert_eq!(page.eval("!document.querySelector('.pv-dock').classList.contains('is-hidden') && document.querySelector('.pv-panel-handle').hidden"), true, "the handle opens it and steps aside");
    assert_eq!(
        page.eval("!!document.querySelector('.pv-panel-foot .feedback-bar-send') && !!document.querySelector('.pv-panel-foot .feedback-bar-approve')"),
        true,
        "both verdicts live in the panel's foot"
    );
    assert_eq!(
        page.text("document.querySelector('.pv-panel-foot .feedback-bar-state-text').textContent"),
        "Saved \u{b7} rev 1"
    );

    // Wide: docked and sticky, the sheet gives it room. Narrow: it floats and
    // the sheet keeps its measure.
    page.call("Emulation.setDeviceMetricsOverride", serde_json::json!({ "width": 1440, "height": 900, "deviceScaleFactor": 1, "mobile": false }));
    assert_eq!(
        page.text("getComputedStyle(document.querySelector('.pv-dock')).position"),
        "sticky"
    );
    assert_eq!(
        page.eval("document.querySelector('.pv-sheet').getBoundingClientRect().width < 1100"),
        true,
        "the sheet narrowed for the dock"
    );
    page.screenshot(&screenshot_path("panel-docked-1440"));
    page.call("Emulation.setDeviceMetricsOverride", serde_json::json!({ "width": 1280, "height": 900, "deviceScaleFactor": 1, "mobile": false }));
    assert_eq!(
        page.text("getComputedStyle(document.querySelector('.pv-dock')).position"),
        "fixed"
    );
    assert_eq!(
        page.eval("document.querySelector('.pv-sheet').getBoundingClientRect().width >= 1170"),
        true,
        "the sheet keeps its measure under the overlay"
    );
    page.screenshot(&screenshot_path("panel-overlay-1280"));
    page.call(
        "Emulation.clearDeviceMetricsOverride",
        serde_json::json!({}),
    );

    // A question from the panel counts on the handle and the top-bar link.
    page.type_into(".pv-panel-composer textarea", "is this the whole plan?");
    page.click(".pv-panel-composer .thread-composer-send");
    page.wait_until(
        "document.querySelectorAll('.pv-chat-msg').length === 1",
        "the question in the log",
    );
    assert_eq!(
        page.text("document.querySelector('.pv-panel-link .pv-panel-count').textContent"),
        "1"
    );

    // The choice survives a reload; hiding returns to the handle, and that
    // survives too.
    page.navigate(&s.page_url());
    connected(&mut page);
    assert_eq!(
        page.eval("!document.querySelector('.pv-dock').classList.contains('is-hidden')"),
        true,
        "open stays open"
    );
    page.click(".pv-panel-hide");
    assert_eq!(page.eval(closed), true, "the X returns to the handle");
    assert_eq!(
        page.text("document.querySelector('.pv-panel-handle .pv-panel-count').textContent"),
        "1"
    );
    page.navigate(&s.page_url());
    connected(&mut page);
    assert_eq!(page.eval(closed), true, "closed stays closed");
    page.click(".pv-panel-link");
    assert_eq!(
        page.eval("!document.querySelector('.pv-dock').classList.contains('is-hidden')"),
        true,
        "the top-bar link opens it too"
    );
}
