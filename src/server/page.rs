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

use crate::server::http::{error_response, header, json_response, Shared};
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
    let policy = format!(
        "default-src 'none'; script-src 'nonce-{nonce}'; style-src 'nonce-{nonce}'; \
         img-src data:; font-src data:; connect-src ws://127.0.0.1:{port}; \
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
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <title>artefacto</title><style>body{{font-family:system-ui;margin:3rem}}</style>\
         </head><body><h1>artefacto</h1>\
         <p>No revision has been pushed for <code>{}</code> yet.</p>\
         <script>window.__artefacto={{artifact:{}}};</script>\
         </body></html>",
        html_escape(artifact),
        serde_json::to_string(artifact).expect("a string always serializes"),
    )
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
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
    let n = nonce();
    let html = stamp_nonce(&placeholder_document(artifact), &n);
    let response = Response::from_string(html)
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
    fn an_id_with_control_characters_is_rejected() {
        assert!(valid_artifact_id("plan:auth-refactor"));
        assert!(!valid_artifact_id("plan:x\r\nSet-Cookie: a=b"));
        assert!(!valid_artifact_id("plan:x\n"));
        assert!(!valid_artifact_id(""));
        assert!(!valid_artifact_id("../etc/passwd"));
    }
}
