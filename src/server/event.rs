//! The wire envelope. Spec section 6.1.

use serde::{Deserialize, Serialize};

pub const EVENT_FORMAT: &str = "artefacto.event/1";
pub const FRAME_FORMAT: &str = "artefacto.frame/1";
/// The first line `events` prints: the session token and where the agent
/// stands. NDJSON has no envelope to carry them in, and spec 5 says `events`
/// returns the token "in its result as session".
pub const SESSION_FORMAT: &str = "artefacto.session/1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Actor {
    Reviewer,
    Agent,
    Server,
}

/// Framing for a group of events that must land together or not at all.
///
/// One `write_all` is **not** a transaction: a crash can leave a prefix of the
/// group on disk with a clean final newline, and the log's torn-tail rule then
/// accepts half a commit as history. This mark is what makes the difference
/// visible — a log whose last record says it is 1 of 3 was interrupted, and
/// the whole group is dropped.
///
/// `id` is not strictly needed to find the group, since its members are
/// contiguous by construction. It is here so recovery can **check** that
/// rather than assume it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchMark {
    pub id: String,
    /// 0-based position within the group.
    pub index: u32,
    /// How many records the group has. Always two or more.
    pub count: u32,
}

impl BatchMark {
    /// Is this the record that completes its group?
    pub fn is_last(&self) -> bool {
        self.index + 1 >= self.count
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub format: String,
    pub seq: u64,
    pub ts: String,
    pub artifact: String,
    pub revision: u32,
    pub actor: Actor,
    /// `type` is a Rust keyword; the wire name is plain `type`.
    #[serde(rename = "type")]
    pub r#type: String,
    pub data: serde_json::Value,
    /// Present only on a record that is part of a multi-event commit.
    /// `default` so every line written before this existed still parses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch: Option<BatchMark>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frame {
    pub format: String,
    pub seq: u64,
    pub events: Vec<Event>,
    /// The rendered body, on the frame the socket delivers for a push. Spec
    /// 4.3 wants the page to receive "one snapshot holding the rendered body,
    /// thread state, and resolutions together", and the events alone carry
    /// the raw plan. Never on an agent's frame: those are built from the log,
    /// and the log holds the plan, not its rendering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
}

impl Frame {
    /// A frame's own `seq` is the acknowledgement point, so it is the seq of
    /// the last event in it — the one that caused the frame to be sent.
    pub fn of(events: Vec<Event>) -> Self {
        let seq = events.last().map(|e| e.seq).unwrap_or(0);
        Frame {
            format: FRAME_FORMAT.to_string(),
            seq,
            events,
            html: None,
        }
    }
}

/// The `await` status an event produces, or `None` when it is passive.
///
/// One table for both questions, so they cannot drift: spec 6.2's active list
/// and spec 5's `await` status table are the same set seen from two sides, and
/// an active event with no status would silently be reported as a timeout.
///
/// Spec 5's table is missing a `back` row; spec 6.2 lists `reviewer.back` as
/// active. This follows 6.2, and the spec needs the row added.
pub fn await_status(event_type: &str) -> Option<&'static str> {
    match event_type {
        "chat.sent" => Some("chat"),
        "review.submitted" => Some("submitted"),
        "reviewer.idle" => Some("idle"),
        "reviewer.away" => Some("away"),
        "reviewer.back" => Some("back"),
        "server.stopping" => Some("stopped"),
        _ => None,
    }
}

/// Active events wake the agent; passive ones ride along with the next active
/// one in digest mode. Spec 6.2 — including `reviewer.back`, which is active.
pub fn is_active(event_type: &str) -> bool {
    await_status(event_type).is_some()
}

/// Control records the server writes so its own state folds from the log.
/// They are never delivered to an agent or a page. Without this, acknowledging
/// a frame appends a record that is itself delivered in the next frame.
pub fn is_internal(event_type: &str) -> bool {
    matches!(
        event_type,
        "cursor.acked" | "lease.taken" | "lease.released" | "log.cleaned"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seq: u64, kind: &str) -> Event {
        Event {
            format: EVENT_FORMAT.to_string(),
            seq,
            ts: "2026-09-06T16:02:11Z".to_string(),
            artifact: "plan:auth-refactor".to_string(),
            revision: 3,
            actor: Actor::Reviewer,
            r#type: kind.to_string(),
            data: serde_json::Value::Null,
            batch: None,
        }
    }

    #[test]
    fn an_event_serializes_to_the_spec_envelope() {
        let mut e = at(42, "thread.replied");
        e.data = serde_json::json!({ "thread": "c-3", "ref": "task:t-session-store" });
        let v: serde_json::Value = serde_json::to_value(&e).unwrap();
        assert_eq!(v["format"], "artefacto.event/1");
        assert_eq!(v["seq"], 42);
        assert_eq!(v["actor"], "reviewer");
        assert_eq!(
            v["type"], "thread.replied",
            "the wire field is `type`, not `r#type`"
        );
        assert_eq!(v["data"]["thread"], "c-3");
    }

    #[test]
    fn an_event_round_trips() {
        let json = r#"{"format":"artefacto.event/1","seq":7,"ts":"2026-09-06T16:02:11Z",
            "artifact":"plan:x","revision":1,"actor":"agent","type":"revision.published",
            "data":{}}"#;
        let e: Event = serde_json::from_str(json).unwrap();
        assert_eq!(e.seq, 7);
        assert!(matches!(e.actor, Actor::Agent));
        assert_eq!(e.r#type, "revision.published");
    }

    #[test]
    fn a_frame_names_its_last_event_as_the_ack_point() {
        let f = Frame::of(vec![
            at(4, "thread.opened"),
            at(5, "thread.opened"),
            at(9, "chat.sent"),
        ]);
        assert_eq!(f.seq, 9, "a frame's seq is its last event's seq");
        assert_eq!(f.format, "artefacto.frame/1");
    }

    #[test]
    fn active_and_passive_match_spec_6_2() {
        // Spec 6.2 lists reviewer.back under Active. An earlier draft of this
        // plan put it in the passive list and asserted that; the test passed
        // and the classification was still wrong.
        for t in [
            "chat.sent",
            "review.submitted",
            "reviewer.idle",
            "reviewer.away",
            "reviewer.back",
            "server.stopping",
        ] {
            assert!(is_active(t), "{t} is active in spec 6.2");
        }
        for t in [
            "thread.opened",
            "thread.replied",
            "thread.edited",
            "thread.deleted",
            "question.answered",
            "element.reviewed",
        ] {
            assert!(!is_active(t), "{t} is passive in spec 6.2");
        }
    }

    #[test]
    fn every_active_event_has_an_await_status() {
        // Spec 5 returns a status per active event. Without this, adding an
        // active type and forgetting its status makes it arrive as `timeout`,
        // which reads to an agent as "nothing happened".
        for t in [
            "chat.sent",
            "review.submitted",
            "reviewer.idle",
            "reviewer.away",
            "reviewer.back",
            "server.stopping",
        ] {
            assert!(await_status(t).is_some(), "{t} needs an await status");
        }
        assert_eq!(await_status("thread.opened"), None);
        assert_eq!(
            await_status("reviewer.back"),
            Some("back"),
            "the row spec 5's table is missing"
        );
    }

    #[test]
    fn control_records_are_internal_and_never_active() {
        assert!(is_internal("cursor.acked"));
        assert!(
            !is_active("cursor.acked"),
            "an internal record must not wake anyone"
        );
        for t in ["chat.sent", "thread.opened", "revision.published"] {
            assert!(!is_internal(t), "{t} is part of the protocol");
        }
    }
}
