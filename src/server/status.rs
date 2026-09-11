//! `status --json`: the review as an agent needs to see it to rejoin.
//!
//! Spec 5 lists what it prints: port, artifacts, revisions, open and
//! unanchored threads, the last event sequence, each lease's `acked_seq`, the
//! lease holder and its age, reviewer presence, and the exact `events
//! --follow` command line for the skill to arm. It never prints the session
//! token — `Holder` has no field for one — so nothing that only reads status
//! can pick up a credential.
//!
//! # One moment
//!
//! The artifacts and `last_seq` are read under the commit gate, as `/state`
//! reads them for the page, so the two describe the same instant. An agent
//! that rejoins with `--since <last_seq>` after reading this must not miss an
//! event that landed between two separate reads.
//!
//! # Locks
//!
//! `commit`, then `core` briefly inside it, then `log`; after the gate is
//! released, `core` again for the lease and the reviewer clocks, and
//! `sockets` for the page count. Never two of them at once except in the
//! documented order.

use crate::server::http::{Committer, Shared};
use crate::server::review::{Artifact, ThreadStatus};
use std::path::Path;
use std::sync::Arc;

/// The default lease name, and the one the follow line names when nobody
/// holds the lease. Must match the `--agent` default in `cli.rs`.
pub const DEFAULT_AGENT: &str = "agent";

pub fn status_json(shared: &Arc<Shared>) -> serde_json::Value {
    let (artifacts, cursors, last_seq) = {
        let committer = Committer::open(shared);
        let (artifacts, cursors) = committer.with_review(|review| {
            let artifacts: Vec<serde_json::Value> =
                review.artifacts.values().map(artifact_json).collect();
            (artifacts, review.cursors.clone())
        });
        let last_seq = shared.log.lock().unwrap().last_seq();
        (artifacts, cursors, last_seq)
    };
    let lease = crate::server::lease::current(shared);
    let agent = lease
        .as_ref()
        .map(|h| h.agent.clone())
        .unwrap_or_else(|| DEFAULT_AGENT.to_string());
    let reviewer = reviewer_json(shared);
    serde_json::json!({
        "ok": true,
        "port": shared.port,
        "last_seq": last_seq,
        "artifacts": artifacts,
        "lease": lease,
        "cursors": cursors,
        "reviewer": reviewer,
        "follow": follow_json(&agent),
    })
}

/// One artifact, with the counts spec 5 names and the thread list the
/// skill's rule 3 needs: an agent about to answer a redelivered chat frame
/// looks at `last_actor` before replying a second time.
fn artifact_json(artifact: &Artifact) -> serde_json::Value {
    let count = |status: ThreadStatus| {
        artifact
            .threads
            .iter()
            .filter(|t| t.status == status)
            .count()
    };
    let threads: Vec<serde_json::Value> = artifact
        .threads
        .iter()
        .map(|t| {
            serde_json::json!({
                "id": t.id,
                "ref": t.target,
                "status": t.status.as_str(),
                "blocking": t.blocking,
                "messages": t.messages.len(),
                "last_actor": t.messages.last().map(|m| m.actor.as_str()),
            })
        })
        .collect();
    serde_json::json!({
        "id": artifact.id,
        "kind": artifact.id.split_once(':').map(|(kind, _)| kind).unwrap_or_default(),
        "title": artifact.plan.pointer("/meta/title").and_then(|t| t.as_str()).unwrap_or_default(),
        "revision": artifact.revision,
        "plan_hash": artifact.plan_hash,
        "source_path": artifact.source_path,
        "feedback_path": crate::server::feedback::feedback_path(Path::new(&artifact.source_path)),
        "submitted": artifact.submitted,
        "open_threads": count(ThreadStatus::Open),
        "unanchored_threads": count(ThreadStatus::Unanchored),
        "blocking_threads": artifact
            .threads
            .iter()
            .filter(|t| t.blocking && t.status == ThreadStatus::Open)
            .count(),
        "threads": threads,
        "chat": artifact.chat.len(),
        "answers": artifact.answers.values().filter(|a| !a.is_empty()).count(),
        "reviewed": artifact.reviewed.len(),
    })
}

/// Spec 5's "reviewer presence": how many pages are open, whether one has
/// ever been, how long since the reviewer did anything, and which nudges have
/// fired. `present` is what the agent branches on; the rest says why.
fn reviewer_json(shared: &Shared) -> serde_json::Value {
    let pages = crate::server::socket::page_count(shared);
    let now = shared.now_ms();
    let core = shared.core.lock().unwrap();
    let last_activity_secs = core
        .page_seen
        .then(|| (now - core.last_reviewer_activity_ms).max(0) as u64 / 1000);
    serde_json::json!({
        "pages": pages,
        "present": pages > 0,
        "seen": core.page_seen,
        "last_activity_secs": last_activity_secs,
        "idle": core.idle_fired,
        "away": core.away_fired,
    })
}

/// The line the skill arms after a push, under the name that holds the lease
/// so the follow rejoins it. `argv` is the same words, unquoted, for a
/// harness that takes a list; `command` is one a person can paste.
fn follow_json(agent: &str) -> serde_json::Value {
    let argv = ["artefacto", "events", "--follow", "--agent", agent];
    let command = argv
        .iter()
        .map(|word| shell_word(word))
        .collect::<Vec<_>>()
        .join(" ");
    serde_json::json!({
        "agent": agent,
        "argv": argv,
        "command": command,
    })
}

/// Quote a word for a POSIX shell only when it needs it. An agent name is
/// the one word here a person chooses, and one with a space in it must reach
/// the follow as one argument.
pub fn shell_word(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '/' | '@'));
    if plain {
        return word.to_string();
    }
    format!("'{}'", word.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_name_is_not_quoted() {
        assert_eq!(shell_word("claude"), "claude");
        assert_eq!(shell_word("agent-2.0"), "agent-2.0");
    }

    #[test]
    fn a_name_a_shell_would_split_or_expand_is_single_quoted() {
        assert_eq!(shell_word("my agent"), "'my agent'");
        assert_eq!(shell_word("$HOME"), "'$HOME'");
        assert_eq!(shell_word("it's"), "'it'\\''s'");
        assert_eq!(shell_word(""), "''");
    }

    #[test]
    fn the_follow_line_names_the_holder() {
        let follow = follow_json("claude");
        assert_eq!(
            follow["command"],
            "artefacto events --follow --agent claude"
        );
        assert_eq!(follow["argv"][4], "claude");
    }
}
