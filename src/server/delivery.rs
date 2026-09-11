//! What an agent receives, and when.
//!
//! # There is no passive buffer
//!
//! Spec 6.4 is explicit about it: a frame's contents "are always computed from
//! the cursor, so a passive event cannot be delivered once by a poll and again
//! by the next wake-up". Everything here follows from that. Any change that
//! introduces a second source for a frame's contents is wrong, however
//! convenient it looks.
//!
//! # Two filters, both from real bugs
//!
//! - [`event::is_internal`] keeps control records out. Acknowledging a frame
//!   appends `cursor.acked`; without the filter the agent receives its own
//!   bookkeeping, and in live mode each flush would trigger the next one.
//! - `actor == Agent` keeps the agent from hearing itself. Spec 7 has it
//!   scanning frames for chat it must answer, not for its own output.
//!
//! # The agent says what it has dealt with. The server never guesses.
//!
//! Spec 16 fixes the contract: delivery is **at-least-once**, and "the
//! alternative, at-most-once, silently drops a review when an agent crashes at
//! the wrong moment". So the cursor moves only when the agent acknowledges —
//! by passing the previous result's `seq` back as `--ack` on its next call, or
//! by running `ack --seq N`. A call that acknowledges nothing is handed the
//! same frame again.
//!
//! An earlier version of this file had the server remember what it last
//! handed out and acknowledge it on the session's next call. That reads as
//! convenient and is at-most-once: an agent that receives a frame and then
//! restarts before acting on it calls again with no memory, the server
//! acknowledges the frame on its behalf, and the review it carried is gone.
//! Spec 5's "calling `await` or `events` again acknowledges everything the
//! previous call returned" describes that convenience; spec 16 forbids its
//! consequence, and 16 wins.
//!
//! # Acknowledging is idempotent
//!
//! An `--ack` at or behind the cursor is a no-op, not an error. At-least-once
//! means acknowledgements get repeated — after a retry, after a replay with
//! `--since` — and a repeat that failed the call would turn the safe path
//! into a failed tool call.
//!
//! # Locks
//!
//! [`settle`] and [`ack`] hold a `Committer` across their whole body.
//! `frame_since` takes `log` only. Nothing here holds two guards at once.

use crate::server::event::{is_active, Actor, Event, Frame};
use crate::server::http::{Committer, Shared};
use crate::server::lease;
use crate::server::review::LeaseRecord;
use anyhow::{bail, Result};

/// What one read produced.
#[derive(Debug)]
pub struct Read {
    /// The frame to hand back, or `None` when nothing active is waiting.
    pub frame: Option<Frame>,
    /// The cursor it was read from. `events` and `await` echo this so a
    /// caller can see where it stood.
    pub since: u64,
}

/// Does this event go to an agent at all?
pub fn deliverable(event: &Event) -> bool {
    !crate::server::event::is_internal(&event.r#type) && event.actor != Actor::Agent
}

/// An agent's delivery cursor: the seq it last acknowledged.
pub fn cursor_for(shared: &Shared, name: &str) -> u64 {
    crate::server::http::with_review(shared, |review| {
        review.cursors.get(name).copied().unwrap_or(0)
    })
}

/// Everything deliverable after `cursor`, up to and **including** the first
/// active event.
///
/// `None` when nothing active is waiting: in digest mode passive traffic does
/// not wake the agent, and an empty frame is not a thing spec 6.1 allows.
pub fn frame_since(shared: &Shared, cursor: u64) -> Option<Frame> {
    frame_since_for(shared, cursor, None)
}

/// [`frame_since`], woken only by an active event for `artifact`.
///
/// `--artifact` chooses **what wakes the agent**, not what it is allowed to
/// see. The frame still carries every deliverable event before the one that
/// ended it, whatever artifact those belong to, because the frame's seq is
/// what gets acknowledged: dropping another artifact's events from the middle
/// of a frame would move the cursor past events nobody ever received.
pub fn frame_since_for(shared: &Shared, cursor: u64, artifact: Option<&str>) -> Option<Frame> {
    let pending = pending_since(shared, cursor);
    // Spec 5: "when several active events are waiting, `await` returns at the
    // earliest one, and the frame stops there", so the agent handles events in
    // the order they happened rather than seeing a later one first.
    let stop = pending
        .iter()
        .position(|e| is_active(&e.r#type) && artifact.is_none_or(|a| e.artifact == a))?;
    Some(Frame::of(pending[..=stop].to_vec()))
}

/// The events a `timeout` carries: spec 5's table says the frame then "holds
/// whatever passive events accumulated".
///
/// **Passive only, and it stops at the first active event of any artifact.**
/// A timeout says "nothing actionable"; the agent acknowledges its seq and
/// does nothing else. An active event that rode along in it — another
/// artifact's chat under `--artifact`, or one that landed after the wait
/// decided to give up — would be acknowledged without ever being delivered as
/// what it is. So the tail ends where the first active event begins, and that
/// event is a later call's frame.
pub fn passive_since(shared: &Shared, cursor: u64) -> Vec<Event> {
    let mut pending = pending_since(shared, cursor);
    if let Some(stop) = pending.iter().position(|e| is_active(&e.r#type)) {
        pending.truncate(stop);
    }
    pending
}

fn pending_since(shared: &Shared, cursor: u64) -> Vec<Event> {
    let log = shared.log.lock().unwrap();
    log.since(cursor)
        .iter()
        .filter(|e| deliverable(e))
        .cloned()
        .collect()
}

/// Acknowledge what the agent names, then read the next frame.
///
/// The caller has already taken or refreshed the lease with this token, so the
/// session is known good here; `events` and `await` do that first and exit 6
/// before they ever reach this.
///
/// `ack` is the `seq` of the previous result, if the agent has dealt with it.
/// `since` is a replay cursor from `--since`; it changes where this read
/// starts and nothing else.
pub fn read(
    shared: &Shared,
    session: &LeaseRecord,
    ack: Option<u64>,
    since: Option<u64>,
) -> Result<Read> {
    let cursor = settle(shared, session, ack, since)?;
    Ok(Read {
        frame: frame_since(shared, cursor),
        since: cursor,
    })
}

/// Acknowledge what the agent names, and hand back the cursor the next read
/// starts from.
///
/// Split out of [`read`] because a long poll settles once and then waits: the
/// acknowledgement belongs to the call that is arriving, not to each of the
/// hundred times it checks the log while it waits.
pub fn settle(
    shared: &Shared,
    session: &LeaseRecord,
    ack: Option<u64>,
    since: Option<u64>,
) -> Result<u64> {
    // One gate across acknowledge-then-read, so a retrying CLI cannot have
    // the cursor land between two frames.
    let committer = Committer::open(shared);
    if let Some(seq) = ack {
        ack_under(shared, &committer, &session.name, seq)?;
    }
    Ok(since.unwrap_or_else(|| {
        committer.with_review(|r| r.cursors.get(&session.name).copied().unwrap_or(0))
    }))
}

/// Acknowledge explicitly, as `artefacto ack --seq N --session TOKEN` does.
/// Spec 5 offers this "when an agent wants to acknowledge only part of a
/// frame"; the rest of that frame comes back on the next read.
pub fn ack(shared: &Shared, session: &LeaseRecord, seq: u64) -> Result<()> {
    // Spec 4.2: every agent mutation carries the token, and a superseded one
    // is refused. Validating inside the gate means a takeover cannot land
    // between the check and the append.
    let committer = Committer::open(shared);
    lease::validate(shared, &session.token)?;
    ack_under(shared, &committer, &session.name, seq)
}

/// The cursor move itself. Persisted as a log record because spec 4.2 folds
/// every piece of state from the log — including this one; `is_internal` is
/// what keeps that record out of the stream it describes.
fn ack_under(shared: &Shared, committer: &Committer, name: &str, seq: u64) -> Result<()> {
    let current = committer.with_review(|r| r.cursors.get(name).copied().unwrap_or(0));
    if seq <= current {
        // Already there, or behind it. Idempotent, and it writes nothing:
        // otherwise the log grows by one record per poll cycle, forever, for
        // an agent with nothing to do. Behind the cursor is the ordinary
        // result of `--since` replaying a frame the agent then acknowledges.
        return Ok(());
    }
    let last = { shared.log.lock().unwrap().last_seq() };
    if seq > last {
        bail!("cannot acknowledge seq {seq}; the log ends at {last}");
    }
    committer.append(
        "",
        0,
        Actor::Server,
        "cursor.acked",
        serde_json::json!({ "agent": name, "acked_seq": seq }),
    )?;
    Ok(())
}
