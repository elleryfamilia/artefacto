//! What the reviewer's page is allowed to say, and what the server does with it.
//!
//! Commands arrive over **HTTP**, not over the WebSocket: the socket is
//! outbound only, for the reasons in [`crate::server::socket`]. The route is
//! `POST /a/<artifact>/cmd`, guarded by the same cookie and a strict `Origin`.
//!
//! # Four rules
//!
//! 1. **The server assigns thread ids.** `c-<n>` per artifact, from a counter
//!    that only increases. A page-chosen id lets two tabs collide, and spec
//!    6.6 requires ids never be renumbered.
//! 2. **A `client_id` already committed is acknowledged, not re-applied**, and
//!    the reply carries the id that command originally assigned. That is what
//!    makes a reconnect-and-retry safe around a submit.
//! 3. **The artifact comes from the URL and the revision from the command.**
//!    A command that names neither cannot be routed when two artifacts are
//!    open, and spec 4.3 requires an edit to carry the revision its composer
//!    was opened against, so text written against revision 3 never arrives
//!    labelled revision 4.
//! 4. **Text is data.** Stored and delivered as a JSON string, never
//!    interpreted as markup anywhere.
//!
//! # Locks
//!
//! One [`Committer`] spans validate, assign, append, and fold. The broadcast
//! happens after it is dropped: nothing touches a socket under a lock.

use crate::server::event::{Actor, Frame};
use crate::server::http::{error_response, json_response, Committer, Shared};
use crate::server::page::{cookie_ok, origin_ok_strict};
use crate::server::review::{plan_refs, Review};
use serde::Deserialize;
use std::io::Read;
use std::sync::Arc;
use tiny_http::Request;

/// Reviewer text is bounded so a runaway page cannot fill an append-only log.
/// Generous for a comment; far below anything that would matter for the file.
pub const MAX_TEXT: usize = 64 * 1024;
/// A whole command, before parsing.
const MAX_BODY: usize = 128 * 1024;
const MAX_CLIENT_ID: usize = 128;

#[derive(Debug, Deserialize)]
#[serde(tag = "cmd")]
pub enum Command {
    #[serde(rename = "thread.open")]
    ThreadOpen {
        client_id: String,
        #[serde(rename = "ref")]
        target: String,
        text: String,
        #[serde(default)]
        blocking: bool,
        #[serde(default)]
        quote: String,
        opened_revision: u32,
    },
    #[serde(rename = "thread.reply")]
    ThreadReply {
        client_id: String,
        thread: String,
        text: String,
        opened_revision: u32,
    },
    #[serde(rename = "thread.edit")]
    ThreadEdit {
        client_id: String,
        thread: String,
        text: String,
        opened_revision: u32,
    },
    #[serde(rename = "thread.delete")]
    ThreadDelete { client_id: String, thread: String },
    #[serde(rename = "question.answer")]
    QuestionAnswer {
        client_id: String,
        question: String,
        text: String,
        opened_revision: u32,
    },
    #[serde(rename = "element.reviewed")]
    ElementReviewed {
        client_id: String,
        #[serde(rename = "ref")]
        target: String,
        on: bool,
    },
    #[serde(rename = "chat.send")]
    ChatSend {
        client_id: String,
        text: String,
        #[serde(default)]
        thread: Option<String>,
        opened_revision: u32,
    },
    #[serde(rename = "review.submit")]
    ReviewSubmit {
        client_id: String,
        verdict: String,
        base_revision: u32,
    },
    /// Reviewer activity, no event. Also the page's liveness signal.
    #[serde(rename = "ping")]
    Ping,
}

impl Command {
    fn client_id(&self) -> Option<&str> {
        match self {
            Command::ThreadOpen { client_id, .. }
            | Command::ThreadReply { client_id, .. }
            | Command::ThreadEdit { client_id, .. }
            | Command::ThreadDelete { client_id, .. }
            | Command::QuestionAnswer { client_id, .. }
            | Command::ElementReviewed { client_id, .. }
            | Command::ChatSend { client_id, .. }
            | Command::ReviewSubmit { client_id, .. } => Some(client_id),
            Command::Ping => None,
        }
    }
}

/// What the page gets back. A refusal is a body, not a dropped connection: a
/// page that cannot report an error is a page that silently loses a comment.
fn reply(
    ok: bool,
    client_id: &str,
    assigned: Option<&str>,
    seq: u64,
    error: Option<&str>,
) -> String {
    serde_json::json!({
        "ok": ok,
        "client_id": client_id,
        "assigned": assigned,
        "seq": seq,
        "error": error,
    })
    .to_string()
}

pub fn handle_command(shared: &Arc<Shared>, mut request: Request, artifact: &str) {
    if !cookie_ok(&request, shared) {
        let _ = request.respond(error_response(401, "unauthorized", "no session cookie"));
        return;
    }
    // A page write, so the Origin must be present and exact — not merely
    // absent-or-matching as a top-level navigation may be.
    if !origin_ok_strict(&request, shared.port) {
        let _ = request.respond(error_response(403, "bad_origin", "exact origin required"));
        return;
    }
    if *request.method() != tiny_http::Method::Post {
        let _ = request.respond(error_response(
            405,
            "method_not_allowed",
            "commands are POSTed",
        ));
        return;
    }

    let mut body = String::new();
    if request
        .as_reader()
        .take(MAX_BODY as u64 + 1)
        .read_to_string(&mut body)
        .is_err()
        || body.len() > MAX_BODY
    {
        let _ = request.respond(error_response(
            413,
            "too_large",
            "command body is too large",
        ));
        return;
    }
    let page_id = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("page").and_then(|p| p.as_u64()))
        .unwrap_or(u64::MAX);

    let command: Command = match serde_json::from_str(&body) {
        Ok(c) => c,
        Err(e) => {
            let _ = request.respond(json_response(
                200,
                &reply(false, "", None, 0, Some(&format!("{e}"))),
            ));
            return;
        }
    };

    // Every command counts as reviewer activity, not just `ping`. This clock
    // is separate from the CLI's: an agent polling every 90 seconds is not the
    // reviewer doing anything, and one field for both would mean the idle
    // nudge could never fire while an agent was attached.
    mark_reviewer_activity(shared);

    let outcome = commit_command(shared, artifact, &command);
    match outcome {
        Ok(None) => {
            let _ = request.respond(json_response(200, &reply(true, "", None, 0, None)));
        }
        Ok(Some(done)) => {
            // The gate is released before anything is sent. The originating
            // page already has the result in this response; sending it the
            // broadcast too would make it count the change twice.
            if let Some(frame) = done.frame {
                crate::server::socket::broadcast_except(shared, &frame, page_id);
            }
            let _ = request.respond(json_response(
                200,
                &reply(
                    true,
                    &done.client_id,
                    done.assigned.as_deref(),
                    done.seq,
                    None,
                ),
            ));
        }
        Err(e) => {
            let cid = command.client_id().unwrap_or_default();
            let _ = request.respond(json_response(
                200,
                &reply(false, cid, None, 0, Some(&format!("{e:#}"))),
            ));
        }
    }
}

struct Committed {
    client_id: String,
    assigned: Option<String>,
    seq: u64,
    frame: Option<Frame>,
}

fn mark_reviewer_activity(shared: &Shared) {
    crate::server::presence::on_reviewer_activity(shared, shared.now_ms());
}

/// Validate, assign, append and fold — all under one gate, so two tabs cannot
/// both be told they created `c-1`, and a retry cannot slip between the
/// duplicate check and the append.
fn commit_command(
    shared: &Arc<Shared>,
    artifact: &str,
    command: &Command,
) -> anyhow::Result<Option<Committed>> {
    if matches!(command, Command::Ping) {
        return Ok(None);
    }
    let client_id = command.client_id().unwrap_or_default().to_string();
    if client_id.is_empty() || client_id.len() > MAX_CLIENT_ID {
        anyhow::bail!("every command needs a client_id, so a retry is not a second comment");
    }

    let c = Committer::open(shared);

    // Already done. Answer with the id that command assigned the first time.
    if let Some(previous) = c.with_review(|r| r.committed.get(&client_id).cloned()) {
        return Ok(Some(Committed {
            client_id,
            assigned: previous,
            seq: 0,
            frame: None,
        }));
    }

    let (kind, mut data, assigned) = c.with_review(|review| build(review, artifact, command))?;
    data["client_id"] = serde_json::json!(client_id);
    if kind == "review.submitted" {
        // Ordering, not decoration. The file is written **first**, so the
        // event can name it. The other way round — append, then work out the
        // path, then put it into the event — is not something an append-only
        // log can do, and an earlier draft of this was written that way.
        //
        // `core` is not held here: `with_review` took it and gave it back, and
        // the gate is what makes the read-then-write safe.
        attach_feedback(&c, artifact, &mut data)?;
    }

    let revision = c.with_review(|r| r.artifacts.get(artifact).map(|a| a.revision).unwrap_or(0));
    let event = c.append(artifact, revision, Actor::Reviewer, kind, data)?;
    let seq = event.seq;
    let frame = Frame::of(vec![event]);
    drop(c);

    Ok(Some(Committed {
        client_id,
        assigned,
        seq,
        frame: Some(frame),
    }))
}

/// Build the `artefacto.feedback/1` document from folded state and write it
/// beside the plan, then put both the document and its path into the event.
///
/// The ids in it are the server's, not the page's: spec 6.6 makes them
/// server-assigned and stable, so the document is built from the `Review`
/// rather than from whatever the page believed.
fn attach_feedback(
    c: &Committer,
    artifact: &str,
    data: &mut serde_json::Value,
) -> anyhow::Result<()> {
    let chosen = data
        .get("verdict")
        .and_then(|v| v.as_str())
        .unwrap_or("comment")
        .to_string();
    let base_revision = data
        .get("base_revision")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let (verdict, document, source_path) = c.with_review(|review| {
        let verdict = crate::server::feedback::settle_verdict(review, artifact, &chosen);
        let document = crate::server::feedback::document(review, artifact, &verdict, base_revision);
        let source = review
            .artifacts
            .get(artifact)
            .map(|a| a.source_path.clone())
            .unwrap_or_default();
        (verdict, document, source)
    });

    data["verdict"] = serde_json::json!(verdict);
    data["feedback"] = document.clone();
    // Spec 6.7 writes it "next to the pushed plan file". An artifact with no
    // source path — one seeded straight into the log — has nowhere to put it,
    // and the event then carries the document without a path rather than
    // inventing a location.
    if !source_path.is_empty() {
        let written =
            crate::server::feedback::write(std::path::Path::new(&source_path), &document)?;
        data["path"] = serde_json::json!(written.display().to_string());
    }
    Ok(())
}

type Built = (&'static str, serde_json::Value, Option<String>);

/// Turn a validated command into the event it becomes. Reads the folded state
/// under the gate, so anything it checks is still true when the append lands.
fn build(review: &Review, artifact: &str, command: &Command) -> anyhow::Result<Built> {
    let art = review
        .artifacts
        .get(artifact)
        .ok_or_else(|| anyhow::anyhow!("no such artifact: {artifact}"))?;
    let refs = plan_refs(&art.plan);

    let check_text = |t: &String| -> anyhow::Result<()> {
        if t.len() > MAX_TEXT {
            anyhow::bail!("text is {} bytes; the limit is {MAX_TEXT}", t.len());
        }
        Ok(())
    };
    let check_revision = |r: u32| -> anyhow::Result<()> {
        if r == 0 || r > art.revision {
            anyhow::bail!("opened_revision {r} is not a revision of this artifact");
        }
        Ok(())
    };
    let check_thread = |id: &String| -> anyhow::Result<()> {
        if art.thread(id).is_none() {
            anyhow::bail!("no such thread: {id}");
        }
        Ok(())
    };

    match command {
        Command::ThreadOpen {
            target,
            text,
            blocking,
            quote,
            opened_revision,
            ..
        } => {
            check_text(text)?;
            check_revision(*opened_revision)?;
            if !refs.contains(target) {
                anyhow::bail!("no such element in this revision: {target}");
            }
            // Assigned here, inside the gate. Outside it, two tabs race.
            let id = format!("c-{}", art.next_thread_n);
            Ok((
                "thread.opened",
                serde_json::json!({
                    "thread": id, "ref": target, "text": text,
                    "blocking": blocking, "quote": quote,
                    "opened_revision": opened_revision,
                }),
                Some(id),
            ))
        }
        Command::ThreadReply {
            thread,
            text,
            opened_revision,
            ..
        } => {
            check_text(text)?;
            check_revision(*opened_revision)?;
            check_thread(thread)?;
            Ok((
                "thread.replied",
                serde_json::json!({ "thread": thread, "text": text, "opened_revision": opened_revision }),
                Some(thread.clone()),
            ))
        }
        Command::ThreadEdit {
            thread,
            text,
            opened_revision,
            ..
        } => {
            check_text(text)?;
            check_revision(*opened_revision)?;
            check_thread(thread)?;
            Ok((
                "thread.edited",
                serde_json::json!({ "thread": thread, "text": text, "opened_revision": opened_revision }),
                Some(thread.clone()),
            ))
        }
        Command::ThreadDelete { thread, .. } => {
            check_thread(thread)?;
            Ok((
                "thread.deleted",
                serde_json::json!({ "thread": thread }),
                Some(thread.clone()),
            ))
        }
        Command::QuestionAnswer {
            question,
            text,
            opened_revision,
            ..
        } => {
            check_text(text)?;
            check_revision(*opened_revision)?;
            Ok((
                "question.answered",
                serde_json::json!({ "question": question, "text": text, "opened_revision": opened_revision }),
                None,
            ))
        }
        Command::ElementReviewed { target, on, .. } => {
            if !refs.contains(target) {
                anyhow::bail!("no such element in this revision: {target}");
            }
            Ok((
                "element.reviewed",
                serde_json::json!({ "ref": target, "on": on }),
                None,
            ))
        }
        Command::ChatSend {
            text,
            thread,
            opened_revision,
            ..
        } => {
            check_text(text)?;
            check_revision(*opened_revision)?;
            if let Some(t) = thread {
                check_thread(t)?;
            }
            Ok((
                "chat.sent",
                serde_json::json!({ "text": text, "thread": thread, "opened_revision": opened_revision }),
                thread.clone(),
            ))
        }
        Command::ReviewSubmit {
            verdict,
            base_revision,
            ..
        } => {
            if !matches!(verdict.as_str(), "approve" | "comment" | "request_changes") {
                anyhow::bail!("verdict must be approve, comment, or request_changes");
            }
            check_revision(*base_revision)?;
            Ok((
                "review.submitted",
                serde_json::json!({ "verdict": verdict, "base_revision": base_revision }),
                None,
            ))
        }
        Command::Ping => unreachable!("handled before the gate is opened"),
    }
}
