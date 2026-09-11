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
//! # Where the lease fits
//!
//! Every call claims or refreshes the lease first, so a refusal is the first
//! thing that happens rather than something discovered after a 90 second wait.
//! `await` claims [`Mode::Waiting`](crate::server::review::Mode::Waiting);
//! `events --follow` claims [`Mode::Live`](crate::server::review::Mode::Live)
//! and hands over its own pid, so killing it releases the lease at once.

use crate::server::delivery;
use crate::server::event::{await_status, Frame};
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
/// next call, from the cursor this one acknowledged.
const MAX_BACKLOG_FRAMES: usize = 256;

/// `GET /cli/await`. Long-polls for one frame.
pub fn handle_await(shared: &Arc<Shared>, request: Request, query: &Query) {
    let session = match claim(shared, query) {
        Ok(session) => session,
        Err(e) => return refuse(request, e),
    };
    let since = query.get("since").and_then(|s| s.parse::<u64>().ok());
    let artifact = query.get("artifact").filter(|a| !a.is_empty()).cloned();
    let cursor = match delivery::settle(shared, &session, since) {
        Ok(cursor) => cursor,
        Err(e) => return fail(request, &e),
    };
    let deadline = Instant::now() + timeout_of(query);

    loop {
        if let Some(frame) = delivery::frame_since_for(shared, cursor, artifact.as_deref()) {
            delivery::hand_out(shared, &session, frame.seq);
            let status = await_status(&frame.events.last().expect("a frame has events").r#type)
                .expect("a frame always ends at an active event");
            let _ = request.respond(json_response(
                200,
                &result(status, &session, frame.seq, &frame.events).to_string(),
            ));
            return;
        }
        // Spec 5: `stopped` carries "the same partial frame". The shutdown
        // event itself is appended before the flag is set, so the branch above
        // usually answers first; this one catches a wait that began after it.
        let stopping = shared.stopping();
        if stopping || Instant::now() >= deadline {
            let tail = delivery::passive_since(shared, cursor);
            // Spec 6.4: a passive event cannot be "delivered once by a poll and
            // again by the next wake-up", so a timeout that carries them is
            // acknowledged like any other frame.
            if let Some(last) = tail.last() {
                delivery::hand_out(shared, &session, last.seq);
            }
            let seq = tail.last().map(|e| e.seq).unwrap_or(cursor);
            let status = if stopping { "stopped" } else { "timeout" };
            let _ = request.respond(json_response(
                200,
                &result(status, &session, seq, &tail).to_string(),
            ));
            return;
        }
        std::thread::sleep(TICK);
    }
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
    let since = query.get("since").and_then(|s| s.parse::<u64>().ok());
    let artifact = query.get("artifact").filter(|a| !a.is_empty()).cloned();
    let mut cursor = match delivery::settle(shared, &session, since) {
        Ok(cursor) => cursor,
        Err(e) => return fail(request, &e),
    };

    let mut frames: Vec<Frame> = Vec::new();
    while frames.len() < MAX_BACKLOG_FRAMES {
        let Some(frame) = delivery::frame_since_for(shared, cursor, artifact.as_deref()) else {
            break;
        };
        cursor = frame.seq;
        frames.push(frame);
    }
    if let Some(last) = frames.last() {
        delivery::hand_out(shared, &session, last.seq);
    }
    let body = serde_json::json!({
        "ok": true,
        "session": session.token,
        "seq": cursor,
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
    let Some(seq) = query.get("seq").and_then(|s| s.parse::<u64>().ok()) else {
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
fn result(
    status: &str,
    session: &LeaseRecord,
    seq: u64,
    events: &[crate::server::event::Event],
) -> serde_json::Value {
    serde_json::json!({
        "ok": true,
        "status": status,
        "seq": seq,
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

/// Both lease refusals are exit 6 on the agent's side; the body carries which
/// one and, for `held`, who has it.
fn refuse(request: Request, error: LeaseError) {
    let code = match error {
        LeaseError::Held { .. } => "lease_held",
        LeaseError::Superseded => "lease_superseded",
    };
    let _ = request.respond(error_response(409, code, &error.to_string()));
}

fn fail(request: Request, error: &anyhow::Error) {
    let _ = request.respond(error_response(400, "refused", &format!("{error:#}")));
}

fn timeout_of(query: &Query) -> Duration {
    query
        .get("timeout_ms")
        .and_then(|t| t.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_TIMEOUT)
        .min(MAX_TIMEOUT)
}
