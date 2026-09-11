//! The artifact index page at `/` (spec 4.4): cookie-gated like the plan
//! page, one row per registry entry with its poster inline, links only for
//! artifacts this server holds, a per-row remove that is a page write, and
//! `open` landing on it when the server holds several artifacts.

mod support;

use std::path::Path;
use support::{get, raw, status_of, InProcess, Repo};

fn plan_in(repo: &Repo, fixture: &str, as_name: &str) -> String {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/plan")
        .join(fixture);
    let target = repo.path().join(as_name);
    std::fs::copy(source, &target).expect("copy the fixture");
    std::fs::canonicalize(&target)
        .expect("the copy exists")
        .display()
        .to_string()
}

/// A repository with a static render (`plan:demo`, never pushed) and a
/// pushed plan (`plan:auth-refactor`, live on this server).
struct Mixed {
    repo: Repo,
    server: InProcess,
    static_plan: String,
    static_page: String,
}

fn mixed() -> Mixed {
    let repo = Repo::new();
    let server = InProcess::start_in(&repo);
    let static_plan = plan_in(&repo, "minimal.json", "static.json");
    let rendered = repo.json(&[
        "plan",
        "render",
        &static_plan,
        "--out",
        "static.html",
        "--no-open",
        "--json",
    ]);
    let pushed = plan_in(&repo, "kitchen-sink.json", "plan.json");
    repo.json(&["plan", "push", &pushed, "--json", "--no-open"]);
    Mixed {
        repo,
        server,
        static_plan,
        static_page: rendered["out"].as_str().unwrap().to_string(),
    }
}

impl Mixed {
    fn cookie(&self) -> String {
        self.server.session_cookie("plan:auth-refactor")
    }

    fn index(&self) -> String {
        let cookie = self.cookie();
        let response = self.server.get("/", &[("Cookie", &cookie)]);
        assert_eq!(status_of(&response), 200, "{response}");
        response
    }

    fn remove(&self, headers: &str, body: &str) -> String {
        raw(
            self.server.port,
            &format!(
                "POST /index/remove HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n{headers}\
                 Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                self.server.port,
                body.len()
            ),
        )
    }

    fn rows(&self) -> Vec<String> {
        self.repo.json(&["list", "--json"])["artifacts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap().to_string())
            .collect()
    }
}

#[test]
fn the_index_needs_the_cookie_and_a_navigation_origin() {
    let m = mixed();
    let bare = get(m.server.port, "/");
    assert_eq!(status_of(&bare), 401, "{bare}");
    let cookie = m.cookie();
    let foreign = m.server.get(
        "/",
        &[("Cookie", &cookie), ("Origin", "http://evil.example")],
    );
    assert_eq!(status_of(&foreign), 403, "{foreign}");
    let bearer = m.server.get(
        "/",
        &[("Authorization", &format!("Bearer {}", m.repo.secret()))],
    );
    assert_eq!(
        status_of(&bearer),
        401,
        "a page route takes the cookie, never the bearer: {bearer}"
    );
    assert_eq!(status_of(&m.index()), 200);
}

#[test]
fn the_index_lists_every_row_with_its_poster_and_links_only_live_ones() {
    let m = mixed();
    let page = m.index();
    assert!(page.contains("Auth refactor"), "the pushed plan's title");
    assert!(page.contains("Demo plan"), "the static render's title");
    assert!(
        page.contains("href=\"/a/plan:auth-refactor\""),
        "the live artifact links to its page"
    );
    assert!(
        !page.contains("href=\"/a/plan:demo\""),
        "a static render has no page on this server to link to"
    );
    assert!(
        page.contains(&format!("Static page at {}", m.static_page)),
        "the static render says where its page is: {page}"
    );
    assert_eq!(page.matches("<svg").count(), 2, "a poster per row, inline");
    assert!(
        page.contains(">rev 1<") || page.contains("rev 1"),
        "the live row's revision"
    );
    assert!(page.contains("not pushed"), "the static row's revision");
    assert!(page.contains("2 artifacts"), "{page}");

    // The live row keeps no remove button; the static row offers one.
    assert!(
        page.contains("data-artifact=\"plan:demo\">Remove from index"),
        "{page}"
    );
    assert!(
        !page.contains("data-artifact=\"plan:auth-refactor\">Remove from index"),
        "a live row cannot be removed: {page}"
    );
    let live_row = page
        .split("<li class=\"")
        .find(|s| s.contains("plan:auth-refactor"))
        .unwrap();
    assert!(live_row.starts_with("ix-row is-live"), "{live_row}");

    // The spec's exact timestamp in the title attribute.
    let listed = m.repo.json(&["list", "--json"]);
    let revised = listed["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "plan:auth-refactor")
        .unwrap()["revised_at"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        page.contains(&format!("title=\"{revised}\">just now</time>")),
        "{page}"
    );
}

#[test]
fn the_index_is_served_under_the_nonce_policy() {
    let m = mixed();
    let page = m.index();
    let head = page.split("\r\n\r\n").next().unwrap();
    let nonce = head
        .lines()
        .find(|l| {
            l.to_ascii_lowercase()
                .starts_with("content-security-policy:")
        })
        .and_then(|l| l.split("'nonce-").nth(1))
        .and_then(|rest| rest.split('\'').next())
        .expect("a nonce in the policy")
        .to_string();
    let body = page.split("\r\n\r\n").nth(1).unwrap();
    assert!(!body.contains("http-equiv"), "no meta policy of its own");
    let stamped = format!("nonce=\"{nonce}\"");
    assert_eq!(
        body.matches("<script").count(),
        body.matches(&format!("<script {stamped}")).count(),
        "every script tag carries the header's nonce"
    );
    assert_eq!(
        body.matches("<style").count(),
        body.matches(&format!("<style {stamped}")).count(),
        "every style tag, the posters' own included"
    );
    assert!(
        body.matches("<style").count() >= 4,
        "two page sheets and two posters"
    );
}

#[test]
fn a_missing_source_greys_its_row_and_says_so() {
    let m = mixed();
    std::fs::remove_file(&m.static_plan).unwrap();
    let page = m.index();
    let row = page
        .split("<li class=\"")
        .find(|s| s.contains("plan:demo"))
        .unwrap();
    assert!(row.starts_with("ix-row is-missing"), "{row}");
    assert!(row.contains("(file missing)"), "{row}");
    let live = page
        .split("<li class=\"")
        .find(|s| s.contains("plan:auth-refactor"))
        .unwrap();
    assert!(!live.contains("is-missing"));
}

#[test]
fn remove_is_a_page_write_that_forgets_a_row_but_never_a_live_one() {
    let m = mixed();
    let cookie = m.cookie();
    let origin = m.server.origin();
    let body = r#"{"artifact":"plan:demo"}"#;

    let bare = m.remove("", body);
    assert_eq!(status_of(&bare), 401, "{bare}");
    let no_origin = m.remove(&format!("Cookie: {cookie}\r\n"), body);
    assert_eq!(
        status_of(&no_origin),
        403,
        "a write needs the strict origin: {no_origin}"
    );
    let foreign = m.remove(
        &format!("Cookie: {cookie}\r\nOrigin: http://evil.example\r\n"),
        body,
    );
    assert_eq!(status_of(&foreign), 403, "{foreign}");
    assert_eq!(m.rows().len(), 2, "nothing was removed by a refused write");

    let ok_headers = format!("Cookie: {cookie}\r\nOrigin: {origin}\r\n");
    let live = m.remove(&ok_headers, r#"{"artifact":"plan:auth-refactor"}"#);
    assert_eq!(
        status_of(&live),
        409,
        "a live row is kept current, not removed: {live}"
    );
    let bad = m.remove(&ok_headers, r#"{"artifact":"../etc"}"#);
    assert_eq!(status_of(&bad), 400, "{bad}");
    let none = m.remove(&ok_headers, r#"{}"#);
    assert_eq!(status_of(&none), 400, "{none}");

    let removed = m.remove(&ok_headers, body);
    assert_eq!(status_of(&removed), 200, "{removed}");
    assert_eq!(m.rows(), ["plan:auth-refactor"]);
    assert!(
        !m.repo.state_dir().join("posters/plan:demo.svg").exists(),
        "the poster goes with the row"
    );
    assert!(
        Path::new(&m.static_plan).exists() && Path::new(&m.static_page).exists(),
        "the user's own files are never touched"
    );
    let again = m.remove(&ok_headers, body);
    assert_eq!(status_of(&again), 404, "{again}");

    let get_it = m.server.get("/index/remove", &[("Cookie", &cookie)]);
    assert_eq!(status_of(&get_it), 405, "{get_it}");
}

#[test]
fn open_with_several_artifacts_lands_on_the_index() {
    let repo = Repo::new();
    let server = InProcess::start_in(&repo);
    let a = plan_in(&repo, "minimal.json", "a.json");
    let b = plan_in(&repo, "kitchen-sink.json", "b.json");
    repo.json(&["plan", "push", &a, "--json", "--no-open"]);
    repo.json(&["plan", "push", &b, "--json", "--no-open"]);

    let out = repo.json(&["open", "--json"]);
    assert_eq!(out["ok"], true);
    assert_eq!(out["index"], true, "{out}");
    assert_eq!(out["artifact"], serde_json::Value::Null);
    let url = out["url"].as_str().unwrap();
    let path = url
        .strip_prefix(&format!("http://127.0.0.1:{}", server.port))
        .expect("this server");

    let first = get(server.port, path);
    assert_eq!(status_of(&first), 302, "{first}");
    assert!(
        first.contains("Location: /\r\n"),
        "lands on the index: {first}"
    );
    let cookie = first
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("set-cookie:"))
        .and_then(|l| l.split_once(": ").map(|(_, v)| v))
        .and_then(|v| v.split(';').next())
        .expect("the link signs the browser in")
        .trim()
        .to_string();
    let second = get(server.port, path);
    assert_eq!(status_of(&second), 403, "spent on first use: {second}");

    let index = server.get("/", &[("Cookie", &cookie)]);
    assert_eq!(status_of(&index), 200, "{index}");
    assert!(
        index.contains("href=\"/a/plan:demo\"") && index.contains("href=\"/a/plan:auth-refactor\"")
    );

    // Naming one still opens that one.
    let named = repo.json(&["open", "--json", "--artifact", "plan:demo"]);
    assert_eq!(named["index"], false);
    assert_eq!(named["artifact"], "plan:demo");
    let named_path = named["url"]
        .as_str()
        .unwrap()
        .strip_prefix(&format!("http://127.0.0.1:{}", server.port))
        .unwrap()
        .to_string();
    let landed = get(server.port, &named_path);
    assert!(landed.contains("Location: /a/plan:demo\r\n"), "{landed}");

    // And the text form prints the link, whichever it is.
    let text = repo.run(&["open", "--no-open"]);
    text.success();
    assert!(
        text.stdout.trim().starts_with("http://127.0.0.1:"),
        "{}",
        text.stdout
    );
}

#[test]
fn the_served_plan_page_links_to_the_index_and_the_static_export_does_not() {
    let m = mixed();
    let cookie = m.cookie();
    let page = m
        .server
        .get("/a/plan:auth-refactor", &[("Cookie", &cookie)]);
    assert_eq!(status_of(&page), 200);
    assert!(
        page.contains("<a class=\"pv-topbar-link\" href=\"/\">All artifacts</a>"),
        "{}",
        &page[..600.min(page.len())]
    );
    let export = std::fs::read_to_string(&m.static_page).unwrap();
    assert!(
        !export.contains("pv-topbar-link\" href"),
        "a static export has no index to link to"
    );
}

#[test]
fn an_empty_index_says_how_to_fill_it() {
    let repo = Repo::new();
    let server = InProcess::start_in(&repo);
    let cookie = server.session_cookie("plan:any");
    let page = server.get("/", &[("Cookie", &cookie)]);
    assert_eq!(status_of(&page), 200, "{page}");
    assert!(page.contains("No artifacts yet"), "{page}");
    assert!(page.contains("artefacto plan push plan.json"));
    assert_eq!(page.matches("<li class=\"ix-row").count(), 0);
}
