//! Who is here, and who has gone quiet.
//!
//! # Two kinds of event, and the rule that separates them
//!
//! - **Announcements** — `agent.attached`, `agent.detached`, `nudge` — are
//!   delivered to open pages and **never written to the log**. They carry no
//!   state a restart rebuilds, and a logged `agent.attached` would mean the
//!   next server to read that log tells a page an agent is here that left
//!   hours ago. This is the same rule `server.stopping` follows.
//! - **Timer events** — `reviewer.idle`, `reviewer.away`, `reviewer.back` —
//!   are appended, because spec 6.2 makes them active events and an agent
//!   receives them through its cursor, which reads the log.
//!
//! # Two clocks, deliberately separate
//!
//! `last_reviewer_activity_ms` is the reviewer's, marked by ingress on every
//! page command including `ping`. `last_request_at` is the agent's, marked by
//! the HTTP layer. One field for both would mean an `await` long poll — an
//! HTTP request every 90 seconds — counted as the reviewer being busy, so
//! `reviewer.idle` could never fire while an agent was attached.
//!
//! Spec 6.2 also says activity is measured from a throttled page ping "on
//! scroll, keys, pointer, and visibility, not from the last comment, so a
//! reader who reads for twenty minutes is not idle".
//!
//! # Locks
//!
//! [`tick`] decides under `core`, releases it, and only then appends through a
//! `Committer`. The lock order forbids the other way round. Two ticks cannot
//! race — the accept loop is one thread — so deciding and acting in two steps
//! is safe here in a way it is not for the lease.

use crate::server::event::{Actor, Event, Frame};
use crate::server::http::{Committer, Shared};
use crate::server::review::Review;
use std::sync::Arc;
use std::time::Duration;

/// How long a nudge timer waits. `None` is spec 16's `off`.
#[derive(Debug, Clone, Copy)]
pub struct Nudges {
    /// Page open, reviewer quiet for this long.
    pub idle: Option<Duration>,
    /// Every page socket closed for this long, review unsubmitted.
    pub away: Option<Duration>,
}

impl Default for Nudges {
    fn default() -> Nudges {
        Nudges {
            idle: Some(Duration::from_secs(15 * 60)),
            away: Some(Duration::from_secs(5 * 60)),
        }
    }
}

impl Nudges {
    /// Both timers off, for tests and for anyone who wants silence.
    pub fn off() -> Nudges {
        Nudges {
            idle: None,
            away: None,
        }
    }
}

/// The reviewer did something. Called from ingress, on every page command.
///
/// Re-arms the idle nudge, which is what "fires once per quiet period and
/// re-arms after activity" means.
///
/// **The throttle is the page's, not this.** Spec 6.2 measures activity "from
/// a throttled page ping (at most one per 30 seconds)" — so the page decides
/// how often to speak, and the server records every mark it is given. Dropping
/// marks here instead would leave the recorded time up to half a minute stale
/// and fire the idle nudge early.
pub fn on_reviewer_activity(shared: &Shared, now_ms: i64) {
    let mut core = shared.core.lock().unwrap();
    core.last_reviewer_activity_ms = now_ms;
    core.idle_fired = false;
}

/// What the timers decided this tick.
enum Fire {
    Idle,
    Away,
    Back,
}

/// Run the nudge timers. Called from the accept loop's idle branch, four times
/// a second, so no extra thread exists just to watch a clock.
pub fn tick(shared: &Arc<Shared>, now_ms: i64) {
    let pages = crate::server::socket::page_count(shared);
    let fire = {
        let mut core = shared.core.lock().unwrap();
        if pages > 0 {
            // `socket::handle_upgrade` already set both of these when the page
            // arrived; this covers a tick that sees a page it did not watch
            // connect.
            core.page_seen = true;
            core.page_gone_since_ms = None;
            if core.away_fired {
                core.away_fired = false;
                Some(Fire::Back)
            } else {
                idle_due(shared, &mut core, now_ms)
            }
        } else {
            // The away clock starts only once a reviewer has actually been
            // here. Otherwise a server nobody opened would report the reviewer
            // as away five minutes after it started.
            if core.page_seen && core.page_gone_since_ms.is_none() {
                core.page_gone_since_ms = Some(now_ms);
            }
            away_due(shared, &mut core, now_ms)
        }
    };
    let Some(fire) = fire else {
        return;
    };
    let (kind, data) = match fire {
        Fire::Idle => ("reviewer.idle", serde_json::json!({})),
        Fire::Away => ("reviewer.away", serde_json::json!({})),
        Fire::Back => ("reviewer.back", serde_json::json!({})),
    };
    let committer = Committer::open(shared);
    let artifact = committer.with_review(open_artifact);
    match committer.append(&artifact, 0, Actor::Server, kind, data) {
        Ok(event) => {
            drop(committer);
            crate::server::socket::broadcast(shared, &Frame::of(vec![event]));
        }
        Err(e) => eprintln!("artefacto: could not record {kind}: {e:#}"),
    }
}

fn idle_due(
    shared: &Arc<Shared>,
    core: &mut crate::server::http::Core,
    now_ms: i64,
) -> Option<Fire> {
    let window = shared.nudges.idle?.as_millis() as i64;
    if core.idle_fired || now_ms - core.last_reviewer_activity_ms <= window {
        return None;
    }
    core.idle_fired = true;
    Some(Fire::Idle)
}

fn away_due(
    shared: &Arc<Shared>,
    core: &mut crate::server::http::Core,
    now_ms: i64,
) -> Option<Fire> {
    let window = shared.nudges.away?.as_millis() as i64;
    if core.away_fired {
        return None;
    }
    let gone_since = core.page_gone_since_ms?;
    if now_ms - gone_since <= window {
        return None;
    }
    // Spec 6.2: away is "with the review unsubmitted". A finished review is
    // not an abandoned one.
    if !core.review.artifacts.values().any(|a| !a.submitted) {
        return None;
    }
    core.away_fired = true;
    Some(Fire::Away)
}

/// Which artifact a timer event names.
///
/// The envelope needs one, and the condition is about the reviewer rather than
/// about any particular artifact. The most recently revised unsubmitted one is
/// the review in progress, which is what an agent filtering with `--artifact`
/// is waiting on. An empty string when every review is finished.
fn open_artifact(review: &Review) -> String {
    review
        .artifacts
        .values()
        .filter(|a| !a.submitted)
        .max_by_key(|a| a.revision)
        .map(|a| a.id.clone())
        .unwrap_or_default()
}

/// Tell every open page something that is true right now.
///
/// Not logged: see the module docs. The seq is the log's current high-water
/// mark, because the page orders what it receives and this is the newest thing
/// it has been told.
pub fn announce(shared: &Shared, artifact: &str, kind: &str, data: serde_json::Value) {
    let seq = shared.log.lock().unwrap().last_seq();
    let event = Event {
        format: crate::server::event::EVENT_FORMAT.to_string(),
        seq,
        ts: crate::server::log::now_rfc3339(),
        artifact: artifact.to_string(),
        revision: 0,
        actor: Actor::Agent,
        r#type: kind.to_string(),
        data,
        batch: None,
    };
    crate::server::socket::broadcast(shared, &Frame::of(vec![event]));
}

/// Spec 6.3: `agent.attached` / `agent.detached`, with mode `live` or
/// `waiting`. The page's pill reads these; deriving them from the lease is
/// what keeps it from flickering between an agent's poll cycles.
pub fn agent_attached(shared: &Shared, name: &str, mode: crate::server::review::Mode) {
    announce(
        shared,
        "",
        "agent.attached",
        serde_json::json!({ "agent": name, "mode": mode }),
    );
}

pub fn agent_detached(shared: &Shared, name: &str) {
    announce(
        shared,
        "",
        "agent.detached",
        serde_json::json!({ "agent": name }),
    );
}
