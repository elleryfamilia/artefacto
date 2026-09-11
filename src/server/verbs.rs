//! The agent's write verbs: `reply` and `resolve`.
//!
//! Both are mutations, so both carry the session token (spec 4.2), and both
//! validate it **inside** the same `Committer` that appends — otherwise a
//! takeover landing between the check and the write would let an agent that
//! has lost the lease still write.
//!
//! `reply --nudge` is the exception: a nudge is announced to open pages and
//! never logged, because it is a banner rather than a fact about the review.
//! Spec 6.3 requires the event; spec 5's `reply` surface has no flag for it,
//! so this adds one.

use crate::server::event::{Actor, Frame};
use crate::server::http::{error_response, json_response, Committer, Query, Shared};
use crate::server::lease::{self, LeaseError};
use crate::server::presence;
use crate::server::review::{LeaseRecord, Review};
use std::io::Read;
use std::sync::Arc;
use tiny_http::Request;

/// The text of a reply or the note on a resolution arrives as the body rather
/// than in the query, so nothing a reviewer or an agent writes has to survive
/// URL encoding to reach the log intact.
const MAX_BODY: usize = crate::server::ingress::MAX_TEXT;

/// `POST /cli/reply`. A thread message, page-level chat, or a banner.
pub fn handle_reply(shared: &Arc<Shared>, request: Request, query: &Query) {
    let Some((request, text)) = body_of(request) else {
        return;
    };
    let session = match validated(shared, query) {
        Ok(session) => session,
        Err(denied) => return deny(request, denied),
    };

    if query.get("nudge").map(String::as_str) == Some("1") {
        let artifact = match target_artifact(shared, query, None) {
            Ok(artifact) => artifact,
            Err(why) => return refuse(request, &why),
        };
        presence::announce(
            shared,
            &artifact,
            "nudge",
            serde_json::json!({ "agent": session.name, "text": text }),
        );
        let _ = request.respond(json_response(
            200,
            &serde_json::json!({ "ok": true, "nudge": true, "artifact": artifact }).to_string(),
        ));
        return;
    }

    let thread = query.get("thread").filter(|t| !t.is_empty()).cloned();
    let artifact = match target_artifact(shared, query, thread.as_deref()) {
        Ok(artifact) => artifact,
        Err(why) => return refuse(request, &why),
    };

    let committer = Committer::open(shared);
    if let Err(e) = lease::validate(shared, &session.token) {
        drop(committer);
        return refuse_lease(request, e);
    }
    let revision = committer.with_review(|r| r.artifacts.get(&artifact).map_or(0, |a| a.revision));
    // Spec 6.3: an agent's answer inside a thread is `thread.replied`. Without
    // a thread it is page-level chat, which the fold keeps on the artifact.
    let (kind, data) = match &thread {
        Some(thread) => (
            "thread.replied",
            serde_json::json!({ "thread": thread, "text": text }),
        ),
        None => ("chat.sent", serde_json::json!({ "text": text })),
    };
    let event = match committer.append(&artifact, revision, Actor::Agent, kind, data) {
        Ok(event) => event,
        Err(e) => return refuse(request, &format!("{e:#}")),
    };
    // Under the gate, so pages hear commits in log order.
    crate::server::socket::broadcast(shared, &Frame::of(vec![event.clone()]));
    drop(committer);

    let _ = request.respond(json_response(
        200,
        &serde_json::json!({
            "ok": true, "artifact": artifact, "thread": thread, "seq": event.seq,
        })
        .to_string(),
    ));
}

/// `POST /cli/resolve`. Spec 6.3: `thread.resolved` with `changed` or
/// `declined` and a note.
pub fn handle_resolve(shared: &Arc<Shared>, request: Request, query: &Query) {
    let Some((request, note)) = body_of(request) else {
        return;
    };
    let session = match validated(shared, query) {
        Ok(session) => session,
        Err(denied) => return deny(request, denied),
    };

    let Some(thread) = query.get("thread").filter(|t| !t.is_empty()).cloned() else {
        return refuse(request, "resolve needs a thread id");
    };
    let status = query.get("status").map(String::as_str).unwrap_or_default();
    if !matches!(status, "changed" | "declined") {
        return refuse(
            request,
            "resolve needs exactly one of --changed or --declined",
        );
    }
    let artifact = match target_artifact(shared, query, Some(&thread)) {
        Ok(artifact) => artifact,
        Err(why) => return refuse(request, &why),
    };

    let committer = Committer::open(shared);
    if let Err(e) = lease::validate(shared, &session.token) {
        drop(committer);
        return refuse_lease(request, e);
    }
    let revision = committer.with_review(|r| r.artifacts.get(&artifact).map_or(0, |a| a.revision));
    let event = match committer.append(
        &artifact,
        revision,
        Actor::Agent,
        "thread.resolved",
        serde_json::json!({ "thread": thread, "status": status, "note": note }),
    ) {
        Ok(event) => event,
        Err(e) => return refuse(request, &format!("{e:#}")),
    };
    crate::server::socket::broadcast(shared, &Frame::of(vec![event.clone()]));
    drop(committer);

    let _ = request.respond(json_response(
        200,
        &serde_json::json!({
            "ok": true, "artifact": artifact, "thread": thread,
            "status": status, "seq": event.seq,
        })
        .to_string(),
    ));
}

/// Which artifact this call is about.
///
/// Spec 5: "`reply` needs either a thread or an artifact. When the server has
/// exactly one artifact, `--artifact` may be omitted for page-level chat."
/// A thread id is only unique within an artifact (`c-<n>` per artifact), so a
/// thread that exists on two of them is ambiguous and is refused by name
/// rather than guessed at.
fn target_artifact(
    shared: &Arc<Shared>,
    query: &Query,
    thread: Option<&str>,
) -> Result<String, String> {
    crate::server::http::with_review(shared, |review| {
        if let Some(named) = query.get("artifact").filter(|a| !a.is_empty()) {
            if !review.artifacts.contains_key(named) {
                return Err(format!("no such artifact: {named}"));
            }
            if let Some(thread) = thread {
                if review.artifacts[named].thread(thread).is_none() {
                    return Err(format!("no thread {thread} on {named}"));
                }
            }
            return Ok(named.clone());
        }
        match thread {
            Some(thread) => by_thread(review, thread),
            None => only_artifact(review),
        }
    })
}

fn by_thread(review: &Review, thread: &str) -> Result<String, String> {
    let found: Vec<&String> = review
        .artifacts
        .iter()
        .filter(|(_, artifact)| artifact.thread(thread).is_some())
        .map(|(id, _)| id)
        .collect();
    match found.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(format!("no thread {thread} on any artifact")),
        many => Err(format!(
            "{thread} exists on {}; name one with --artifact",
            many.iter()
                .map(|id| id.as_str())
                .collect::<Vec<_>>()
                .join(" and ")
        )),
    }
}

fn only_artifact(review: &Review) -> Result<String, String> {
    match review.artifacts.len() {
        1 => Ok(review.artifacts.keys().next().cloned().unwrap_or_default()),
        0 => Err("there is nothing to reply to yet; push a plan first".to_string()),
        _ => Err("this server has several artifacts; name one with --artifact".to_string()),
    }
}

/// Read the body before touching the lease, so the request is always consumed.
fn body_of(mut request: Request) -> Option<(Request, String)> {
    let mut text = String::new();
    let read = request
        .as_reader()
        .take(MAX_BODY as u64 + 1)
        .read_to_string(&mut text);
    if read.is_err() || text.len() > MAX_BODY {
        let _ = request.respond(error_response(
            413,
            "too_large",
            "the text is longer than the server will store",
        ));
        return None;
    }
    Some((request, text))
}

/// A refusal, as the status, code and message the caller should send. Kept as
/// data rather than sent here because `Request::respond` consumes the request,
/// and the caller is the one that owns it.
struct Denied {
    status: u16,
    code: &'static str,
    message: String,
}

fn validated(shared: &Arc<Shared>, query: &Query) -> Result<LeaseRecord, Denied> {
    let Some(token) = query.get("session") else {
        return Err(Denied {
            status: 400,
            code: "usage",
            message: "this command needs --session".to_string(),
        });
    };
    lease::validate(shared, token).map_err(|e| {
        let (status, code) = e.http();
        Denied {
            status,
            code,
            message: e.to_string(),
        }
    })
}

fn deny(request: Request, denied: Denied) {
    let _ = request.respond(error_response(denied.status, denied.code, &denied.message));
}

fn refuse(request: Request, why: &str) {
    let _ = request.respond(error_response(409, "refused", why));
}

fn refuse_lease(request: Request, error: LeaseError) {
    let (status, code) = error.http();
    let _ = request.respond(error_response(status, code, &error.to_string()));
}
