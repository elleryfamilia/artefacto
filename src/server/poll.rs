//! The three routes an agent talks to: `await`, `events`, and `ack`.
//!
//! # Responding is this module's job
//!
//! Every function here calls `request.respond` itself. A route that returns a
//! `Response` and drops the `Request` makes `tiny_http` answer 500 on its way
//! past, which is a failure the agent cannot tell from a real one.
//!
//! # The wait is a thread, not a timer
//!
//! `await` blocks its own handler thread until something active arrives or its
//! deadline passes. The accept loop spawns a thread per request, so a hundred
//! concurrent waits cost a hundred threads and block nothing else. The loop
//! wakes every 50 ms and reads an in-memory slice of the log; a condition
//! variable would be tighter, and would be worth it only if a profile said so.
//!
//! # The agent acknowledges; the server does not guess
//!
//! Every call may carry `ack=<seq>`, the `seq` of the previous result. That is
//! the only thing that moves the cursor here. A call without it is handed the
//! same frame again, which is what at-least-once means. See `delivery`.
//!
//! # Where the lease fits
//!
//! Every call claims or refreshes the lease first, so a refusal is the first
//! thing that happens rather than something discovered after a 90 second wait.
//! `await` claims [`Mode::Waiting`](crate::server::review::Mode::Waiting);
//! `events --follow` claims [`Mode::Live`](crate::server::review::Mode::Live)
//! and hands over its own pid, so killing it releases the lease at once.

use crate::server::delivery;
use crate::server::event::{await_status, Event, Frame};
use crate::server::http::{error_response, json_response, Query, Shared};
use crate::server::lease::{self, Claim, LeaseError};
use crate::server::review::LeaseRecord;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tiny_http::Request;

/// Spec 5: "The default timeout is **90 seconds**", safely inside every
/// harness command limit we know of, and the skill re-arms it.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(90);
/// A caller cannot ask the server to hold a connection open forever.
const MAX_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// How often a waiting call looks at the log.
const TICK: Duration = Duration::from_millis(50);
/// Frames one `events` backlog response will carry. The rest arrive on the
/// next call.
const MAX_BACKLOG_FRAMES: usize = 256;

/// `GET /cli/await`. Long-polls for one frame.
pub fn handle_await(shared: &Arc<Shared>, request: Request, query: &Query) {
    let session = match claim(shared, query) {
        Ok(session) => session,
        Err(e) => return refuse(request, e),
    };
    let artifact = query.get("artifact").filter(|a| !a.is_empty()).cloned();
    let cursor = match delivery::settle(
        shared,
        &session,
        seq_of(query, "ack"),
        seq_of(query, "since"),
    ) {
        Ok(cursor) => cursor,
        Err(e) => return fail(request, &e),
    };
    let deadline = Instant::now() + timeout_of(query);

    loop {
        // The holder is provably here — its request is open — and spec 4.2
        // says any agent call refreshes the TTL, so a long poll refreshes on
        // every tick: a wait longer than the TTL must not expire the lease it
        // is holding. The same check ends the wait the moment the lease
        // changes hands, with the refusal, rather than handing a frame to a
        // token that can no longer act on it.
        if let Err(e) = lease::validate(shared, &session.token) {
            return refuse(request, e);
        }
        if let Some(frame) = delivery::frame_since_for(shared, cursor, artifact.as_deref()) {
            return answer(request, &session, cursor, &frame);
        }
        let stopping = shared.stopping();
        if stopping || Instant::now() >= deadline {
            // One last look before giving up. An active event that landed
            // between the check above and here is this call's frame; swept
            // into a timeout instead, it would be acknowledged as "nothing
            // actionable" and never delivered as what it is.
            if let Some(frame) = delivery::frame_since_for(shared, cursor, artifact.as_deref()) {
                return answer(request, &session, cursor, &frame);
            }
            // Spec 5: `timeout` and `stopped` carry "whatever passive events
            // accumulated" — and only those. The tail stops before the first
            // active event of any artifact, so acknowledging it can never skip
            // one.
            let tail = delivery::passive_since(shared, cursor);
            let seq = tail.last().map(|e| e.seq).unwrap_or(cursor);
            let status = if stopping { "stopped" } else { "timeout" };
            let _ = request.respond(json_response(
                200,
                &result(status, &session, seq, cursor, &tail).to_string(),
            ));
            return;
        }
        std::thread::sleep(TICK);
    }
}

fn answer(request: Request, session: &LeaseRecord, cursor: u64, frame: &Frame) {
    let status = await_status(&frame.events.last().expect("a frame has events").r#type)
        .expect("a frame always ends at an active event");
    let _ = request.respond(json_response(
        200,
        &result(status, session, frame.seq, cursor, &frame.events).to_string(),
    ));
}

/// `GET /cli/events`. The backlog, as frames, without waiting.
///
/// `events --follow` is not this route: it is a loop of `await` calls, which
/// gives the same one-frame-per-line stdout with no streaming response body.
/// See `commands::agent`.
pub fn handle_events(shared: &Arc<Shared>, request: Request, query: &Query) {
    let session = match claim(shared, query) {
        Ok(session) => session,
        Err(e) => return refuse(request, e),
    };
    let artifact = query.get("artifact").filter(|a| !a.is_empty()).cloned();
    let mut cursor = match delivery::settle(
        shared,
        &session,
        seq_of(query, "ack"),
        seq_of(query, "since"),
    ) {
        Ok(cursor) => cursor,
        Err(e) => return fail(request, &e),
    };

    // Where this call started reading: the acknowledged cursor, or `since`.
    let settled = cursor;
    let mut frames: Vec<Frame> = Vec::new();
    while frames.len() < MAX_BACKLOG_FRAMES {
        let Some(frame) = delivery::frame_since_for(shared, cursor, artifact.as_deref()) else {
            break;
        };
        cursor = frame.seq;
        frames.push(frame);
    }
    let body = serde_json::json!({
        "ok": true,
        "session": session.token,
        "agent": session.name,
        "seq": cursor,
        "cursor": settled,
        "frames": frames,
    });
    let _ = request.respond(json_response(200, &body.to_string()));
}

/// `POST /cli/ack`. Spec 5: acknowledge part of a frame explicitly.
pub fn handle_ack(shared: &Arc<Shared>, request: Request, query: &Query) {
    let Some(token) = query.get("session") else {
        let _ = request.respond(error_response(
            400,
            "usage",
            "ack needs --session; the token is the agent's identity",
        ));
        return;
    };
    let Some(seq) = seq_of(query, "seq") else {
        let _ = request.respond(error_response(400, "usage", "ack needs a numeric --seq"));
        return;
    };
    let session = match lease::validate(shared, token) {
        Ok(session) => session,
        Err(e) => return refuse(request, e),
    };
    match delivery::ack(shared, &session, seq) {
        Ok(()) => {
            let body = serde_json::json!({
                "ok": true,
                "session": session.token,
                "seq": delivery::cursor_for(shared, &session.name),
            });
            let _ = request.respond(json_response(200, &body.to_string()));
        }
        Err(e) => fail(request, &e),
    }
}

/// Spec 5: the result carries `status`, `seq`, `events`, and `session`.
/// `cursor` is where this call started reading — the acknowledged position,
/// or `--since` — which the `events` session record reports as the agent's
/// position; `seq` is the acknowledgement point for what this result holds.
fn result(
    status: &str,
    session: &LeaseRecord,
    seq: u64,
    cursor: u64,
    events: &[Event],
) -> serde_json::Value {
    serde_json::json!({
        "ok": true,
        "status": status,
        "seq": seq,
        "cursor": cursor,
        // Spec 5: "await and events return it in their result as session".
        // An agent takes it from the first call and passes it to every
        // mutation until a call hands back a new one.
        "session": session.token,
        "agent": session.name,
        "events": events,
    })
}

/// Take the lease, or refresh the one the caller already holds.
///
/// Shared with `push`, which claims it the same way: an agent's first command
/// is often a push, so push has to be able to take a lease and hand back the
/// token rather than requiring one it has no way to have yet.
pub fn claim(shared: &Arc<Shared>, query: &Query) -> Result<LeaseRecord, LeaseError> {
    let name = query.get("agent").map(String::as_str).unwrap_or("agent");
    let live = query.get("mode").map(String::as_str) == Some("live");
    let pid = query.get("pid").and_then(|p| p.parse::<u32>().ok());
    let claim = match (live, pid) {
        // A `--follow` agent is a durable process, so its pid is what releases
        // the lease the moment it is killed.
        (true, Some(pid)) => Claim::live(name, pid),
        _ => Claim::waiting(name),
    };
    lease::acquire(
        shared,
        claim
            .with_token(query.get("session").map(String::as_str))
            .with_takeover(query.get("takeover").map(String::as_str) == Some("1")),
    )
}

/// A held or superseded lease is exit 6 on the agent's side; a name that could not be handed back is exit 2; the body carries which
/// one and, for `held`, who has it.
fn refuse(request: Request, error: LeaseError) {
    let (status, code) = error.http();
    let _ = request.respond(error_response(status, code, &error.to_string()));
}

fn fail(request: Request, error: &anyhow::Error) {
    let _ = request.respond(error_response(400, "refused", &format!("{error:#}")));
}

fn seq_of(query: &Query, key: &str) -> Option<u64> {
    query.get(key).and_then(|s| s.parse::<u64>().ok())
}

fn timeout_of(query: &Query) -> Duration {
    query
        .get("timeout_ms")
        .and_then(|t| t.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_TIMEOUT)
        .min(MAX_TIMEOUT)
}
