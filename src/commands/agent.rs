//! `await`, `events`, and `ack`: the three commands an agent runs.
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
//! # Exit codes are the interface
//!
//! Spec 5: `await` "always exits 0 when the server answered", because agents
//! treat a non-zero exit as a failed tool call rather than as "poll again".
//! Only a real error is non-zero: 4 when there is no server, 6 when the lease
//! is held or the token was superseded.

use crate::cli::{AckArgs, AwaitArgs, EventsArgs, ReplyArgs, ResolveArgs};
use crate::client::Client;
use crate::commands::Exit;
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
        &args.artifact,
        &args.session,
        args.takeover,
    );

    // The absolute deadline the reconnect rule is measured against: the wait
    // the caller asked for, plus room for one reconnect to complete.
    let deadline = Instant::now() + args.timeout + SLACK;
    let result = client.call_until("GET", "await", &query, deadline)?;
    println!("{result}");
    Ok(())
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
        &args.artifact,
        &args.session,
        args.takeover,
    );

    let result = client.call("GET", "events", &query, Duration::from_secs(30))?;
    let frames = result["frames"].as_array().cloned().unwrap_or_default();
    for frame in frames {
        println!("{frame}");
    }
    Ok(())
}

/// Stay attached: one NDJSON frame per line, flushed, until the server stops.
fn follow(client: &Client, args: &EventsArgs) -> Result<()> {
    let mut session = args.session.clone();
    let mut takeover = args.takeover;
    let mut since = args.since;
    let pid = std::process::id().to_string();

    loop {
        let mut query = vec![
            ("agent", args.agent.clone()),
            ("timeout_ms", FOLLOW_POLL.as_millis().to_string()),
            // Spec 4.2: a `--follow` agent is a durable process, so its pid is
            // recorded and a dead one releases the lease at once.
            ("mode", "live".to_string()),
            ("pid", pid.clone()),
        ];
        push_common(&mut query, since, &args.artifact, &session, takeover);
        // The cursor is the server's from here on: `--since` applies to the
        // first poll only, or every poll would replay from the same place.
        since = None;
        // And the lease is ours from here on, so a later poll must not try to
        // take it from whoever we handed it to.
        takeover = false;

        let deadline = Instant::now() + FOLLOW_POLL + SLACK;
        let result = match client.call_until("GET", "await", &query, deadline) {
            Ok(result) => result,
            // The server went away without saying so. For a command whose job
            // is to stay attached until the server stops, that *is* the stop.
            Err(e) if session.is_some() && e.downcast_ref::<Exit>().is_none() => return Ok(()),
            Err(e) => return Err(e),
        };
        session = result["session"].as_str().map(str::to_string);

        let status = result["status"].as_str().unwrap_or("timeout");
        let events = result["events"].as_array().cloned().unwrap_or_default();
        if !events.is_empty() {
            let frame = serde_json::json!({
                "format": crate::server::event::FRAME_FORMAT,
                "seq": result["seq"],
                "events": events,
            });
            println!("{frame}");
            // A monitoring agent reads this pipe as it is written, so a frame
            // that sits in a buffer is a frame that has not arrived.
            std::io::stdout().flush()?;
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
    artifact: &Option<String>,
    session: &Option<String>,
    takeover: bool,
) {
    if let Some(since) = since {
        query.push(("since", since.to_string()));
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
