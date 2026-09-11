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
}

impl Served {
    fn server(&self) -> &InProcess {
        &self.server
    }

    /// The page's tokenless address, for a second tab or a reload.
    fn page_url(&self) -> String {
        format!("http://127.0.0.1:{}/a/plan:demo", self.server().port)
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
        page.eval("!!document.querySelector('.feedback-bar')"),
        true,
        "the page mounted its controls"
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
        "window.artefactoPlan.debug() && window.artefactoPlan.debug().connected && !window.artefactoPlan.debug().syncing",
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
        "none"
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
    let errors = page.errors();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
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
        3,
        "comment, reply, note"
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

    page.click(".feedback-bar-approve input");
    assert_eq!(
        page.text("document.querySelector('.feedback-bar-send').textContent"),
        "Send approval"
    );
    page.click(".feedback-bar-send");
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

    page.click(".feedback-bar-chat");
    page.type_into(".pv-chat .composer textarea", "is this the whole plan?");
    page.click(".pv-chat .composer .composer-send");
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

    comment(&mut page, "task:t-a", "and this?");
    page.click(".thread[data-thread=\"c-1\"] .thread-ask");
    page.type_into(".thread[data-thread=\"c-1\"] .composer textarea", "really?");
    page.click(".thread[data-thread=\"c-1\"] .composer .composer-send");
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
        page.eval("!!document.querySelector('.feedback-bar-copy')"),
        true,
        "the static export keeps the clipboard flow"
    );
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
        page.text("document.querySelector('.pv-notice[data-kind=\"revision\"]').textContent")
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
        page.text("document.querySelector('.pv-notice[data-kind=\"revision\"]').textContent")
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

    // A comment from the other tab lands while tab a may still be in its
    // backoff. Its POST retries onto a fresh connection; tab a's socket
    // was closed, so it never hears the broadcast.
    comment(&mut b, "task:t-a", "while it was away");

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
        "document.querySelectorAll('.thread[data-thread=\"c-1\"] .thread-msg').length === 2",
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
