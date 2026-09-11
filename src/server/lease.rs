//! One agent acts at a time. The **token**, not the process, is the identity.
//!
//! Spec 4.2 puts it this way: the token exists "so a lease survives an `await`
//! returning every 90 seconds instead of dropping and re-taking on every
//! cycle". A poll-mode agent is a series of short subprocesses, and what holds
//! them together is the string it passes back on the next call.
//!
//! # The one critical section
//!
//! [`acquire`] decides, mints the token, appends and folds inside a single
//! [`Committer`]. That is the whole design, and it is a review finding rather
//! than a preference: with the decision taken outside the gate, two callers
//! both read generation 0, both compute 1, and the loser is handed a token
//! that was already superseded — returned as `Ok`, so nothing reports it.
//!
//! `std::sync::Mutex` is not reentrant, so the public functions here never
//! call one another. The logic lives in `*_locked` helpers that take the
//! guard's contents; the public function locks once, calls them, and appends.
//!
//! # Two clocks, and only one of them is history
//!
//! What the log holds is [`LeaseRecord`]: name, generation, token, mode, pid.
//! When the holder was last heard from is **not** in there. It is
//! `Core.lease_seen_ms`, monotonic milliseconds since this server started, and
//! it resets on a restart — so a replayed lease gets a fresh TTL, which is
//! right, because the agent has to call again regardless.
//!
//! # Liveness, decided per mode
//!
//! Spec 4.2 says an expired lease "or one whose recorded pid is dead" is
//! released. That rule describes `events --follow`, a long-running process
//! holding a connection. It cannot describe `await`, which exits between
//! polls: its pid is dead moments after it returns, and a pid check would stop
//! its own token validating before `reply` could use it.
//!
//! - [`Mode::Live`] records a pid. A dead pid releases the lease at once.
//! - [`Mode::Waiting`] records none. The TTL alone measures it.
//!
//! # Who counts as a second agent
//!
//! The lease **name**. Spec 4.2 refuses "a second agent" with exit 6, and a
//! caller under the holder's own name is not a second one: it is the same
//! agent that has lost track of its token, which happens whenever `push` runs
//! after an `await` in a different shell. It rejoins and gets the same token.
//! A different name is refused, and the refusal says who holds it.
//!
//! # Expiry is a release
//!
//! Spec 4.2: "an expired lease, or one whose recorded pid is dead, is released
//! by the server." Released means the token is dead: `status` says no agent,
//! the page's pill says no agent, and a write with that token is refused. An
//! earlier version let the holder revive an expired lease by presenting its
//! token, which was friendlier and made those three disagree with each other.
//! A holder that comes back after five quiet minutes claims again under its
//! own name and gets a fresh token; its cursor is keyed by name, so it loses
//! nothing but the string.

use crate::server::event::Actor;
use crate::server::http::{constant_time_eq, Committer, Core, Shared};
use crate::server::review::{LeaseRecord, Mode};
use crate::server::state_dir;
use serde::Serialize;
use std::time::Duration;

const TTL_SECS: u64 = 300;
/// Five minutes of silence from the holder, after which another agent may take
/// the lease without `--takeover`.
pub const TTL: Duration = Duration::from_secs(TTL_SECS);
const TTL_MS: i64 = (TTL_SECS * 1000) as i64;

/// Why a lease call was refused. Both map to exit code 6.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseError {
    /// Another agent holds it. Spec 4.2: the refusal names the current holder
    /// and its age, so the user is never left guessing.
    Held { holder: String, age_secs: u64 },
    /// The presented token is not the current one. A takeover, a release, or a
    /// dead `--follow` process has happened since it was minted.
    Superseded,
}

impl std::fmt::Display for LeaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LeaseError::Held { holder, age_secs } => write!(
                f,
                "the lease is held by {holder}, last seen {age_secs}s ago; \
                 pass --takeover to take it over"
            ),
            LeaseError::Superseded => write!(
                f,
                "this session token is no longer valid; another agent took the \
                 lease, or it was released"
            ),
        }
    }
}

impl std::error::Error for LeaseError {}

/// What the server says about the lease in public.
///
/// Deliberately has no `token` field. Spec 5 says `status --json` never prints
/// the session token, and a type that cannot carry one cannot leak it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Holder {
    pub agent: String,
    pub generation: u64,
    pub mode: Mode,
    pub pid: Option<u32>,
    /// Seconds since the holder was last heard from.
    pub age_secs: u64,
    /// Spec 5: `status --json` prints each lease's `acked_seq`.
    pub acked_seq: u64,
}

/// What an agent is asking for.
#[derive(Debug, Clone)]
pub struct Claim<'a> {
    pub name: &'a str,
    pub mode: Mode,
    /// Recorded in [`Mode::Live`] only.
    pub pid: Option<u32>,
    /// The token the caller already holds, if any. This one field is what
    /// turns a refusal into a refresh.
    pub presenting: Option<&'a str>,
    pub takeover: bool,
}

impl<'a> Claim<'a> {
    /// An `await` caller: a short-lived subprocess, so no pid.
    pub fn waiting(name: &'a str) -> Claim<'a> {
        Claim {
            name,
            mode: Mode::Waiting,
            pid: None,
            presenting: None,
            takeover: false,
        }
    }

    /// An `events --follow` caller, which stays alive for as long as it holds
    /// the lease and so can be checked for.
    pub fn live(name: &'a str, pid: u32) -> Claim<'a> {
        Claim {
            name,
            mode: Mode::Live,
            pid: Some(pid),
            presenting: None,
            takeover: false,
        }
    }

    pub fn with_token(mut self, token: Option<&'a str>) -> Claim<'a> {
        self.presenting = token;
        self
    }

    pub fn with_takeover(mut self, takeover: bool) -> Claim<'a> {
        self.takeover = takeover;
        self
    }
}

/// The lease as the log records it, unless its recorded process is gone.
///
/// A dead pid is the one thing that kills a lease outright: spec 4.2 releases
/// such a lease, and the process that would have presented the token no longer
/// exists to present it.
fn recorded_locked(core: &Core) -> Option<&LeaseRecord> {
    core.review
        .lease
        .as_ref()
        .filter(|l| l.pid.is_none_or(state_dir::is_alive))
}

fn age_secs_locked(core: &Core, now_ms: i64) -> u64 {
    now_ms.saturating_sub(core.lease_seen_ms).max(0) as u64 / 1000
}

fn expired_locked(core: &Core, now_ms: i64) -> bool {
    now_ms.saturating_sub(core.lease_seen_ms) > TTL_MS
}

/// The lease another agent has to wait for: recorded, alive, and inside its
/// TTL. This is also what presence and self-exit read.
fn blocking_locked(core: &Core, now_ms: i64) -> Option<&LeaseRecord> {
    recorded_locked(core).filter(|_| !expired_locked(core, now_ms))
}

fn holds(record: &LeaseRecord, token: &str) -> bool {
    constant_time_eq(record.token.as_bytes(), token.as_bytes())
}

/// Who holds the lease right now, for presence and for `status --json`.
///
/// `None` when nobody does, when the holder's `--follow` process died, or when
/// the TTL ran out.
pub fn current(shared: &Shared) -> Option<Holder> {
    let now = shared.now_ms();
    let core = shared.core.lock().unwrap();
    let record = blocking_locked(&core, now)?;
    Some(Holder {
        agent: record.name.clone(),
        generation: record.generation,
        mode: record.mode,
        pid: record.pid,
        age_secs: age_secs_locked(&core, now),
        acked_seq: core
            .review
            .cursors
            .get(&record.name)
            .copied()
            .unwrap_or_default(),
    })
}

/// What [`acquire`] decided, worked out under the guard and acted on after it.
enum Decision {
    /// The holder called again and nothing about the lease changed. Touch the
    /// clock; write nothing.
    Refresh(LeaseRecord),
    /// The holder called again having changed transport. Same token, same
    /// generation, new mode or pid — which the presence pill reads, so it is
    /// state and goes through the log.
    Relog(LeaseRecord),
    /// A new lease at this generation. Carries the outgoing holder's name, if
    /// there was one, so the new agent can inherit its delivery cursor.
    Fresh {
        generation: u64,
        outgoing: Option<String>,
    },
}

/// Take the lease, refresh it, or be refused.
///
/// Everything happens inside one [`Committer`]: reading the current lease,
/// choosing the next generation, minting the token, appending and folding. Two
/// callers therefore cannot both compute the same generation.
///
/// Safe to call while already holding a `Committer`? No — it opens its own.
pub fn acquire(shared: &Shared, claim: Claim) -> Result<LeaseRecord, LeaseError> {
    let committer = Committer::open(shared);
    let now = shared.now_ms();

    let decision = {
        let core = shared.core.lock().unwrap();
        decide_locked(&core, now, &claim)?
    };

    let record = match decision {
        Decision::Refresh(record) => record,
        Decision::Relog(record) => {
            append_taken(&committer, &record)?;
            record
        }
        Decision::Fresh {
            generation,
            outgoing,
        } => {
            let record = LeaseRecord {
                name: claim.name.to_string(),
                generation,
                token: mint(generation),
                pid: match claim.mode {
                    Mode::Live => claim.pid,
                    // No durable process to record; see the module docs.
                    Mode::Waiting => None,
                },
                mode: claim.mode,
            };
            append_taken(&committer, &record)?;
            inherit_cursor(&committer, claim.name, outgoing.as_deref())?;
            record
        }
    };

    shared.core.lock().unwrap().lease_seen_ms = now;
    // Presence is not announced from here. `presence::tick` derives it from
    // the lease on every tick, so a takeover, a release, an expiry and a dead
    // `--follow` all reach the page the same way.
    Ok(record)
}

fn decide_locked(core: &Core, now_ms: i64, claim: &Claim) -> Result<Decision, LeaseError> {
    let pid = match claim.mode {
        Mode::Live => claim.pid,
        // No durable process to record; see the module docs.
        Mode::Waiting => None,
    };

    if let Some(token) = claim.presenting {
        // The holder calling again. Expired means released (module docs), so
        // the token has to name a lease that is still live.
        match blocking_locked(core, now_ms).filter(|r| holds(r, token)) {
            Some(record) => return Ok(rejoin(record, claim.mode, pid)),
            // A dead token: expired, released, or taken over. Spec 4.2 refuses
            // "unless it passes --takeover", so a takeover falls through and
            // claims fresh, exactly as it would with no token at all. Without
            // this, an agent that paused past the TTL and retried with both
            // its token and --takeover was refused, and only dropping the
            // token worked — which no agent would guess.
            None if !claim.takeover => return Err(LeaseError::Superseded),
            None => {}
        }
    }

    if !claim.takeover {
        if let Some(held) = blocking_locked(core, now_ms) {
            // Spec 4.2 refuses "a second agent". The lease name is what says
            // which agent this is, so a caller under the holder's own name is
            // not a second one — it is the same agent that has lost track of
            // its token, which is the ordinary case for `push` run after an
            // `await` in another shell. It rejoins the lease it already holds
            // and gets the same token back.
            //
            // This does not weaken the token rule: there is still exactly one
            // live token, and anyone reaching this code already holds the
            // bearer secret from a file only the user can read.
            if held.name != claim.name {
                return Err(LeaseError::Held {
                    holder: held.name.clone(),
                    age_secs: age_secs_locked(core, now_ms),
                });
            }
            return Ok(rejoin(held, claim.mode, pid));
        }
    }
    Ok(Decision::Fresh {
        generation: core.review.lease_generation + 1,
        // The record, not `blocking_locked`: an expired or crashed holder is
        // still the agent whose cursor the new one should pick up.
        outgoing: core.review.lease.as_ref().map(|l| l.name.clone()),
    })
}

/// Keep the lease as it stands, re-recording it only when the transport
/// changed — which the presence pill reads, so it is state and is logged.
///
/// A `Waiting` claim never demotes a `Live` lease. A `push` or an `await` from
/// the same agent while its `events --follow` is running is a poll beside the
/// follow, not a change of transport; recording it as `waiting` would drop the
/// pid that releases the lease when the follow dies, and flip the pill twice
/// per push as the follow's next poll flipped it back.
fn rejoin(record: &LeaseRecord, mode: Mode, pid: Option<u32>) -> Decision {
    let (mode, pid) = match (record.mode, mode) {
        (Mode::Live, Mode::Waiting) => (Mode::Live, record.pid),
        _ => (mode, pid),
    };
    if record.mode == mode && record.pid == pid {
        Decision::Refresh(record.clone())
    } else {
        Decision::Relog(LeaseRecord {
            mode,
            pid,
            ..record.clone()
        })
    }
}

/// `<generation>.<256 random bits>`. Spec 4.2 calls this "a session token
/// carrying a generation number"; the generation is in the clear because it is
/// not a secret, and the random half is what makes the token unguessable.
fn mint(generation: u64) -> String {
    format!("{generation}.{}", state_dir::new_secret())
}

/// Lease events are server-wide, so they name no artifact and no revision.
fn append_taken(committer: &Committer, record: &LeaseRecord) -> Result<(), LeaseError> {
    log_or_refuse(committer.append(
        "",
        0,
        Actor::Server,
        "lease.taken",
        serde_json::json!({
            "agent": record.name,
            "generation": record.generation,
            "token": record.token,
            "mode": record.mode,
            "pid": record.pid,
        }),
    ))
}

/// Delivery cursors are keyed by agent name. Without this, a takeover under a
/// different name starts from that name's cursor — usually zero — and replays
/// every passive event the previous agent already acknowledged.
///
/// A name that already has a cursor keeps it: that is a genuine resume, and
/// inheriting would skip whatever it had not seen.
fn inherit_cursor(
    committer: &Committer,
    name: &str,
    outgoing: Option<&str>,
) -> Result<(), LeaseError> {
    let Some(outgoing) = outgoing.filter(|o| *o != name) else {
        return Ok(());
    };
    let inherited = committer.with_review(|review| {
        if review.cursors.contains_key(name) {
            return None;
        }
        review.cursors.get(outgoing).copied().filter(|s| *s > 0)
    });
    let Some(seq) = inherited else {
        return Ok(());
    };
    log_or_refuse(committer.append(
        "",
        0,
        Actor::Server,
        "cursor.acked",
        serde_json::json!({ "agent": name, "acked_seq": seq }),
    ))
}

/// A lease that could not be written is a lease nobody holds. Reporting it as
/// `Superseded` sends the agent down the path that is already correct for a
/// token it cannot use, and the log's own error is on `server.log`.
fn log_or_refuse<T>(result: anyhow::Result<T>) -> Result<(), LeaseError> {
    match result {
        Ok(_) => Ok(()),
        Err(e) => {
            eprintln!("artefacto: could not record a lease change: {e:#}");
            Err(LeaseError::Superseded)
        }
    }
}

/// Confirm a token still names the current lease, and refresh the TTL.
///
/// Spec 4.2: "Every agent mutation carries the token... A token from a
/// superseded generation is refused", and "any agent call refreshes it".
///
/// Takes `core` only, so it is safe to call while holding a [`Committer`] —
/// which is where a mutation should call it, so that validating and appending
/// cannot be split by a takeover landing in between.
pub fn validate(shared: &Shared, token: &str) -> Result<LeaseRecord, LeaseError> {
    let now = shared.now_ms();
    let mut core = shared.core.lock().unwrap();
    let record = blocking_locked(&core, now)
        .filter(|r| holds(r, token))
        .cloned()
        .ok_or(LeaseError::Superseded)?;
    core.lease_seen_ms = now;
    Ok(record)
}

/// Give the lease up. Spec 4.2: "a `--follow` disconnect releases it
/// immediately."
///
/// Only the holder can do it: a release from anything but the current token is
/// ignored, so a stale process shutting down cannot evict whoever took over
/// from it.
pub fn release(shared: &Shared, token: &str) {
    let committer = Committer::open(shared);
    let record = {
        let core = shared.core.lock().unwrap();
        // Not `recorded_locked`: a `--follow` process that has already died is
        // exactly the case this needs to clear.
        core.review
            .lease
            .as_ref()
            .filter(|r| holds(r, token))
            .cloned()
    };
    let Some(record) = record else {
        return;
    };
    // The generation is deliberately not lowered anywhere: a released token
    // must never validate again.
    let _ = log_or_refuse(committer.append(
        "",
        0,
        Actor::Server,
        "lease.released",
        serde_json::json!({ "agent": record.name, "generation": record.generation }),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_carries_its_generation_and_256_unguessable_bits() {
        let token = mint(7);
        let (generation, random) = token.split_once('.').expect("<generation>.<random>");
        assert_eq!(generation, "7");
        assert_eq!(random.len(), 64, "256 bits of hex");
        assert_ne!(mint(7), token, "two tokens of one generation still differ");
    }

    #[test]
    fn a_refusal_says_who_holds_it_and_how_to_proceed() {
        let held = LeaseError::Held {
            holder: "claude".to_string(),
            age_secs: 12,
        }
        .to_string();
        assert!(held.contains("claude"), "{held}");
        assert!(held.contains("12s"), "{held}");
        assert!(held.contains("--takeover"), "{held}");
    }
}
