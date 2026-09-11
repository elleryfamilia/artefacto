//! `artefacto open`: a fresh bootstrap link without a push.
//!
//! Spec 5: "mint a fresh bootstrap URL and open the browser". The page tells a
//! reviewer whose link died to run it, so every path here is one a reviewer
//! reaches from the page's own advice.

mod support;

use std::path::Path;
use support::{get, raw, status_of, Repo};

fn plan_in(repo: &Repo, name: &str) -> String {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/plan")
        .join(name);
    let target = repo.path().join(name);
    std::fs::copy(&source, &target).expect("copy the fixture");
    target.display().to_string()
}

fn push(repo: &Repo, plan: &str) -> serde_json::Value {
    repo.json(&["plan", "push", plan, "--json", "--no-open"])
}

/// The link's whole job: one GET signs the browser in and sends it to
/// `landing` — an artifact's page, or the index at `/`; a second GET is
/// refused.
fn assert_signs_in_once(port: u16, url: &str, landing: &str) {
    let path = url
        .strip_prefix(&format!("http://127.0.0.1:{port}"))
        .unwrap_or_else(|| panic!("the link must point at this server: {url}"));
    let first = get(port, path);
    assert_eq!(status_of(&first), 302, "{first}");
    assert!(
        first.contains("Set-Cookie: artefacto_session="),
        "the link trades itself for the page cookie: {first}"
    );
    assert!(
        first.contains(&format!("Location: {landing}\r\n")),
        "and lands on {landing}: {first}"
    );
    let second = get(port, path);
    assert_eq!(
        status_of(&second),
        403,
        "a link is spent on first use: {second}"
    );
}

#[test]
fn open_mints_a_fresh_link_that_signs_a_browser_in_once() {
    let repo = Repo::new();
    let plan = plan_in(&repo, "minimal.json");
    let pushed = push(&repo, &plan);
    let port = repo.port();

    let out = repo.run(&["open", "--no-open"]);
    let url = out.success().stdout.trim().to_string();
    repo_stop_later(&repo, || {
        assert!(
            url.starts_with("http://127.0.0.1:"),
            "prints the link: {url}"
        );
        assert_ne!(
            url,
            pushed["url"].as_str().unwrap(),
            "a fresh link, not the push's"
        );
        assert!(
            !url.contains(&repo.secret()),
            "the link is a one-time token, never the bearer secret"
        );
        assert_signs_in_once(port, &url, "/a/plan:demo");
    });
}

#[test]
fn open_json_carries_the_contract_and_the_link_works() {
    let repo = Repo::new();
    let plan = plan_in(&repo, "minimal.json");
    push(&repo, &plan);
    let port = repo.port();

    let out = repo.json(&["open", "--json"]);
    repo_stop_later(&repo, || {
        assert_eq!(out["ok"], true);
        assert_eq!(out["artifact"], "plan:demo");
        let url = out["url"].as_str().expect("a url");
        assert_signs_in_once(port, url, "/a/plan:demo");
    });
}

#[test]
fn open_with_several_artifacts_lands_on_the_index_and_refuses_an_unknown_name() {
    let repo = Repo::new();
    let demo = plan_in(&repo, "minimal.json");
    let sink = plan_in(&repo, "kitchen-sink.json");
    let first = push(&repo, &demo);
    let session = first["session"].as_str().expect("a session");
    repo.json(&[
        "plan",
        "push",
        &sink,
        "--json",
        "--no-open",
        "--session",
        session,
    ]);
    let port = repo.port();

    let unnamed = repo.run(&["open", "--no-open"]);
    let unknown = repo.run(&["open", "--no-open", "--artifact", "plan:nope"]);
    let named = repo.run(&["open", "--no-open", "--artifact", "plan:auth-refactor"]);
    repo_stop_later(&repo, || {
        // Spec 5: "with no id and several, the artifact index".
        let index_url = unnamed.success().stdout.trim().to_string();
        assert_signs_in_once(port, &index_url, "/");
        assert_eq!(unknown.code, 2, "{}", unknown.stdout);
        assert!(
            unknown.stderr.contains("plan:nope"),
            "names the id it did not find: {}",
            unknown.stderr
        );
        let url = named.success().stdout.trim().to_string();
        assert_signs_in_once(port, &url, "/a/plan:auth-refactor");
    });
}

#[test]
fn open_with_nothing_pushed_says_so_and_starts_nothing() {
    let repo = Repo::new();
    // A state directory and a log exist — a server ran here once, an agent
    // took a lease, and the server stopped — but nothing was ever pushed, so
    // there is no artifact to open.
    repo.run(&["serve", "--no-open"]).success();
    repo.json(&["await", "--timeout", "1s"]);
    repo.stop();
    let log = repo.state_dir().join("events.ndjson");
    assert!(
        std::fs::metadata(&log).is_ok_and(|m| m.len() > 0),
        "the log has a lease record in it"
    );
    let out = repo.run(&["open", "--no-open"]);
    repo_stop_later(&repo, || {
        assert_eq!(out.code, 2, "{}", out.stdout);
        assert!(
            out.stderr.contains("push"),
            "tells the user what would give it something to open: {}",
            out.stderr
        );
        assert!(
            out.stdout.trim().is_empty(),
            "no link is printed: {}",
            out.stdout
        );
        assert_eq!(
            repo.run(&["status", "--json"]).code,
            4,
            "no daemon was started just to say no"
        );
    });
}

#[test]
fn open_recovers_a_log_whose_torn_tail_ends_inside_a_multibyte_character() {
    // A crash mid-record can leave the log ending in the first byte of a
    // multibyte character. The server truncates that tail and serves the
    // rest; `open` must reach the same answer rather than read the log as
    // empty and say there is nothing to open.
    let repo = Repo::new();
    let plan = plan_in(&repo, "minimal.json");
    push(&repo, &plan);
    repo.stop();
    let log = repo.state_dir().join("events.ndjson");
    let mut bytes = std::fs::read(&log).unwrap();
    bytes.extend_from_slice(
        b"{\"format\":\"artefacto.event/1\",\"seq\":9,\"data\":{\"text\":\"caf\xc3",
    );
    std::fs::write(&log, &bytes).unwrap();
    assert!(
        String::from_utf8(bytes).is_err(),
        "the fixture is not valid UTF-8"
    );

    let out = repo.run(&["open", "--no-open"]);
    let port = repo.port();
    repo_stop_later(&repo, || {
        let url = out.success().stdout.trim().to_string();
        assert_signs_in_once(port, &url, "/a/plan:demo");
    });
}

#[test]
fn open_against_a_running_server_with_nothing_pushed_says_so() {
    let repo = Repo::new();
    repo.run(&["serve", "--no-open"]).success();
    let out = repo.run(&["open", "--no-open"]);
    repo_stop_later(&repo, || {
        assert_eq!(out.code, 2, "{}", out.stdout);
        assert!(out.stderr.contains("push"), "{}", out.stderr);
        assert!(out.stdout.trim().is_empty(), "{}", out.stdout);
    });
}

#[test]
fn open_starts_the_server_when_none_is_running() {
    // The page says "run `artefacto open` for a fresh link" when the server
    // is gone. After a self-exit the log is still there, so the link can be.
    let repo = Repo::new();
    let plan = plan_in(&repo, "minimal.json");
    push(&repo, &plan);
    repo.stop();
    assert_eq!(repo.run(&["status", "--json"]).code, 4, "stopped");

    let out = repo.run(&["open", "--no-open"]);
    let port = repo.port();
    repo_stop_later(&repo, || {
        let url = out.success().stdout.trim().to_string();
        assert_signs_in_once(port, &url, "/a/plan:demo");
    });
}

#[test]
fn an_open_link_is_a_page_route_and_needs_no_bearer() {
    // The browser presents nothing but the link; the bearer secret is for the
    // CLI. A link that needed the secret could not be opened by a browser.
    let repo = Repo::new();
    let plan = plan_in(&repo, "minimal.json");
    push(&repo, &plan);
    let port = repo.port();
    let out = repo.run(&["open", "--no-open"]);
    repo_stop_later(&repo, || {
        let url = out.success().stdout.trim().to_string();
        let path = url
            .strip_prefix(&format!("http://127.0.0.1:{port}"))
            .unwrap();
        let response = raw(
            port,
            &format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"),
        );
        assert_eq!(status_of(&response), 302, "{response}");
    });
}

#[test]
fn the_open_route_needs_the_bearer_secret() {
    // Minting a link is minting a credential; only the CLI, which holds the
    // bearer, may do it.
    let repo = Repo::new();
    let plan = plan_in(&repo, "minimal.json");
    push(&repo, &plan);
    let port = repo.port();
    repo_stop_later(&repo, || {
        for method in ["POST", "GET"] {
            let response = raw(
                port,
                &format!(
                    "{method} /cli/open HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\
                     Connection: close\r\n\r\n"
                ),
            );
            assert_eq!(status_of(&response), 401, "{method}: {response}");
            assert!(
                !response.contains("/b/"),
                "no link without the bearer: {response}"
            );
        }
    });
}

/// Run the assertions, then stop the daemon whether or not they passed, so a
/// failing test does not leave a server behind for the next one.
fn repo_stop_later(repo: &Repo, body: impl FnOnce()) {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));
    repo.stop();
    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}
