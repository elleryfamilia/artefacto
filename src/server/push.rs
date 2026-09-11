//! `POST /cli/push`: a new revision, and the threads it answers.
//!
//! # The base revision comes from the caller
//!
//! Spec 5 is emphatic about it. Reading the current revision here and
//! comparing it to itself would always pass, which makes the only concurrency
//! check in the system vacuous. The caller sends the revision it last saw; the
//! server compares and appends **inside one gate**, so two agents cannot both
//! pass the check.
//!
//! # A revision and its resolutions are one commit
//!
//! They go into the log through `Committer::append_all`, which frames them as
//! a batch a restart either keeps whole or drops whole. Appending them one at a
//! time — or in one `write_all` without the framing — lets a crash commit a
//! revision whose threads were never resolved, and nothing afterwards can tell
//! that happened.
//!
//! The page needs the same atomicity for a different reason: spec 4.3 says the
//! server sends "one snapshot holding the rendered body, thread state, and
//! resolutions together", because sending them separately would let the
//! reviewer see a body from one revision beside threads from another. One
//! commit becomes one frame.
//!
//! # The server validates the plan itself
//!
//! The CLI validates first, so an invalid plan normally never gets here. The
//! server does it again anyway and computes its own hash, so "the log holds
//! only validated plans" is true of the log rather than true of whoever wrote
//! to it.

use crate::plan::model;
use crate::server::event::{Actor, Frame};
use crate::server::http::{error_response, json_response, Committer, Query, Shared};
use crate::server::lease;
use crate::server::log::Pending;
use crate::server::review::ThreadStatus;
use serde::Deserialize;
use std::io::Read;
use std::sync::Arc;
use tiny_http::Request;

/// A plan is large; this is generous for one and far below anything that would
/// matter for the log.
const MAX_BODY: usize = 8 * 1024 * 1024;

#[derive(Debug, Deserialize)]
struct PushBody {
    /// The plan as JSON. Re-parsed and re-validated here.
    plan: serde_json::Value,
    #[serde(default)]
    source_path: String,
    #[serde(default)]
    base_revision: Option<u32>,
    #[serde(default)]
    force: bool,
    #[serde(default)]
    resolutions: Vec<Resolution>,
}

#[derive(Debug, Clone, Deserialize)]
struct Resolution {
    thread: String,
    status: String,
    #[serde(default)]
    note: String,
}

/// Why a push was refused, and which exit code the agent owes its caller.
enum Refusal {
    /// Exit 2. A usage error, even though the server is what noticed.
    MissingBaseRevision,
    /// Exit 7. Someone else moved first.
    Stale { seen: u32, current: u32 },
    /// Exit 2.
    Invalid(String),
}

impl Refusal {
    fn code(&self) -> &'static str {
        match self {
            Refusal::MissingBaseRevision => "base_revision_required",
            Refusal::Stale { .. } => "stale_base_revision",
            Refusal::Invalid(_) => "invalid_push",
        }
    }

    fn message(&self) -> String {
        match self {
            Refusal::MissingBaseRevision => {
                "a later push needs --base-revision <N> or --force".to_string()
            }
            Refusal::Stale { seen, current } => format!(
                "the server is at revision {current}, you pushed against {seen}; \
                 re-read with `artefacto status --json` or pass --force"
            ),
            Refusal::Invalid(why) => why.clone(),
        }
    }
}

pub fn handle_push(shared: &Arc<Shared>, mut request: Request, query: &Query) {
    if *request.method() != tiny_http::Method::Post {
        let _ = request.respond(error_response(405, "method_not_allowed", "push is a POST"));
        return;
    }
    let mut raw = String::new();
    if request
        .as_reader()
        .take(MAX_BODY as u64 + 1)
        .read_to_string(&mut raw)
        .is_err()
        || raw.len() > MAX_BODY
    {
        let _ = request.respond(error_response(413, "too_large", "the plan is too large"));
        return;
    }
    let body: PushBody = match serde_json::from_str(&raw) {
        Ok(body) => body,
        Err(e) => {
            let _ = request.respond(error_response(400, "invalid_push", &format!("{e}")));
            return;
        }
    };

    // The lease first, so a refusal costs nothing and names the holder.
    let session = match crate::server::poll::claim(shared, query) {
        Ok(session) => session,
        Err(e) => {
            let code = match e {
                lease::LeaseError::Held { .. } => "lease_held",
                lease::LeaseError::Superseded => "lease_superseded",
            };
            let _ = request.respond(error_response(409, code, &e.to_string()));
            return;
        }
    };

    match commit(shared, &body) {
        Ok(done) => {
            crate::server::socket::broadcast(shared, &done.frame);
            let mut result = done.result;
            result["session"] = serde_json::json!(session.token);
            let _ = request.respond(json_response(200, &result.to_string()));
        }
        Err(refusal) => {
            let _ = request.respond(error_response(409, refusal.code(), &refusal.message()));
        }
    }
}

struct Published {
    result: serde_json::Value,
    frame: Frame,
}

/// Validate, check the base revision, append, and fold — all under one gate.
fn commit(shared: &Arc<Shared>, body: &PushBody) -> Result<Published, Refusal> {
    let raw = serde_json::to_string(&body.plan)
        .map_err(|e| Refusal::Invalid(format!("the plan is not JSON: {e}")))?;
    let parsed = model::parse(&raw, false)
        .map_err(|issues| Refusal::Invalid(describe(&issues, "the plan does not parse")))?;
    let errors = model::validate(&parsed.plan);
    if !errors.is_empty() {
        return Err(Refusal::Invalid(describe(&errors, "the plan is invalid")));
    }
    let plan = parsed.plan;
    let artifact = artifact_id(&plan);
    let plan_hash = model::plan_hash(&plan);

    let committer = Committer::open(shared);

    let (current, previous_plan, threads) =
        committer.with_review(|review| match review.artifacts.get(&artifact) {
            Some(a) => (
                a.revision,
                a.plan.clone(),
                a.threads.iter().map(|t| t.id.clone()).collect::<Vec<_>>(),
            ),
            None => (0, serde_json::Value::Null, Vec::new()),
        });
    check_base_revision(current, body.base_revision, body.force)?;

    for resolution in &body.resolutions {
        if !matches!(resolution.status.as_str(), "changed" | "declined") {
            return Err(Refusal::Invalid(format!(
                "resolution for {} has status {:?}; it must be changed or declined",
                resolution.thread, resolution.status
            )));
        }
        if !threads.contains(&resolution.thread) {
            return Err(Refusal::Invalid(format!(
                "no such thread on {artifact}: {}",
                resolution.thread
            )));
        }
    }

    let revision = current + 1;
    let summary = summarize(
        &previous_plan,
        &serde_json::to_value(&plan).unwrap_or_default(),
    );
    let mut entries = vec![Pending::new(
        &artifact,
        revision,
        Actor::Agent,
        "revision.published",
        serde_json::json!({
            // Spec 4.2: the event carries the whole validated plan, not a
            // summary, because a summary cannot rebuild the body after a
            // restart and the log would stop being the source of truth.
            "plan": plan,
            "plan_hash": plan_hash,
            "source_path": body.source_path,
            "summary": summary,
        }),
    )];
    for resolution in &body.resolutions {
        entries.push(Pending::new(
            &artifact,
            revision,
            Actor::Agent,
            "thread.resolved",
            serde_json::json!({
                "thread": resolution.thread,
                "status": resolution.status,
                "note": resolution.note,
            }),
        ));
    }

    let events = committer
        .append_all(entries)
        .map_err(|e| Refusal::Invalid(format!("{e:#}")))?;
    let open_threads = committer.with_review(|review| {
        review
            .artifacts
            .get(&artifact)
            .map(|a| {
                a.threads
                    .iter()
                    .filter(|t| t.status == ThreadStatus::Open)
                    .count()
            })
            .unwrap_or(0)
    });
    drop(committer);

    // The page reaches the server through a one-time bootstrap URL, minted
    // fresh on every push so the agent always has a working link to hand over.
    let url = crate::server::page::mint_bootstrap(shared, &artifact)
        .map(|token| format!("http://127.0.0.1:{}/b/{token}", shared.port))
        .unwrap_or_default();

    Ok(Published {
        result: serde_json::json!({
            "ok": true,
            "artifact": artifact,
            "revision": revision,
            "url": url,
            // Spec 5: every JSON result carries the hash, the title, and the
            // phase and task counts.
            "plan_hash": plan_hash,
            "title": plan.meta.title,
            "phases": plan.phases.len(),
            "tasks": plan.phases.iter().map(|p| p.tasks.len()).sum::<usize>(),
            "summary": summary,
            "open_threads": open_threads,
            "seq": events.last().map(|e| e.seq).unwrap_or(0),
        }),
        frame: Frame::of(events),
    })
}

/// `plan:<meta.id>`. `meta.id` is already constrained by the model's id rule,
/// so it cannot hold a character that would break a URL or a header.
pub fn artifact_id(plan: &model::Plan) -> String {
    format!("plan:{}", plan.meta.id)
}

/// The check spec 5 exists for.
///
/// `base` comes from the caller, never from the server: it is the revision the
/// agent last saw. Reading the current revision here instead would compare the
/// server to itself and always pass.
fn check_base_revision(current: u32, base: Option<u32>, force: bool) -> Result<(), Refusal> {
    if force || current == 0 {
        return Ok(());
    }
    match base {
        None => Err(Refusal::MissingBaseRevision),
        Some(seen) if seen == current => Ok(()),
        Some(seen) => Err(Refusal::Stale { seen, current }),
    }
}

/// What changed, for the page's revision banner and for spec 6.3's
/// "`revision.published` with a change summary".
///
/// Derived rather than typed by the agent, because spec 5's `push` surface has
/// no flag for it and a summary nobody has to write is a summary that is
/// always there and always true.
fn summarize(previous: &serde_json::Value, next: &serde_json::Value) -> String {
    if previous.is_null() {
        return "first revision".to_string();
    }
    let before = elements(previous);
    let after = elements(next);
    let added = after.difference(&before).count();
    let removed = before.difference(&after).count();
    let retitled = titles(next)
        .iter()
        .filter(|(reference, title)| {
            titles(previous)
                .get(*reference)
                .is_some_and(|old| old != *title)
        })
        .count();

    let mut parts = Vec::new();
    if added > 0 {
        parts.push(format!("{added} added"));
    }
    if removed > 0 {
        parts.push(format!("{removed} removed"));
    }
    if retitled > 0 {
        parts.push(format!("{retitled} retitled"));
    }
    if parts.is_empty() {
        return "no change to phases or tasks".to_string();
    }
    parts.join(", ")
}

fn elements(plan: &serde_json::Value) -> std::collections::BTreeSet<String> {
    crate::server::review::plan_refs(plan)
}

fn titles(plan: &serde_json::Value) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    let Some(phases) = plan.get("phases").and_then(|p| p.as_array()) else {
        return out;
    };
    for phase in phases {
        let (Some(id), Some(title)) = (
            phase.get("id").and_then(|i| i.as_str()),
            phase.get("title").and_then(|t| t.as_str()),
        ) else {
            continue;
        };
        out.insert(format!("phase:{id}"), title.to_string());
        let Some(tasks) = phase.get("tasks").and_then(|t| t.as_array()) else {
            continue;
        };
        for task in tasks {
            if let (Some(id), Some(title)) = (
                task.get("id").and_then(|i| i.as_str()),
                task.get("title").and_then(|t| t.as_str()),
            ) {
                out.insert(format!("task:{id}"), title.to_string());
            }
        }
    }
    out
}

fn describe(issues: &[model::Issue], prefix: &str) -> String {
    let detail: Vec<String> = issues
        .iter()
        .take(5)
        .map(|i| format!("{}: {}", i.path, i.message))
        .collect();
    format!("{prefix}: {}", detail.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(phases: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "phases": phases })
    }

    #[test]
    fn the_first_push_needs_no_base_revision() {
        assert!(check_base_revision(0, None, false).is_ok());
    }

    #[test]
    fn a_later_push_without_a_base_revision_is_a_usage_error() {
        assert!(matches!(
            check_base_revision(1, None, false),
            Err(Refusal::MissingBaseRevision)
        ));
    }

    #[test]
    fn a_matching_base_revision_passes_and_a_stale_one_does_not() {
        assert!(check_base_revision(3, Some(3), false).is_ok());
        match check_base_revision(3, Some(1), false) {
            Err(Refusal::Stale { seen, current }) => {
                assert_eq!((seen, current), (1, 3));
            }
            _ => panic!("a stale base revision must be refused"),
        }
    }

    #[test]
    fn force_skips_the_check_entirely() {
        assert!(check_base_revision(3, Some(1), true).is_ok());
        assert!(check_base_revision(3, None, true).is_ok());
    }

    #[test]
    fn a_stale_refusal_says_how_to_catch_up() {
        let message = Refusal::Stale {
            seen: 1,
            current: 3,
        }
        .message();
        assert!(message.contains("status --json"), "{message}");
        assert!(message.contains("--force"), "{message}");
    }

    #[test]
    fn the_first_revision_says_so() {
        assert_eq!(
            summarize(&serde_json::Value::Null, &plan(serde_json::json!([]))),
            "first revision"
        );
    }

    #[test]
    fn a_summary_counts_what_moved() {
        let before = plan(serde_json::json!([
            { "id": "p-one", "title": "One", "tasks": [
                { "id": "t-a", "title": "A" }, { "id": "t-b", "title": "B" }
            ] }
        ]));
        let after = plan(serde_json::json!([
            { "id": "p-one", "title": "One", "tasks": [
                { "id": "t-a", "title": "A, revised" }, { "id": "t-c", "title": "C" }
            ] }
        ]));
        let summary = summarize(&before, &after);
        assert!(summary.contains("1 added"), "{summary}");
        assert!(summary.contains("1 removed"), "{summary}");
        assert!(summary.contains("1 retitled"), "{summary}");
    }

    #[test]
    fn an_unchanged_plan_says_nothing_moved() {
        let same = plan(serde_json::json!([
            { "id": "p-one", "title": "One", "tasks": [{ "id": "t-a", "title": "A" }] }
        ]));
        assert_eq!(
            summarize(&same, &same),
            "no change to phases or tasks",
            "a push that changes only prose is still a revision, and says so honestly"
        );
    }
}
