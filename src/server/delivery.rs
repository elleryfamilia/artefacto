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
//! # The offer, and why the server has to remember it
//!
//! Spec 5: "Calling `await` or `events` again acknowledges everything the
//! previous call returned." The thing calling again is a **fresh CLI process
//! that knows only its session token** — it cannot tell the server which frame
//! to acknowledge, because it was not there when the frame was handed out. So
//! the server records what it last offered, against the token it offered it
//! to, and settles it on the next call. See [`offer`].
//!
//! The mark is keyed by **token**, not by agent name. Keyed by name, a
//! takeover would acknowledge a frame the new agent never saw, losing it
//! outright — which is worse than the duplicate that at-least-once trades for.
//!
//! It lives in memory and not in the log, so a server restart drops it and the
//! frame is delivered a second time. That is the documented bargain: spec 5
//! says an agent "sees that frame again rather than losing it, so every
//! handler must be safe to run twice".
//!
//! # Locks
//!
//! [`offer`] and [`ack`] hold a `Committer` across their whole body, so the
//! implicit acknowledgement and the frame that follows it cannot be split.
//! `frame_since` takes `log` only. Nothing here holds two guards at once.

use crate::server::event::{is_active, Actor, Event, Frame};
use crate::server::http::{Committer, Shared};
use crate::server::lease;
use crate::server::review::LeaseRecord;
use anyhow::{bail, Result};

/// The last frame handed to one session, waiting to be acknowledged by that
/// session's next call.
#[derive(Debug, Clone)]
pub struct Offer {
    pub token: String,
    pub seq: u64,
}

/// What one delivery attempt produced.
#[derive(Debug)]
pub struct Offered {
    /// The frame to hand back, or `None` when nothing active is waiting.
    pub frame: Option<Frame>,
    /// The cursor it was computed from. `events` and `await` echo this so a
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
/// They are acknowledged along with it, because spec 6.4 requires that a
/// passive event cannot be "delivered once by a poll and again by the next
/// wake-up".
pub fn passive_since(shared: &Shared, cursor: u64) -> Vec<Event> {
    pending_since(shared, cursor)
}

fn pending_since(shared: &Shared, cursor: u64) -> Vec<Event> {
    let log = shared.log.lock().unwrap();
    log.since(cursor)
        .iter()
        .filter(|e| deliverable(e))
        .cloned()
        .collect()
}

/// Settle the previous frame for this session, then compute the next one.
///
/// The caller has already taken or refreshed the lease with this token, so the
/// session is known good here; `events` and `await` do that first and exit 6
/// before they ever reach this.
///
/// `since` is the caller's own cursor, from `--since`. Naming one means the
/// agent is driving its own bookkeeping, so the outstanding offer is dropped
/// rather than acknowledged: `events --since 0` must not quietly acknowledge
/// the frame it is replaying past.
pub fn offer(shared: &Shared, session: &LeaseRecord, since: Option<u64>) -> Result<Offered> {
    let cursor = settle(shared, session, since)?;
    let frame = frame_since(shared, cursor);
    if let Some(f) = &frame {
        hand_out(shared, session, f.seq);
    }
    Ok(Offered {
        frame,
        since: cursor,
    })
}

/// Acknowledge whatever the previous call to this session returned, and hand
/// back the cursor the next read starts from.
///
/// Split out of [`offer`] because a long poll settles once and then waits: the
/// acknowledgement belongs to the call that is arriving, not to each of the
/// hundred times it checks the log while it waits.
pub fn settle(shared: &Shared, session: &LeaseRecord, since: Option<u64>) -> Result<u64> {
    // One gate across settle-then-read, so a retrying CLI cannot have the
    // cursor land between two frames.
    let committer = Committer::open(shared);
    // Any call from this session invalidates whatever was outstanding, whether
    // or not it is about to be acknowledged.
    let outstanding = take_offer(shared, &session.token);
    match since {
        Some(named) => Ok(named),
        None => {
            if let Some(seq) = outstanding {
                ack_under(shared, &committer, &session.name, seq)?;
            }
            Ok(committer.with_review(|r| r.cursors.get(&session.name).copied().unwrap_or(0)))
        }
    }
}

/// Remember that everything up to `seq` was handed to this session, so its
/// next call acknowledges it.
pub fn hand_out(shared: &Shared, session: &LeaseRecord, seq: u64) {
    shared.core.lock().unwrap().last_offer = Some(Offer {
        token: session.token.clone(),
        seq,
    });
}

/// Acknowledge explicitly, as `artefacto ack --seq N --session TOKEN` does.
///
/// Spec 5 offers this "when an agent wants to acknowledge only part of a
/// frame", so it also cancels the outstanding offer: the rest of that frame
/// has to come back rather than be swallowed by the next call's implicit
/// acknowledgement.
pub fn ack(shared: &Shared, session: &LeaseRecord, seq: u64) -> Result<()> {
    // Spec 4.2: every agent mutation carries the token, and a superseded one
    // is refused. Validating inside the gate means a takeover cannot land
    // between the check and the append.
    let committer = Committer::open(shared);
    lease::validate(shared, &session.token)?;
    take_offer(shared, &session.token);
    ack_under(shared, &committer, &session.name, seq)
}

/// Take and clear whatever this session had outstanding.
fn take_offer(shared: &Shared, token: &str) -> Option<u64> {
    let mut core = shared.core.lock().unwrap();
    core.last_offer
        .take()
        .filter(|o| o.token == token)
        .map(|o| o.seq)
}

/// The cursor move itself. Persisted as a log record because spec 4.2 folds
/// every piece of state from the log — including this one; `is_internal` is
/// what keeps that record out of the stream it describes.
fn ack_under(shared: &Shared, committer: &Committer, name: &str, seq: u64) -> Result<()> {
    let current = committer.with_review(|r| r.cursors.get(name).copied().unwrap_or(0));
    if seq < current {
        bail!("the cursor for {name} is at {current}; it does not move back to {seq}");
    }
    if seq == current {
        // An idempotent ack writes nothing. Otherwise the log grows by one
        // record per poll cycle, forever, for an agent with nothing to do.
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
