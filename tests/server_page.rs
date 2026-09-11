//! Page authentication, the CSP, and the outbound-only WebSocket.
//!
//! The test that carries this file is `a_quiet_page_does_not_block_a_broadcast`.
//! A design where one thread reads the socket while another writes it
//! deadlocks the moment a reviewer simply reads the page instead of typing
//! into it, and that deadlock reaches the accept loop through `page_count`.

mod support;
use support::*;

// --- bootstrap and the cookie ---------------------------------------------

#[test]
fn a_bootstrap_token_sets_a_cookie_and_redirects_once() {
    let s = InProcess::start();
    let token = s.mint("plan:x");

    let first = s.get(&format!("/b/{token}"), &[]);
    assert_eq!(status_of(&first), 302);
    let lower = first.to_ascii_lowercase();
    assert!(lower.contains("set-cookie:"));
    assert!(
        lower.contains("httponly"),
        "the page's own script must not read it"
    );
    assert!(lower.contains("samesite=strict"));
    assert!(
        !lower.contains(&token.to_ascii_lowercase()),
        "the redirect target carries no token"
    );

    assert_eq!(
        status_of(&s.get(&format!("/b/{token}"), &[])),
        403,
        "a bootstrap token is single use"
    );
}

#[test]
fn an_expired_token_is_refused_and_still_spent() {
    let s = InProcess::start();
    let token = artefacto::server::page::mint_bootstrap_aged(
        &s.shared,
        "plan:x",
        std::time::Duration::from_secs(301),
    )
    .expect("valid id");
    assert_eq!(status_of(&s.get(&format!("/b/{token}"), &[])), 403);
    assert_eq!(
        status_of(&s.get(&format!("/b/{token}"), &[])),
        403,
        "spent on presentation, so a leaked URL is never retryable"
    );
}

#[test]
fn an_artifact_id_with_a_newline_is_refused_at_mint_time() {
    let s = InProcess::start();
    assert!(
        artefacto::server::page::mint_bootstrap(&s.shared, "plan:x\r\nSet-Cookie: a=1").is_err(),
        "the id reaches a Location header, so a CR or LF would be header injection"
    );
}

#[test]
fn the_page_cookie_survives_a_restart() {
    let s = InProcess::start();
    let cookie = s.session_cookie("plan:x");
    let s = s.restart();
    assert_eq!(
        status_of(&s.get("/a/plan:x", &[("Cookie", &cookie)])),
        200,
        "the cookie is derived from the persisted secret, so a restart does not log the \
         reviewer out"
    );
}

#[test]
fn a_page_route_needs_the_cookie_and_not_the_bearer() {
    let s = InProcess::start();
    let bearer = format!("Bearer {}", s.shared.secret);
    assert_eq!(status_of(&s.get("/a/plan:x", &[])), 401);
    assert_eq!(
        status_of(&s.get("/a/plan:x", &[("Authorization", &bearer)])),
        401,
        "page routes accept the cookie and nothing else"
    );
}

#[test]
fn a_page_route_with_a_foreign_origin_is_refused() {
    let s = InProcess::start();
    let cookie = s.session_cookie("plan:x");
    assert_eq!(
        status_of(&s.get(
            "/a/plan:x",
            &[("Cookie", &cookie), ("Origin", "http://evil.example.com")]
        )),
        403
    );
}

// --- the content security policy ------------------------------------------

#[test]
fn the_served_page_carries_a_nonce_csp_and_no_meta_policy() {
    let s = InProcess::start();
    let cookie = s.session_cookie("plan:x");
    let r = s.get("/a/plan:x", &[("Cookie", &cookie)]);
    assert_eq!(status_of(&r), 200);

    let csp = r
        .lines()
        .find(|l| {
            l.to_ascii_lowercase()
                .starts_with("content-security-policy:")
        })
        .expect("every page response carries a CSP header");
    assert!(csp.contains("default-src 'none'"));
    assert!(csp.contains("'nonce-"));
    assert!(
        !csp.contains("'unsafe-inline'"),
        "the nonce replaces unsafe-inline"
    );
    assert!(csp.contains(&format!("connect-src ws://127.0.0.1:{}", s.port)));
    assert!(csp.contains("frame-ancestors 'none'"));
    assert!(
        !csp.contains("sandbox"),
        "a sandbox directive makes the origin opaque and kills the cookie"
    );
    assert!(
        !r.contains("http-equiv=\"Content-Security-Policy\""),
        "a document under two policies must satisfy both, and the static one has no \
         connect-src, so leaving it in blocks the socket"
    );
}

#[test]
fn every_inline_tag_in_the_served_page_carries_the_nonce() {
    let s = InProcess::start();
    let cookie = s.session_cookie("plan:x");
    let r = s.get("/a/plan:x", &[("Cookie", &cookie)]);
    let body = r.split("\r\n\r\n").nth(1).expect("a body");
    let nonce = r
        .split("'nonce-")
        .nth(1)
        .and_then(|x| x.split('\'').next())
        .expect("a nonce in the header");

    let opens = body.matches("<script").count() + body.matches("<style").count();
    assert!(
        opens > 0,
        "the document must contain inline tags for this to be a real test"
    );
    assert_eq!(
        body.matches(&format!("nonce=\"{nonce}\"")).count(),
        opens,
        "one unstamped tag is one blocked tag"
    );
}

#[test]
fn two_responses_never_share_a_nonce() {
    let s = InProcess::start();
    let cookie = s.session_cookie("plan:x");
    let grab = |r: &str| {
        r.split("'nonce-")
            .nth(1)
            .and_then(|x| x.split('\'').next())
            .map(str::to_string)
            .expect("a nonce")
    };
    assert_ne!(
        grab(&s.get("/a/plan:x", &[("Cookie", &cookie)])),
        grab(&s.get("/a/plan:x", &[("Cookie", &cookie)])),
        "a per-response nonce is the whole point"
    );
}

// --- the socket -----------------------------------------------------------

#[test]
fn a_socket_without_the_cookie_is_refused() {
    let s = InProcess::start();
    assert!(s.connect_page_raw(None, Some(&s.origin())).is_err());
}

#[test]
fn a_socket_with_no_origin_header_is_refused() {
    let s = InProcess::start();
    let cookie = s.session_cookie("plan:x");
    assert!(
        s.connect_page_raw(Some(&cookie), None).is_err(),
        "spec 8 requires an exact Origin on the handshake; a browser always sends one, \
         so absent means a non-browser client"
    );
}

#[test]
fn a_socket_with_a_foreign_origin_is_refused() {
    let s = InProcess::start();
    let cookie = s.session_cookie("plan:x");
    assert!(s
        .connect_page_raw(Some(&cookie), Some("http://evil.example.com"))
        .is_err());
}

#[test]
fn a_quiet_page_does_not_block_a_broadcast() {
    // The deadlock two reviews found in the plan for this code. This page
    // never sends anything, which is what a page being read looks like.
    let s = InProcess::start();
    let mut page = s.connect_page();
    page.hello();
    s.broadcast_test_frame(7);
    let frame = page.next_frame();
    assert_eq!(frame["format"], "artefacto.frame/1");
    assert_eq!(frame["seq"], 7);
    assert_eq!(frame["events"][0]["type"], "thread.opened");
}

#[test]
fn a_quiet_page_does_not_block_the_accept_loop() {
    let s = InProcess::start();
    let _page = s.connect_page();
    assert_eq!(
        status_of(&s.get("/healthz", &[])),
        200,
        "an open, silent page must not stop the server answering"
    );
}

#[test]
fn two_pages_both_receive_a_broadcast() {
    let s = InProcess::start();
    let mut a = s.connect_page();
    let mut b = s.connect_page();
    a.hello();
    b.hello();
    wait_for(|| s.page_count() == 2, "both pages registered");
    s.broadcast_test_frame(3);
    assert_eq!(a.next_frame()["seq"], 3);
    assert_eq!(b.next_frame()["seq"], 3);
}

#[test]
fn a_closed_page_is_dropped_from_the_registry() {
    let s = InProcess::start();
    let page = s.connect_page();
    wait_for(|| s.page_count() == 1, "the page registered");
    drop(page);
    // Detection needs writes, and it needs more than one. A write to a peer
    // that has gone away succeeds until its RST comes back, so the count is
    // how many writes it takes, not a property worth asserting. In production
    // the writer's heartbeat supplies these; here the test does, so it depends
    // on no timer.
    wait_for(
        || {
            s.broadcast_test_frame(1);
            s.page_count() == 0
        },
        "the closed page should be dropped once a write actually fails",
    );
}

#[test]
fn an_open_page_keeps_the_server_alive() {
    // Spec 4.2: self-exit is "no page and no agent", not "no traffic". A page
    // connected over a socket sends no further HTTP requests.
    let s = InProcess::start_with_idle(std::time::Duration::from_millis(200));
    let _page = s.connect_page();
    std::thread::sleep(std::time::Duration::from_millis(700));
    assert_eq!(
        status_of(&s.get("/healthz", &[])),
        200,
        "a page open and quiet must not let the daemon exit under the reviewer"
    );
}

#[test]
fn a_malformed_upgrade_is_refused_before_the_takeover() {
    let s = InProcess::start();
    let cookie = s.session_cookie("plan:x");
    // Cookie and Origin are right, but the RFC handshake fields are missing.
    let r = raw(
        s.port,
        &format!(
            "GET /ws HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nCookie: {cookie}\r\n\
             Origin: {}\r\nConnection: close\r\n\r\n",
            s.port,
            s.origin()
        ),
    );
    assert_eq!(status_of(&r), 400);
    assert!(r.contains("bad_handshake"));
}

// --- the served plan --------------------------------------------------------

#[test]
fn the_page_serves_the_pushed_plan_and_names_its_artifact() {
    let s = InProcess::start();
    s.seed_artifact();
    let cookie = s.session_cookie("plan:demo");
    let r = s.get("/a/plan:demo", &[("Cookie", &cookie)]);
    assert_eq!(status_of(&r), 200);
    let body = r.split("\r\n\r\n").nth(1).expect("a body");
    assert!(body.contains("Demo"), "the plan's title is on the page");
    assert!(
        body.contains("id=\"plan-data\""),
        "the plan island the page's script reads"
    );
    assert!(
        body.contains("data-artefacto-artifact=\"plan:demo\""),
        "the page learns which artifact it is, and that it was served, from the body"
    );
    assert!(
        !body.contains("No revision has been pushed"),
        "not the placeholder"
    );
}

#[test]
fn an_artifact_nobody_has_pushed_gets_the_placeholder() {
    let s = InProcess::start();
    let cookie = s.session_cookie("plan:nothing");
    let r = s.get("/a/plan:nothing", &[("Cookie", &cookie)]);
    assert_eq!(status_of(&r), 200);
    assert!(r.contains("No revision has been pushed"));
}

#[test]
fn the_served_plan_is_stamped_and_carries_no_meta_policy() {
    // The real render carries a meta CSP with no connect-src; left in place it
    // blocks the socket. And every one of its inline tags must be stamped.
    let s = InProcess::start();
    s.seed_artifact();
    let cookie = s.session_cookie("plan:demo");
    let r = s.get("/a/plan:demo", &[("Cookie", &cookie)]);
    let body = r.split("\r\n\r\n").nth(1).expect("a body");
    let nonce = r
        .split("'nonce-")
        .nth(1)
        .and_then(|x| x.split('\'').next())
        .expect("a nonce in the header");
    assert!(!body.contains("http-equiv=\"Content-Security-Policy\""));
    let opens = body.matches("<script").count() + body.matches("<style").count();
    assert!(opens >= 3, "a style, a data island, and the script");
    assert_eq!(body.matches(&format!("nonce=\"{nonce}\"")).count(), opens);
}

#[test]
fn the_csp_lets_the_page_post_commands_and_fetch_its_state() {
    // Commands go over HTTP, not the socket, so `connect-src` has to name the
    // page's own origin as well as the socket's.
    let s = InProcess::start();
    let cookie = s.session_cookie("plan:x");
    let r = s.get("/a/plan:x", &[("Cookie", &cookie)]);
    let csp = r
        .lines()
        .find(|l| {
            l.to_ascii_lowercase()
                .starts_with("content-security-policy:")
        })
        .expect("a CSP header");
    assert!(
        csp.contains(&format!("http://127.0.0.1:{}", s.port)),
        "{csp}"
    );
    assert!(csp.contains(&format!("ws://127.0.0.1:{}", s.port)), "{csp}");
}

// --- the state route --------------------------------------------------------

#[test]
fn the_state_route_returns_the_folded_review_and_the_rendered_body() {
    let s = InProcess::start();
    s.seed_artifact();
    let cookie = s.session_cookie("plan:demo");
    s.post_cmd(
        &cookie,
        "plan:demo",
        serde_json::json!({
            "cmd": "thread.open", "client_id": "cid-1", "ref": "task:t-a",
            "text": "why?", "blocking": true, "opened_revision": 1,
        }),
    );
    s.post_cmd(
        &cookie,
        "plan:demo",
        serde_json::json!({ "cmd": "element.reviewed", "client_id": "cid-2", "ref": "task:t-b", "on": true }),
    );

    let r = s.get("/a/plan:demo/state", &[("Cookie", &cookie)]);
    assert_eq!(status_of(&r), 200);
    let body = r.split("\r\n\r\n").nth(1).expect("a body");
    let v: serde_json::Value = serde_json::from_str(body).expect("json");
    assert_eq!(v["ok"], true);
    assert_eq!(v["artifact"], "plan:demo");
    assert_eq!(v["revision"], 1);
    assert_eq!(v["plan_hash"], "sha256:abc");
    assert_eq!(v["threads"][0]["id"], "c-1");
    assert_eq!(v["threads"][0]["target"], "task:t-a");
    assert_eq!(v["threads"][0]["status"], "open");
    assert_eq!(v["threads"][0]["messages"][0]["text"], "why?");
    assert_eq!(v["reviewed"][0], "task:t-b");
    assert_eq!(v["submitted"], false);
    assert!(v["presence"].is_null(), "no agent has attached");
    assert_eq!(v["last_seq"], s.last_seq());
    assert!(
        v["plan"]["phases"].is_array(),
        "the raw plan, for re-anchoring"
    );
    let html = v["html"].as_str().expect("the rendered body");
    assert!(html.starts_with("<body"), "{}", &html[..40.min(html.len())]);
    assert!(html.trim_end().ends_with("</body>"));
    assert!(
        html.contains("id=\"plan-data\""),
        "the island travels with the body"
    );
    assert!(
        !html.contains("<style"),
        "the stylesheet is already on the page"
    );
    assert_eq!(
        html.matches("<script").count(),
        1,
        "only the data island; the page's own script must not run twice"
    );
}

#[test]
fn the_state_route_reports_the_lease_holder() {
    let s = InProcess::start();
    s.seed_artifact();
    artefacto::server::lease::acquire(
        &s.shared,
        artefacto::server::lease::Claim::waiting("claude"),
    )
    .unwrap();
    let cookie = s.session_cookie("plan:demo");
    let r = s.get("/a/plan:demo/state", &[("Cookie", &cookie)]);
    let body = r.split("\r\n\r\n").nth(1).expect("a body");
    let v: serde_json::Value = serde_json::from_str(body).expect("json");
    assert_eq!(v["presence"]["agent"], "claude");
    assert_eq!(v["presence"]["mode"], "waiting");
}

#[test]
fn the_state_route_needs_the_cookie_and_knows_no_unknown_artifact() {
    let s = InProcess::start();
    s.seed_artifact();
    assert_eq!(status_of(&s.get("/a/plan:demo/state", &[])), 401);
    let cookie = s.session_cookie("plan:demo");
    assert_eq!(
        status_of(&s.get("/a/plan:nothing/state", &[("Cookie", &cookie)])),
        404
    );
}

// --- what the socket says first ---------------------------------------------

#[test]
fn hello_carries_the_presence_at_the_moment_of_connecting() {
    // Presence is announced on change only, so a page that connects after
    // the agent attached would otherwise never be told there is one.
    let s = InProcess::start();
    s.seed_artifact();
    artefacto::server::lease::acquire(
        &s.shared,
        artefacto::server::lease::Claim::waiting("claude"),
    )
    .unwrap();
    let mut page = s.connect_page();
    let hello = page.hello_frame();
    assert_eq!(hello["presence"]["agent"], "claude");
    assert_eq!(hello["presence"]["mode"], "waiting");
    assert_eq!(hello["last_seq"], s.last_seq());
}

#[test]
fn hello_says_no_agent_when_there_is_none() {
    let s = InProcess::start();
    let mut page = s.connect_page();
    let hello = page.hello_frame();
    assert!(hello["presence"].is_null());
    assert!(hello["page"].is_u64());
}
