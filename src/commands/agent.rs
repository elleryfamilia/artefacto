//! `await`, `events`, `ack`, `reply`, and `resolve`: the commands an agent runs.
//!
//! # Two transports, one contract
//!
//! Spec 6.5 gives agents two ways to hear things. Both produce the same
//! frames from the same cursor:
//!
//! - `await` is one long poll per call, re-armed by the skill. Its result is
//!   one JSON object on stdout.
//! - `events --follow` is a loop of long polls that prints one NDJSON frame
//!   per line and flushes each one, for agents that can watch a running
//!   command.
//!
//! **`--follow` is a loop, not a streamed response body.** Streaming would
//! mean a chunked `tiny_http` response fed by a pipe, whose flushing behaviour
//! is not ours to control, for no gain: a poll that returns the moment
//! something happens and is immediately re-issued delivers the same frames
//! with the same latency, over machinery that is already tested. What the
//! stream would have given — "a `--follow` disconnect releases the lease
//! immediately" — comes instead from `Mode::Live` recording the process's pid,
//! which the server checks on every read of the lease.
//!
//! # The agent acknowledges; nothing else moves the cursor
//!
//! Spec 16: delivery is at-least-once. So `await --ack <seq>` passes the
//! previous result's `seq` back once the agent has acted on it, and that is
//! what moves the cursor. A call without `--ack` is handed the same frame
//! again. In monitor mode the agent runs `ack --seq <seq>` after acting; the
//! follow itself never acknowledges, and advances only its own read position.
//!
//! # Where the token comes from
//!
//! Spec 5: `await` and `events` "return it in their result as session". For
//! `await` it is a field. `events` prints NDJSON, which has no envelope, so
//! its first line is an `artefacto.session/1` record carrying the token and
//! the cursor; the frames follow. Without that line a monitor-mode agent had
//! no token to `reply` with.
//!
//! # Exit codes are the interface
//!
//! Spec 5: `await` "always exits 0 when the server answered", because agents
//! treat a non-zero exit as a failed tool call rather than as "poll again".
//! Only a real error is non-zero: 4 when there is no server, 6 when the lease
//! is held or the token was superseded.

use crate::cli::{AckArgs, AwaitArgs, EventsArgs, ReplyArgs, ResolveArgs};
use crate::client::Client;
use crate::commands::Exit;
use crate::server::event::{FRAME_FORMAT, SESSION_FORMAT};
use anyhow::Result;
use std::io::Write;
use std::time::{Duration, Instant};

/// Slack on top of the server's own deadline, so the client's read timeout
/// never fires first and turns a clean `timeout` into a reconnect.
const SLACK: Duration = Duration::from_secs(10);

pub fn await_cmd(args: &AwaitArgs) -> Result<()> {
    let client = Client::connect()?;
    let mut query = vec![
        ("agent", args.agent.clone()),
        ("timeout_ms", args.timeout.as_millis().to_string()),
    ];
    push_common(
        &mut query,
        args.since,
        args.ack,
        &args.artifact,
        &args.session,
        args.takeover,
    );

    // The absolute deadline the reconnect rule is measured against: the wait
    // the caller asked for, plus room for one reconnect to complete.
    let deadline = Instant::now() + args.timeout + SLACK;
    let result = match client.call_until("GET", "await", &query, deadline) {
        Ok(result) => result,
        Err(e) if e.downcast_ref::<Exit>().is_none() => unreachable_timeout(args, &e),
        Err(e) => return Err(e),
    };
    println!("{result}");
    Ok(())
}

/// Spec 5: `await` "retries against the same cursor until its absolute
/// deadline, then returns `timeout`". A server that never came back is a
/// timeout, not a failed tool call; the agent's next call finds no server and
/// exits 4, which it can branch on. `seq` is what a live server's empty
/// timeout would carry — the cursor unchanged, which is `--since` when one
/// was passed — so passing it back is a no-op.
fn unreachable_timeout(args: &AwaitArgs, error: &anyhow::Error) -> serde_json::Value {
    let cursor = args.since.or(args.ack).unwrap_or(0);
    serde_json::json!({
        "ok": true,
        "status": "timeout",
        "seq": cursor,
        "cursor": cursor,
        "session": args.session,
        "agent": args.agent,
        "events": [],
        "unreachable": format!("{error:#}"),
    })
}

pub fn events(args: &EventsArgs) -> Result<()> {
    let client = Client::connect()?;
    if args.follow {
        return follow(&client, args);
    }
    let mut query = vec![("agent", args.agent.clone())];
    push_common(
        &mut query,
        args.since,
        args.ack,
        &args.artifact,
        &args.session,
        args.takeover,
    );

    let result = client.call("GET", "events", &query, Duration::from_secs(30))?;
    println!("{}", session_line(&result));
    for frame in result["frames"].as_array().cloned().unwrap_or_default() {
        println!("{frame}");
    }
    Ok(())
}

/// Spec 5: the session record carries "the session token and the cursor".
/// The cursor, not the result's `seq`: a follow's first poll may already
/// hold a frame, and its `seq` would then name events the agent has not
/// acted on. An agent that read that as its position and acknowledged it
/// would skip them.
fn session_line(result: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "format": SESSION_FORMAT,
        "session": result["session"],
        "agent": result["agent"],
        "seq": result["cursor"],
    })
}

/// Stay attached: one NDJSON frame per line, flushed, until the server stops.
fn follow(client: &Client, args: &EventsArgs) -> Result<()> {
    let mut session = args.session.clone();
    let mut takeover = args.takeover;
    let mut ack = args.ack;
    // The follow's own read position. It advances as frames are printed and
    // never touches the persisted cursor: that moves only when the agent runs
    // `ack --seq`, after acting. A follow that restarts therefore replays what
    // was printed but never acknowledged — at-least-once, as spec 16 requires
    // — and a monitor that never acknowledges pays only in replay.
    let mut since = args.since;
    let mut announced = false;
    let pid = std::process::id().to_string();
    // The first poll returns at once. It exists to take the lease and hand the
    // token over — a monitoring agent needs it to `reply` — and a first poll
    // that waited its full minute would hold that token back for as long.
    let mut timeout = Duration::ZERO;

    loop {
        let mut query = vec![
            ("agent", args.agent.clone()),
            ("timeout_ms", timeout.as_millis().to_string()),
            // Spec 4.2: a `--follow` agent is a durable process, so its pid is
            // recorded and a dead one releases the lease at once.
            ("mode", "live".to_string()),
            ("pid", pid.clone()),
        ];
        push_common(&mut query, since, ack, &args.artifact, &session, takeover);
        // `--ack` and `--takeover` apply to the first poll only.
        ack = None;
        takeover = false;

        let deadline = Instant::now() + timeout + SLACK;
        timeout = FOLLOW_POLL;
        let result = match client.call_until("GET", "await", &query, deadline) {
            Ok(result) => result,
            // The server went away without saying so. For a command whose job
            // is to stay attached until the server stops, that *is* the stop.
            Err(e) if session.is_some() && e.downcast_ref::<Exit>().is_none() => return Ok(()),
            Err(e) => return Err(e),
        };
        session = result["session"].as_str().map(str::to_string);
        if !announced {
            println!("{}", session_line(&result));
            std::io::stdout().flush()?;
            announced = true;
        }

        let status = result["status"].as_str().unwrap_or("timeout");
        let events = result["events"].as_array().cloned().unwrap_or_default();
        // Digest, spec 6.4: "a passive event does not by itself cause a
        // frame". `timeout` and `stopped` carry whatever passive events
        // accumulated, and waking a monitoring agent for them costs a model
        // turn that buys nothing. They are not printed, and the read position
        // stays put, so they ride along in the next frame that does wake it.
        // A stop is signalled by this process exiting 0 — consistently, never
        // sometimes by a frame that ends at a passive event.
        if !matches!(status, "timeout" | "stopped") && !events.is_empty() {
            let frame = serde_json::json!({
                "format": FRAME_FORMAT,
                "seq": result["seq"],
                "events": events,
            });
            println!("{frame}");
            // A monitoring agent reads this pipe as it is written, so a frame
            // that sits in a buffer is a frame that has not arrived.
            std::io::stdout().flush()?;
            since = result["seq"].as_u64();
        }
        if status == "stopped" {
            return Ok(());
        }
    }
}

/// One poll's wait while following. Shorter than `await`'s default because
/// nothing re-arms this loop but itself, so a shorter poll only costs one
/// loopback round trip and keeps the lease's TTL well refreshed.
const FOLLOW_POLL: Duration = Duration::from_secs(60);

pub fn ack_cmd(args: &AckArgs) -> Result<()> {
    let client = Client::connect()?;
    let query = vec![
        ("seq", args.seq.to_string()),
        ("session", args.session.clone()),
    ];
    let result = client.call("POST", "ack", &query, Duration::from_secs(30))?;
    println!("{result}");
    Ok(())
}

fn push_common(
    query: &mut Vec<(&'static str, String)>,
    since: Option<u64>,
    ack: Option<u64>,
    artifact: &Option<String>,
    session: &Option<String>,
    takeover: bool,
) {
    if let Some(since) = since {
        query.push(("since", since.to_string()));
    }
    if let Some(ack) = ack {
        query.push(("ack", ack.to_string()));
    }
    if let Some(artifact) = artifact {
        query.push(("artifact", artifact.clone()));
    }
    if let Some(session) = session {
        query.push(("session", session.clone()));
    }
    if takeover {
        query.push(("takeover", "1".to_string()));
    }
}

/// Answer the reviewer. Spec 5: `reply` needs either a thread or an artifact,
/// and `--artifact` may be omitted when the server has exactly one.
pub fn reply(args: &ReplyArgs) -> Result<()> {
    let text = match (&args.text, args.stdin) {
        (Some(text), _) => text.clone(),
        (None, true) => {
            let mut buffer = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut buffer)?;
            buffer.trim_end_matches('\n').to_string()
        }
        (None, false) => unreachable!("clap requires text or --stdin"),
    };
    let client = Client::connect()?;
    let mut query = vec![("session", args.session.clone())];
    if let Some(thread) = &args.thread {
        query.push(("thread", thread.clone()));
    }
    if let Some(artifact) = &args.artifact {
        query.push(("artifact", artifact.clone()));
    }
    if args.nudge {
        query.push(("nudge", "1".to_string()));
    }
    let result = client.call_body("POST", "reply", &query, &text, Duration::from_secs(30))?;
    println!("{result}");
    Ok(())
}

/// Mark a thread addressed or declined, with a note the reviewer reads in the
/// thread. Spec 6.3.
pub fn resolve(args: &ResolveArgs) -> Result<()> {
    let client = Client::connect()?;
    let status = if args.changed { "changed" } else { "declined" };
    let mut query = vec![
        ("session", args.session.clone()),
        ("thread", args.thread.clone()),
        ("status", status.to_string()),
    ];
    if let Some(artifact) = &args.artifact {
        query.push(("artifact", artifact.clone()));
    }
    let note = args.note.clone().unwrap_or_default();
    let result = client.call_body("POST", "resolve", &query, &note, Duration::from_secs(30))?;
    println!("{result}");
    Ok(())
}
