//! The live state of a review: what the log folds into.
//!
//! Every type here is rebuilt from the log on start, so nothing in it may be
//! the only copy of anything.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Which transport an agent is holding the lease with.
///
/// The distinction decides how liveness is measured, which is why it is
/// recorded rather than inferred:
///
/// - `Live` is `events --follow`: a long-running process holding a connection,
///   so its pid is recorded and a dead pid releases the lease.
/// - `Waiting` is `await`: a short-lived subprocess that exits between polls.
///   **No pid is recorded**, because it would be dead moments later and the
///   token it just handed back would stop validating before `reply` could use
///   it. Liveness for a waiting lease is the TTL alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Live,
    Waiting,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Live => "live",
            Mode::Waiting => "waiting",
        }
    }
}

/// Everything the log says about the current lease, and nothing else.
///
/// There is deliberately **no timestamp here**. When the holder was last heard
/// from is server-local liveness, not history: it lives in `Core.lease_seen_ms`
/// and starts fresh on every restart. A replayed lease therefore gets a full
/// TTL, which is correct — the agent has to call again regardless.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseRecord {
    pub name: String,
    pub generation: u64,
    /// The session token. Never printed by `status`; see `lease::Holder`.
    pub token: String,
    /// Recorded in [`Mode::Live`] only.
    pub pid: Option<u32>,
    pub mode: Mode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThreadStatus {
    Open,
    Changed,
    Declined,
    /// The element this thread was anchored to no longer exists in the current
    /// revision. Never dropped: spec 4.3 requires it be listed instead.
    Unanchored,
}

impl ThreadStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ThreadStatus::Open => "open",
            ThreadStatus::Changed => "changed",
            ThreadStatus::Declined => "declined",
            ThreadStatus::Unanchored => "unanchored",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub actor: String,
    pub text: String,
    pub ts: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thread {
    pub id: String,
    /// The element this thread hangs on, such as `task:t-session-store`.
    pub target: String,
    pub quote: String,
    pub blocking: bool,
    /// Opened by a question to the agent rather than by a comment: the
    /// opening message was delivered as `chat.sent` and answered in place.
    #[serde(default)]
    pub asked: bool,
    pub status: ThreadStatus,
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    pub id: String,
    pub revision: u32,
    /// The whole validated plan of the current revision. Spec 4.2 requires the
    /// log alone to rebuild the page, which a change summary cannot do.
    pub plan: serde_json::Value,
    pub plan_hash: String,
    pub source_path: String,
    /// The change summary of the current revision, for the page's banner
    /// when it learns of a revision from a snapshot rather than a frame.
    pub summary: String,
    pub threads: Vec<Thread>,
    /// Only ever increases. Spec 6.6: ids are stable and never renumbered, so
    /// a deleted `c-1` does not free the number.
    pub next_thread_n: u32,
    pub answers: BTreeMap<String, String>,
    pub reviewed: BTreeSet<String>,
    /// Page-level chat, folded so a restart can rebuild the conversation.
    pub chat: Vec<Message>,
    pub submitted: bool,
    /// The last verdict sent, kept across a new revision: a new revision
    /// reopens the review, and the verdict is still the last one given.
    pub verdict: Option<String>,
    /// When the current revision was published, from its event.
    pub revised_at: String,
}

impl Thread {
    /// Whether resolving this thread as `status` with `note` would change
    /// nothing: the status is already that, and the note is empty or is
    /// already the thread's last message from the agent. A resolution the
    /// agent sends twice — after a crash between resolving and
    /// acknowledging, or a push that repeats resolutions already recorded
    /// — is then not a second note on the reviewer's page. Spec 7: every
    /// handler must be safe to run twice.
    pub fn already_resolved_as(&self, status: &str, note: &str) -> bool {
        if self.status.as_str() != status {
            return false;
        }
        note.is_empty()
            || self
                .messages
                .last()
                .is_some_and(|m| m.actor == "agent" && m.text == note)
    }
}

impl Artifact {
    pub fn thread(&self, id: &str) -> Option<&Thread> {
        self.threads.iter().find(|t| t.id == id)
    }

    pub fn thread_mut(&mut self, id: &str) -> Option<&mut Thread> {
        self.threads.iter_mut().find(|t| t.id == id)
    }
}

#[derive(Debug, Default)]
pub struct Review {
    pub artifacts: BTreeMap<String, Artifact>,
    /// Per agent name. The single representation of a delivery cursor: spec 5
    /// has `status --json` print "each lease's acked_seq", and two fields
    /// holding the same number drift.
    pub cursors: BTreeMap<String, u64>,
    /// Every `client_id` the server has acted on, mapped to whatever id that
    /// command assigned. A map rather than a set, because a retry must be
    /// answered with the **same** assigned id — a set can only say "seen".
    pub committed: BTreeMap<String, Option<String>>,
    pub lease: Option<LeaseRecord>,
    /// Only ever increases, including across a restart, so a token from a
    /// superseded generation can never validate again.
    pub lease_generation: u64,
}

impl Review {
    /// A comparable projection, for asserting that a restart rebuilt exactly
    /// what was there. The token is left out so a snapshot can be printed.
    pub fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({
            "artifacts": self.artifacts,
            "cursors": self.cursors,
            "committed": self.committed,
            "lease_generation": self.lease_generation,
            "lease": self.lease.as_ref().map(|l| serde_json::json!({
                "name": l.name,
                "generation": l.generation,
                "pid": l.pid,
                "mode": l.mode,
            })),
        })
    }
}

/// Every element a thread may anchor to in this plan, as the renderer marks
/// them with `data-plan-ref`: the plan itself (`meta:<id>`), each open
/// question and risk, and each phase and task. Used to decide, after a push,
/// which threads have lost their target, and by ingress to refuse a thread on
/// an element that does not exist. The page computes the same set from the
/// raw plan a revision event carries.
pub fn plan_refs(plan: &serde_json::Value) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    if let Some(id) = plan.pointer("/meta/id").and_then(|i| i.as_str()) {
        out.insert(format!("meta:{id}"));
    }
    for (key, prefix) in [("open_questions", "question"), ("risks", "risk")] {
        for item in plan
            .get(key)
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
        {
            if let Some(id) = item.get("id").and_then(|i| i.as_str()) {
                out.insert(format!("{prefix}:{id}"));
            }
        }
    }
    let Some(phases) = plan.get("phases").and_then(|p| p.as_array()) else {
        return out;
    };
    for phase in phases {
        if let Some(id) = phase.get("id").and_then(|i| i.as_str()) {
            out.insert(format!("phase:{id}"));
        }
        let Some(tasks) = phase.get("tasks").and_then(|t| t.as_array()) else {
            continue;
        };
        for task in tasks {
            if let Some(id) = task.get("id").and_then(|i| i.as_str()) {
                out.insert(format!("task:{id}"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_refs_finds_every_element_the_renderer_marks() {
        let plan = serde_json::json!({
            "meta": { "id": "demo" },
            "open_questions": [{ "id": "q-ttl" }],
            "risks": [{ "id": "r-lock" }],
            "phases": [
                { "id": "p-one", "tasks": [{ "id": "t-a" }, { "id": "t-b" }] },
                { "id": "p-two", "tasks": [] }
            ]
        });
        let refs = plan_refs(&plan);
        for r in [
            "meta:demo",
            "question:q-ttl",
            "risk:r-lock",
            "phase:p-one",
            "phase:p-two",
            "task:t-a",
            "task:t-b",
        ] {
            assert!(
                refs.contains(r),
                "{r}: the page puts a comment button on it"
            );
        }
        assert_eq!(refs.len(), 7);
    }

    #[test]
    fn plan_refs_tolerates_a_shapeless_plan() {
        assert!(plan_refs(&serde_json::json!({})).is_empty());
        assert!(plan_refs(&serde_json::json!({ "phases": "not an array" })).is_empty());
    }
}
