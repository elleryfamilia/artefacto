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

#[allow(dead_code)]
struct Served {
    repo: Repo,
    server: InProcess,
    url: String,
    session: String,
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
    let _ = (&s.server, &s.session);
}
