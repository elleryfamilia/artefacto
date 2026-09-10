//! The wire envelope. Spec section 6.1.

use serde::{Deserialize, Serialize};

pub const EVENT_FORMAT: &str = "artefacto.event/1";
pub const FRAME_FORMAT: &str = "artefacto.frame/1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Actor {
    Reviewer,
    Agent,
    Server,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frame {
    pub format: String,
    pub seq: u64,
    pub events: Vec<Event>,
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
        }
    }
}

/// Active events wake the agent; passive ones ride along with the next active
/// one in digest mode. Spec 6.2 — including `reviewer.back`, which is active.
pub fn is_active(event_type: &str) -> bool {
    matches!(
        event_type,
        "chat.sent"
            | "review.submitted"
            | "reviewer.idle"
            | "reviewer.away"
            | "reviewer.back"
            | "server.stopping"
    )
}

/// Control records the server writes so its own state folds from the log.
/// They are never delivered to an agent or a page. Without this, acknowledging
/// a frame appends a record that is itself delivered in the next frame.
pub fn is_internal(event_type: &str) -> bool {
    matches!(
        event_type,
        "cursor.acked" | "lease.taken" | "lease.released"
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
