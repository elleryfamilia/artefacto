//! The artifact index page (spec 4.4), served at `/`.
//!
//! One row per artifact in the registry, newest first, each with its drawn
//! poster inline, its age, its review state, where it came from and whether
//! that file is still there. A row whose artifact this server holds links
//! to the page; the rest are what the registry remembers — a static render,
//! or a review `clean` took out of the log — and say so. A row whose source
//! file is gone is greyed and, unless this server holds its review, offers a
//! per-row remove: a live row's next event would write it straight back, so
//! it says it is kept instead (a divergence from spec 4.4's sentence, noted
//! in BUILD-STATUS). Nothing is removed on the user's behalf: an absent file
//! usually means an unmounted volume.
//!
//! A page route: it needs the cookie and a navigation origin, like the plan
//! page, and is served under the same nonce policy. The posters are inline
//! SVG so the page's stylesheet can restyle them for a dark theme.

use crate::index::{self, Entry, Index};
use crate::server::http::{error_response, header, json_response, with_review, Shared};
use crate::server::page::{
    cookie_ok, csp_header, nonce, origin_ok_navigation, origin_ok_strict, stamp_nonce,
    valid_artifact_id,
};
use maud::{html, PreEscaped, DOCTYPE};
use std::collections::BTreeSet;
use std::io::Read;
use std::sync::Arc;
use tiny_http::{Header, Request, Response};

/// Rules the plan stylesheet does not have. Posters are restyled through
/// the page's variables so a dark theme reaches them; the selectors are
/// more specific than the card's own, which is what lets them win.
const CSS: &str = r#"
.ix-main { padding: 2.5rem var(--gutter) 0; }
.ix-lead { display: flex; justify-content: space-between; align-items: baseline; gap: 1rem; margin: 0 0 1.5rem; }
.ix-empty { font-size: 1.05rem; color: var(--muted); max-width: 40rem; }
.ix-empty code { font-family: var(--font-mono); font-size: 0.85rem; }
.ix-rows { list-style: none; margin: 0; padding: 0; }
.ix-row { display: grid; grid-template-columns: 200px minmax(0, 1fr); gap: 1.5rem; padding: 1.5rem 0; border-top: 1px solid var(--rule); }
.ix-row:last-child { border-bottom: 1px solid var(--rule); }
.ix-poster svg { width: 100%; height: auto; display: block; border-radius: 6px; box-shadow: var(--shadow-float); }
.ix-poster a { display: block; }
.ix-poster .ap-bg { fill: var(--page); stroke: var(--rule); }
.ix-poster .ap-kind { fill: var(--brand); }
.ix-poster .ap-kind-text { fill: var(--alarm-ink); }
.ix-poster .ap-title { fill: var(--ink); }
.ix-poster .ap-rev, .ix-poster .ap-counts, .ix-poster .ap-state { fill: var(--muted); }
.ix-poster .ap-phase { fill: var(--rule); }
.ix-poster .ap-phase-done { fill: var(--ok); }
.ix-poster .ap-risk-high { fill: var(--alarm); }
.ix-poster .ap-risk-medium { fill: var(--warn); }
.ix-poster .ap-risk-low { fill: var(--ok); }
.ix-poster-empty { aspect-ratio: 16 / 9; border: 1px dashed var(--rule); border-radius: 6px; display: grid; place-items: center; color: var(--muted); font-family: var(--font-mono); font-size: 0.719rem; }
.ix-head { display: flex; align-items: center; gap: 0.75rem; flex-wrap: wrap; margin: 0 0 0.5rem; }
.ix-title { margin: 0; font-size: 1.375rem; font-weight: 500; line-height: 1.25; }
.ix-title a { color: inherit; text-decoration: none; }
.ix-title a:hover { color: var(--action); }
.ix-facts, .ix-source, .ix-where { margin: 0.25rem 0; }
.ix-facts time { color: var(--ink); }
.ix-missing { color: var(--alarm); }
.is-missing .ix-title, .is-missing .ix-poster { opacity: 0.55; }
.ix-remove { appearance: none; border: 1px solid var(--rule); background: transparent; color: var(--muted); font-family: var(--font-mono); font-size: 0.719rem; letter-spacing: 0.14em; text-transform: uppercase; padding: 0.375rem 0.75rem; border-radius: 999px; cursor: pointer; margin-top: 0.75rem; }
.ix-remove:hover { color: var(--action); border-color: var(--action); }
.ix-remove:disabled { cursor: default; opacity: 0.6; }
@media (max-width: 640px) { .ix-row { grid-template-columns: 1fr; } }
"#;

/// The remove button's handler. Runs under the nonce, like the plan page's
/// script; the POST is same-origin, which the CSP's `connect-src` allows.
const JS: &str = r#"
(function () {
  var count = document.querySelector(".ix-count");
  document.querySelectorAll(".ix-remove").forEach(function (button) {
    button.addEventListener("click", function () {
      var row = button.closest(".ix-row");
      button.disabled = true;
      fetch("/index/remove", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ artifact: button.dataset.artifact })
      }).then(function (response) {
        if (!response.ok) { throw new Error(String(response.status)); }
        row.remove();
        var left = document.querySelectorAll(".ix-row").length;
        if (count) { count.textContent = left + (left === 1 ? " artifact" : " artifacts"); }
      }).catch(function () {
        button.disabled = false;
        button.textContent = "Could not remove";
      });
    });
  });
})();
"#;

/// One row as the page shows it: the registry's facts plus what only this
/// moment and this server know.
struct Row<'a> {
    entry: &'a Entry,
    live: bool,
    age: String,
    source_exists: bool,
    poster: Option<String>,
}

pub fn serve_index(shared: &Arc<Shared>, request: Request) {
    if !matches!(
        request.method(),
        tiny_http::Method::Get | tiny_http::Method::Head
    ) {
        let _ = request.respond(error_response(
            405,
            "method_not_allowed",
            "the index is a GET",
        ));
        return;
    }
    if !cookie_ok(&request, shared) {
        let _ = request.respond(error_response(401, "unauthorized", "no session cookie"));
        return;
    }
    if !origin_ok_navigation(&request, shared.port) {
        let _ = request.respond(error_response(403, "bad_origin", "origin not allowed"));
        return;
    }
    let n = nonce();
    let html = stamp_nonce(&index_document(shared), &n);
    let response = Response::from_string(html)
        .with_status_code(200)
        .with_header(
            Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..])
                .expect("content type"),
        )
        .with_header(csp_header(shared.port, &n));
    let _ = request.respond(response);
}

/// The page, before the nonce is stamped.
pub fn index_document(shared: &Shared) -> String {
    let index = Index::load(&shared.dir);
    let live: BTreeSet<String> = with_review(shared, |r| r.artifacts.keys().cloned().collect());
    let now = crate::time::now_secs();
    let rows: Vec<Row> = index
        .entries()
        .into_iter()
        .map(|entry| Row {
            entry,
            live: live.contains(&entry.id),
            age: index::age_label(&entry.revised_at, now),
            source_exists: std::path::Path::new(&entry.source_path).exists(),
            poster: std::fs::read_to_string(index::poster_path(&shared.dir, &entry.id)).ok(),
        })
        .collect();
    let count = match rows.len() {
        1 => "1 artifact".to_string(),
        n => format!("{n} artifacts"),
    };
    let notes = crate::commands::list::notes(&index);
    let page = html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { "Artifacts — artefacto" }
                style { (PreEscaped(crate::plan::render::stylesheet())) }
                style { (PreEscaped(CSS)) }
            }
            body data-artefacto-index="" {
                div.pv-sheet {
                    header.pv-topbar {
                        div.pv-brand {
                            span.pv-brand-mark aria-hidden="true" {}
                            span.pv-brand-name { "artefacto" }
                            span.pv-brand-surface { "Artifacts" }
                        }
                        div.pv-topbar-right {
                            span.pv-topbar-id.ix-count { (count) }
                        }
                    }
                    main.ix-main {
                        @for note in &notes {
                            p.ix-empty.ix-note { (note) }
                        }
                        @if rows.is_empty() && notes.is_empty() {
                            p.ix-empty {
                                "No artifacts yet. Push a plan with "
                                code { "artefacto plan push plan.json" }
                                " or render one with "
                                code { "artefacto plan render plan.json" }
                                ", and it appears here."
                            }
                        } @else if !rows.is_empty() {
                            p.pv-label { "Every artifact for this repository, newest first" }
                            ul.ix-rows {
                                @for row in &rows { (row_html(row)) }
                            }
                        }
                    }
                }
                script { (PreEscaped(JS)) }
            }
        }
    };
    page.into_string()
}

fn row_html(row: &Row) -> maud::Markup {
    let e = row.entry;
    let page = format!("/a/{}", e.id);
    let mut classes = String::from("ix-row");
    if row.live {
        classes.push_str(" is-live");
    }
    if !row.source_exists {
        classes.push_str(" is-missing");
    }
    let revision = if e.revision == 0 {
        "not pushed".to_string()
    } else {
        format!("rev {}", e.revision)
    };
    let verdict = match e.verdict.as_deref() {
        Some("approve") => Some("approved"),
        Some("request_changes") => Some("changes requested"),
        Some("comment") => Some("commented"),
        Some(other) => Some(other),
        None => None,
    };
    html! {
        li class=(classes) data-artifact=(e.id) {
            div.ix-poster {
                @match &row.poster {
                    Some(svg) => {
                        @if row.live {
                            a href=(page) aria-label=(format!("Open {}", e.title)) { (PreEscaped(svg)) }
                        } @else {
                            (PreEscaped(svg))
                        }
                    }
                    None => div.ix-poster-empty { "no poster" }
                }
            }
            div.ix-body {
                div.ix-head {
                    span.pv-chip { (e.kind) }
                    h2.ix-title {
                        @if row.live { a href=(page) { (e.title) } } @else { (e.title) }
                    }
                }
                p.pv-meta.ix-facts {
                    span.ix-id { (e.id) }
                    " · "
                    time datetime=(e.revised_at) title=(e.revised_at) { (row.age) }
                    " · " (revision)
                    @if e.revision > 0 {
                        " · " (e.open_threads) " open"
                        @if e.unanchored_threads > 0 { " · " (e.unanchored_threads) " unanchored" }
                        " · " (verdict.unwrap_or("in review"))
                    }
                }
                p.pv-meta.ix-source {
                    "from " (e.source_path)
                    @if !row.source_exists { " " span.ix-missing { "(file missing)" } }
                }
                p.pv-meta.ix-where {
                    @if row.live {
                        a href=(page) { "Open on this server" }
                        @if !row.source_exists {
                            " · kept in the index while its review is open here"
                        }
                    } @else if let Some(rendered) = &e.rendered_path {
                        "Static page at " (rendered)
                    } @else {
                        "Not on this server; push it again to review it."
                    }
                }
                @if !row.live {
                    button.ix-remove type="button" data-artifact=(e.id) { "Remove from index" }
                }
            }
        }
    }
}

/// `POST /index/remove` with `{"artifact": id}`: forget a row. A page write,
/// so cookie plus a strict Origin. An artifact this server holds is refused:
/// its next event would write the row straight back.
pub fn handle_remove(shared: &Arc<Shared>, mut request: Request) {
    if *request.method() != tiny_http::Method::Post {
        let _ = request.respond(error_response(
            405,
            "method_not_allowed",
            "remove is a POST",
        ));
        return;
    }
    if !cookie_ok(&request, shared) {
        let _ = request.respond(error_response(401, "unauthorized", "no session cookie"));
        return;
    }
    if !origin_ok_strict(&request, shared.port) {
        let _ = request.respond(error_response(403, "bad_origin", "origin not allowed"));
        return;
    }
    let _ = header(&request, "Content-Type");
    let mut raw = String::new();
    if request
        .as_reader()
        .take(4096)
        .read_to_string(&mut raw)
        .is_err()
    {
        let _ = request.respond(error_response(400, "invalid_body", "unreadable body"));
        return;
    }
    let body: serde_json::Value = serde_json::from_str(&raw).unwrap_or_default();
    let Some(id) = body.get("artifact").and_then(|a| a.as_str()) else {
        let _ = request.respond(error_response(400, "invalid_body", "no artifact named"));
        return;
    };
    if !valid_artifact_id(id) {
        let _ = request.respond(error_response(
            400,
            "invalid_artifact",
            "not an artifact id",
        ));
        return;
    }
    if with_review(shared, |r| r.artifacts.contains_key(id)) {
        let _ = request.respond(error_response(
            409,
            "live",
            "this artifact is open on this server; its row is kept current and cannot be removed",
        ));
        return;
    }
    match index::remove(&shared.dir, id) {
        Ok(index::Removed::Removed) => {
            let _ = request.respond(json_response(200, "{\"ok\":true}"));
        }
        Ok(index::Removed::Absent) => {
            let _ = request.respond(error_response(404, "unknown_artifact", "no such row"));
        }
        Ok(index::Removed::ReadOnlyNewer) => {
            let _ = request.respond(error_response(
                409,
                "readonly",
                "the index was written by a newer artefacto",
            ));
        }
        Err(e) => {
            let _ = request.respond(error_response(500, "index_error", &format!("{e:#}")));
        }
    }
}
