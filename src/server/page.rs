//! How a browser authenticates, and the policy the page is served under.
//!
//! The page cannot hold the bearer secret: a token in a URL ends up in
//! history, in a referrer, and in the agent's transcript. So a one-time
//! bootstrap URL trades itself for an `HttpOnly` cookie and redirects to a
//! tokenless address.
//!
//! # Locks
//!
//! Takes `core` briefly for the bootstrap table. Never while holding
//! `sockets`, and never across I/O.

use crate::server::http::{error_response, header, json_response, Committer, Shared};
use crate::server::state_dir::new_secret;
use anyhow::{bail, Result};
use std::io::Cursor;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tiny_http::{Header, Request, Response};

pub const BOOTSTRAP_TTL: Duration = Duration::from_secs(300);
pub const COOKIE_NAME: &str = "artefacto_session";

/// Artifact ids reach a `Location` header and a URL path. A CR or LF would be
/// header injection, so the shape is checked once, at mint time.
pub fn valid_artifact_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && !id.contains("..")
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '-' | '_' | '.'))
}

pub fn mint_bootstrap(shared: &Shared, artifact: &str) -> Result<String> {
    mint_bootstrap_aged(shared, artifact, Duration::ZERO)
}

/// `age` backdates the token so expiry is testable without waiting five
/// minutes. `checked_sub`, because `Instant` subtraction panics on underflow
/// and a container booted moments ago makes that reachable.
pub fn mint_bootstrap_aged(shared: &Shared, artifact: &str, age: Duration) -> Result<String> {
    if !valid_artifact_id(artifact) {
        bail!("invalid artifact id: {artifact:?}");
    }
    let token = new_secret();
    let issued = Instant::now().checked_sub(age).unwrap_or_else(Instant::now);
    let mut core = shared.core.lock().unwrap();
    core.bootstrap
        .insert(token.clone(), (artifact.to_string(), issued));
    Ok(token)
}

pub fn bootstrap_url(port: u16, token: &str) -> String {
    format!("http://127.0.0.1:{port}/b/{token}")
}

/// Consumes the token whatever the outcome. A token presented once is spent,
/// even if it had already expired, so a leaked URL is never retryable.
fn consume_bootstrap(shared: &Shared, token: &str) -> Option<String> {
    let mut core = shared.core.lock().unwrap();
    let (artifact, issued) = core.bootstrap.remove(token)?;
    if issued.elapsed() > BOOTSTRAP_TTL {
        return None;
    }
    Some(artifact)
}

pub fn handle_bootstrap(shared: &Arc<Shared>, request: Request, token: &str) {
    let Some(artifact) = consume_bootstrap(shared, token) else {
        let _ = request.respond(error_response(
            403,
            "bad_bootstrap",
            "this link was already used or has expired; run `artefacto open` for a fresh one",
        ));
        return;
    };
    let cookie = format!(
        "{COOKIE_NAME}={}; HttpOnly; SameSite=Strict; Path=/",
        shared.page_cookie
    );
    let response = Response::empty(302)
        .with_header(Header::from_bytes(&b"Set-Cookie"[..], cookie.as_bytes()).expect("cookie"))
        .with_header(
            Header::from_bytes(&b"Location"[..], format!("/a/{artifact}").as_bytes())
                .expect("location"),
        );
    let _ = request.respond(response);
}

pub fn cookie_ok(req: &Request, shared: &Shared) -> bool {
    let Some(raw) = header(req, "Cookie") else {
        return false;
    };
    let prefix = format!("{COOKIE_NAME}=");
    raw.split(';')
        .map(str::trim)
        .filter_map(|kv| kv.strip_prefix(&prefix))
        .any(|v| constant_time_eq(v.as_bytes(), shared.page_cookie.as_bytes()))
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Absent `Origin` is allowed: a top-level navigation sends none.
pub fn origin_ok_navigation(req: &Request, port: u16) -> bool {
    match header(req, "Origin") {
        None => true,
        Some(o) => o == format!("http://127.0.0.1:{port}"),
    }
}

/// Absent `Origin` is refused. Spec 8 requires "the cookie plus an exact
/// Origin match" on page writes and the socket handshake, and a browser always
/// sends `Origin` on both — so absent means a non-browser client.
pub fn origin_ok_strict(req: &Request, port: u16) -> bool {
    header(req, "Origin").is_some_and(|o| o == format!("http://127.0.0.1:{port}"))
}

pub fn nonce() -> String {
    new_secret()[..32].to_string()
}

/// Spec 8. No `sandbox` directive: it makes the origin opaque, which breaks
/// the cookie the page authenticates with.
pub fn csp_header(port: u16, nonce: &str) -> Header {
    // `connect-src` names the socket and the page's own origin: commands
    // are POSTed over HTTP and the page catches up from `/state`, so the
    // socket alone would leave a page that can listen but never speak.
    let policy = format!(
        "default-src 'none'; script-src 'nonce-{nonce}'; style-src 'nonce-{nonce}'; \
         img-src data:; font-src data:; \
         connect-src ws://127.0.0.1:{port} http://127.0.0.1:{port}; \
         base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
    );
    Header::from_bytes(&b"Content-Security-Policy"[..], policy.as_bytes()).expect("csp header")
}

/// Removes the document's own meta CSP and stamps `nonce` onto every inline
/// `<script` and `<style` opening tag.
///
/// Both halves must change together. A header naming a nonce the document does
/// not carry blanks the page; leaving the meta policy in place blocks the
/// WebSocket, because that policy has no `connect-src` and a document under
/// two policies must satisfy both.
///
/// `</script>` and `</style>` are untouched: they begin `</`, so neither
/// matches the prefixes searched for here.
pub fn stamp_nonce(html: &str, nonce: &str) -> String {
    strip_meta_csp(html)
        .replace("<script", &format!("<script nonce=\"{nonce}\""))
        .replace("<style", &format!("<style nonce=\"{nonce}\""))
}

fn strip_meta_csp(html: &str) -> String {
    const NEEDLE: &str = "<meta http-equiv=\"Content-Security-Policy\"";
    let Some(start) = html.find(NEEDLE) else {
        return html.to_string();
    };
    let Some(end) = html[start..].find('>') else {
        return html.to_string();
    };
    let mut out = String::with_capacity(html.len());
    out.push_str(&html[..start]);
    out.push_str(&html[start + end + 1..]);
    out
}

/// What `/a/<artifact>` serves until `push` exists. Deliberately carries one
/// inline `<style>` and one inline `<script>`, so the nonce path is exercised
/// by a document whose contents are pinned here rather than left to chance.
pub fn placeholder_document(artifact: &str) -> String {
    // The id is escaped for the script the same way the render escapes its
    // data island: `serde_json` leaves `</script>` alone, and a raw request
    // can put anything in the path.
    let island = crate::plan::render::escape_json_island(
        &serde_json::to_string(artifact).expect("a string always serializes"),
    );
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <title>artefacto</title><style>body{{font-family:system-ui;margin:3rem}}</style>\
         </head><body><h1>artefacto</h1>\
         <p>No revision has been pushed for <code>{}</code> yet.</p>\
         <script>window.__artefacto={{artifact:{island}}};</script>\
         </body></html>",
        html_escape(artifact),
    )
}

/// What a navigation gets when the stored plan will not render. A page, not
/// a JSON body: the reviewer is looking at a browser tab.
fn error_document(message: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <title>artefacto</title><style>body{{font-family:system-ui;margin:3rem}}</style>\
         </head><body><h1>artefacto</h1>\
         <p>This revision could not be rendered: <code>{}</code></p>\
         </body></html>",
        html_escape(message),
    )
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The current revision of `artifact`, rendered. `None` when nothing has
/// been pushed for it; an error when the log holds a plan the renderer will
/// not take, which a validated push cannot produce.
pub fn render_artifact(shared: &Shared, artifact: &str) -> Result<Option<(u32, String)>> {
    let Some((revision, plan)) = crate::server::http::with_review(shared, |r| {
        r.artifacts
            .get(artifact)
            .map(|a| (a.revision, a.plan.clone()))
    }) else {
        return Ok(None);
    };
    Ok(Some((revision, render_plan(&plan)?)))
}

/// Render the plan a `revision.published` event carries. The server validates
/// on push, so this is the same document `plan render` would write.
pub fn render_plan(plan: &serde_json::Value) -> Result<String> {
    let raw = serde_json::to_string(plan)?;
    // Lenient on the read path: the log holds only what validated, but a
    // field this binary does not know is not a reason to refuse to show a
    // review that is already under way.
    let parsed = crate::plan::model::parse(&raw, true).map_err(|issues| {
        let first = issues
            .first()
            .map(|i| format!("{}: {}", i.path, i.message))
            .unwrap_or_default();
        anyhow::anyhow!("the stored plan does not render: {first}")
    })?;
    Ok(crate::plan::render::render(&parsed.plan))
}

/// The served page is the render plus two attributes on `<body>`: the artifact
/// id and the revision. Their presence is how the script knows it was served
/// rather than opened from a file, which selects the server-mode store.
pub fn served_document(rendered: &str, artifact: &str, revision: u32) -> String {
    rendered.replacen(
        "<body",
        &format!(
            "<body data-artefacto-artifact=\"{}\" data-artefacto-revision=\"{revision}\"",
            html_escape(artifact).replace('"', "&quot;")
        ),
        1,
    )
}

/// The body a push delivers and `/state` returns: the served document's
/// `<body …>…</body>`, markers and all, without the page's own script.
///
/// The markers matter. The page swaps its whole body on a push and mounts
/// again, and mount reads the body's attributes to know it was served; a
/// fragment without them would put the page into static mode with the
/// localStorage store spec 4.3 forbids there.
pub fn served_fragment(rendered: &str, artifact: &str, revision: u32) -> String {
    body_fragment(&served_document(rendered, artifact, revision)).unwrap_or_default()
}

/// `<body …>…</body>` of a document, without the page's own script.
///
/// The page it lands in already has the stylesheet and the script, and a
/// second copy of the script would not run anyway — markup inserted through
/// `innerHTML` never executes — but it would sit in the document as a lie.
/// The data island is kept; it is the plan the script reads.
pub fn body_fragment(rendered: &str) -> Option<String> {
    let start = rendered.find("<body")?;
    let end = rendered.rfind("</body>")? + "</body>".len();
    let body = &rendered[start..end];
    // The render emits the script as a bare `<script>` and the island as
    // `<script type=…>`, so the bare form identifies the script alone.
    let Some(s) = body.rfind("<script>") else {
        return Some(body.to_string());
    };
    let Some(len) = body[s..].find("</script>") else {
        return Some(body.to_string());
    };
    let mut out = String::with_capacity(body.len());
    out.push_str(&body[..s]);
    out.push_str(&body[s + len + "</script>".len()..]);
    Some(out)
}

/// Everything a page needs to show a review from nothing: the rendered body,
/// the raw plan, the folded threads, answers, marks and chat, who holds the
/// lease, and the log's high-water mark.
///
/// Read under the commit gate, so `last_seq` and the state describe the same
/// moment. Read separately, an event landing between the two reads is either
/// counted twice or never — the page skips logged events at or below
/// `last_seq` when it catches up, and that only works if the state includes
/// every one of them.
pub fn state_json(shared: &Arc<Shared>, artifact: &str) -> Result<Option<serde_json::Value>> {
    let snapshot = {
        let c = Committer::open(shared);
        let Some(art) = c.with_review(|r| r.artifacts.get(artifact).cloned()) else {
            return Ok(None);
        };
        let last_seq = shared.log.lock().unwrap().last_seq();
        (art, last_seq)
    };
    let (art, last_seq) = snapshot;
    let presence = crate::server::lease::current(shared)
        .map(|h| serde_json::json!({ "agent": h.agent, "mode": h.mode }));
    let html = served_fragment(&render_plan(&art.plan)?, artifact, art.revision);
    Ok(Some(serde_json::json!({
        "ok": true,
        "artifact": art.id,
        "revision": art.revision,
        "plan_hash": art.plan_hash,
        "plan": art.plan,
        "html": html,
        "threads": art.threads,
        "answers": art.answers,
        "reviewed": art.reviewed,
        "chat": art.chat,
        "submitted": art.submitted,
        "presence": presence,
        "last_seq": last_seq,
    })))
}

/// `GET /a/<artifact>/state`. A same-origin GET carries no `Origin` header,
/// so this takes the navigation rule: absent or exact.
pub fn handle_state(shared: &Arc<Shared>, request: Request, artifact: &str) {
    if !cookie_ok(&request, shared) {
        let _ = request.respond(error_response(401, "unauthorized", "no session cookie"));
        return;
    }
    if !origin_ok_navigation(&request, shared.port) {
        let _ = request.respond(error_response(403, "bad_origin", "origin not allowed"));
        return;
    }
    match state_json(shared, artifact) {
        Ok(Some(state)) => {
            let _ = request.respond(json_response(200, &state.to_string()));
        }
        Ok(None) => {
            let _ = request.respond(error_response(
                404,
                "not_found",
                "no revision has been pushed for this artifact",
            ));
        }
        Err(e) => {
            let _ = request.respond(error_response(500, "render_failed", &format!("{e:#}")));
        }
    }
}

pub fn serve_page(shared: &Arc<Shared>, request: Request, artifact: &str) {
    if !cookie_ok(&request, shared) {
        let _ = request.respond(error_response(401, "unauthorized", "no session cookie"));
        return;
    }
    if !origin_ok_navigation(&request, shared.port) {
        let _ = request.respond(error_response(403, "bad_origin", "origin not allowed"));
        return;
    }
    let (status, document) = match render_artifact(shared, artifact) {
        Ok(Some((revision, rendered))) => (200, served_document(&rendered, artifact, revision)),
        Ok(None) => (200, placeholder_document(artifact)),
        Err(e) => (500, error_document(&format!("{e:#}"))),
    };
    let n = nonce();
    let html = stamp_nonce(&document, &n);
    let response = Response::from_string(html)
        .with_status_code(status)
        .with_header(
            Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..])
                .expect("content type"),
        )
        .with_header(csp_header(shared.port, &n));
    let _ = request.respond(response);
}

/// Page writes: cookie plus a **strict** Origin. This is where the reviewer's
/// commands will arrive; the WebSocket is outbound only. See `socket`.
pub fn ok_json() -> Response<Cursor<Vec<u8>>> {
    json_response(200, "{\"ok\":true}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The document this will serve once `push` lands is the real plan render,
    /// so the transformation is pinned against that rather than against the
    /// placeholder this slice happens to serve.
    fn real_render() -> String {
        let raw = std::fs::read_to_string("tests/fixtures/plan/kitchen-sink.json")
            .expect("the fixture is present");
        let parsed = crate::plan::model::parse(&raw, false).expect("the fixture is valid");
        crate::plan::render::render(&parsed.plan)
    }

    #[test]
    fn stamping_covers_every_inline_tag_of_the_real_page() {
        let html = real_render();
        let opens = html.matches("<script").count() + html.matches("<style").count();
        assert!(
            html.matches("<script").count() >= 2,
            "a data island and a script"
        );
        assert!(html.contains("<style"));

        let out = stamp_nonce(&html, "abc123");
        assert_eq!(
            out.matches("nonce=\"abc123\"").count(),
            opens,
            "one unstamped tag is one blocked tag"
        );
    }

    #[test]
    fn stamping_removes_the_documents_own_meta_policy() {
        let html = real_render();
        assert!(html.contains("http-equiv=\"Content-Security-Policy\""));
        let out = stamp_nonce(&html, "abc123");
        assert!(!out.contains("http-equiv=\"Content-Security-Policy\""));
        assert!(
            !out.contains("unsafe-inline"),
            "no trace of the static policy is left"
        );
    }

    #[test]
    fn stamping_leaves_closing_tags_alone() {
        let out = stamp_nonce("<script>x</script><style>y</style>", "n");
        assert_eq!(
            out,
            "<script nonce=\"n\">x</script><style nonce=\"n\">y</style>"
        );
    }

    #[test]
    fn the_placeholder_exercises_the_nonce_path() {
        let doc = placeholder_document("plan:x");
        assert_eq!(doc.matches("<script").count(), 1);
        assert_eq!(doc.matches("<style").count(), 1);
        let out = stamp_nonce(&doc, "n");
        assert_eq!(out.matches("nonce=\"n\"").count(), 2);
    }

    #[test]
    fn the_body_fragment_keeps_the_island_and_drops_the_script() {
        let html = real_render();
        let body = body_fragment(&html).expect("a body");
        assert!(body.starts_with("<body"));
        assert!(body.ends_with("</body>"));
        assert!(body.contains("id=\"plan-data\""));
        assert!(!body.contains("<style"), "the head stays behind");
        assert_eq!(
            body.matches("<script").count(),
            1,
            "the island only; the page's script must not travel"
        );
        assert!(
            !body.contains("window.artefactoPlan"),
            "no trace of the script"
        );
    }

    #[test]
    fn the_placeholder_cannot_be_broken_out_of_through_the_id() {
        // A raw request can put anything in the path. `serde_json` does not
        // escape `</script>`, so the island escaping is what keeps a hostile
        // id inside the string it was written into.
        let doc = placeholder_document("x\"</script><script>alert(1)</script>");
        assert_eq!(doc.matches("<script").count(), 1, "{doc}");
        assert!(!doc.contains("</script><script>"));
    }

    #[test]
    fn the_served_fragment_carries_the_markers_the_page_mounts_by() {
        let html = real_render();
        let fragment = served_fragment(&html, "plan:auth-refactor", 4);
        assert!(fragment.starts_with("<body data-artefacto-artifact=\"plan:auth-refactor\""));
        assert!(fragment.contains("data-artefacto-revision=\"4\""));
        assert_eq!(fragment.matches("<script").count(), 1);
    }

    #[test]
    fn the_served_document_marks_the_body_once() {
        let html = real_render();
        let served = served_document(&html, "plan:auth-refactor", 3);
        assert_eq!(
            served
                .matches("data-artefacto-artifact=\"plan:auth-refactor\"")
                .count(),
            1
        );
        assert!(served.contains("data-artefacto-revision=\"3\""));
        assert!(
            served.contains("<body data-artefacto-artifact="),
            "on the body's own tag, where the script looks"
        );
    }

    #[test]
    fn an_id_with_control_characters_is_rejected() {
        assert!(valid_artifact_id("plan:auth-refactor"));
        assert!(!valid_artifact_id("plan:x\r\nSet-Cookie: a=b"));
        assert!(!valid_artifact_id("plan:x\n"));
        assert!(!valid_artifact_id(""));
        assert!(!valid_artifact_id("../etc/passwd"));
    }
}
