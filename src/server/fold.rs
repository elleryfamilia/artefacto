//! The log, folded into live state.
//!
//! A pure function over a slice: no locks, no I/O, no clock beyond the event's
//! own timestamp. `Shared::new` calls it once with `log.since(0)`, and
//! [`apply`] runs for each newly appended event so the in-memory `Review`
//! stays exactly what a fresh fold would produce.
//!
//! # The invariant
//!
//! **`fold(all_events)` equals the result of `apply`-ing those events one at a
//! time, in log order.** Everything else here serves that.
//!
//! It is why appending and folding cannot be two separate critical sections.
//! Appending under the log lock, releasing it, then folding under `core` lets
//! two threads interleave — A appends, B appends and folds, A folds — and the
//! in-memory state stops matching the log. See `http::Committer`, which is the
//! only way to append.

use crate::server::event::{Actor, Event};
use crate::server::review::{
    plan_refs, Artifact, LeaseRecord, Message, Mode, Review, Thread, ThreadStatus,
};

pub fn fold(events: &[Event]) -> Review {
    let mut review = Review::default();
    for event in events {
        apply(&mut review, event);
    }
    review
}

pub fn apply(review: &mut Review, event: &Event) {
    // Recorded first and unconditionally, so a duplicate is suppressed whether
    // or not the specific handler cares about it.
    if let Some(cid) = event.data.get("client_id").and_then(|v| v.as_str()) {
        let assigned = event
            .data
            .get("thread")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        review.committed.insert(cid.to_string(), assigned);
    }

    match event.r#type.as_str() {
        "revision.published" => revision(review, event),
        "thread.opened" => thread_opened(review, event),
        "thread.replied" => thread_message(review, event),
        "thread.edited" => thread_edited(review, event),
        "thread.deleted" => thread_deleted(review, event),
        "thread.resolved" => thread_resolved(review, event),
        "question.answered" => answered(review, event),
        "element.reviewed" => reviewed(review, event),
        "chat.sent" => chat(review, event),
        "review.submitted" => submitted(review, event),
        "cursor.acked" => cursor(review, event),
        "lease.taken" => lease_taken(review, event),
        "lease.released" => lease_released(review, event),
        // Presence and nudges are delivered, not folded: they carry no state a
        // restart needs to rebuild.
        _ => {}
    }
}

fn artifact_mut<'a>(review: &'a mut Review, event: &Event) -> Option<&'a mut Artifact> {
    review.artifacts.get_mut(&event.artifact)
}

fn str_field(event: &Event, key: &str) -> String {
    event
        .data
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

fn message(event: &Event) -> Message {
    Message {
        actor: match event.actor {
            Actor::Reviewer => "reviewer",
            Actor::Agent => "agent",
            Actor::Server => "server",
        }
        .to_string(),
        text: str_field(event, "text"),
        ts: event.ts.clone(),
    }
}

fn revision(review: &mut Review, event: &Event) {
    let plan = event
        .data
        .get("plan")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let entry = review
        .artifacts
        .entry(event.artifact.clone())
        .or_insert_with(|| Artifact {
            id: event.artifact.clone(),
            next_thread_n: 1,
            ..Artifact::default()
        });
    entry.revision = entry.revision.saturating_add(1);
    entry.plan_hash = str_field(event, "plan_hash");
    entry.source_path = str_field(event, "source_path");
    entry.summary = str_field(event, "summary");
    entry.plan = plan;
    // A new revision reopens the review. Spec 7 rule 4 has the agent push the
    // next revision after a submit "and keep the monitor armed for the next
    // round"; a review that stayed submitted would never be away again and
    // would name no artifact for its timer events.
    entry.submitted = false;

    // Re-anchoring is a fold concern, not a push concern: a push is the only
    // thing that can change which elements exist, and every thread has to be
    // re-checked against the new set.
    let refs = plan_refs(&entry.plan);
    for thread in &mut entry.threads {
        let anchored = refs.contains(&thread.target);
        match (anchored, thread.status) {
            (false, ThreadStatus::Open) => thread.status = ThreadStatus::Unanchored,
            // A thread whose target came back is open again; the agent may
            // have restored the element it was hanging on.
            (true, ThreadStatus::Unanchored) => thread.status = ThreadStatus::Open,
            _ => {}
        }
    }
}

fn thread_opened(review: &mut Review, event: &Event) {
    let id = str_field(event, "thread");
    let target = str_field(event, "ref");
    let msg = message(event);
    let Some(artifact) = artifact_mut(review, event) else {
        return;
    };
    // The server assigns ids, so the counter follows whatever the log says
    // rather than the other way round. `max` keeps it monotonic even if the
    // log is replayed out of the order it was written.
    if let Some(n) = id.strip_prefix("c-").and_then(|n| n.parse::<u32>().ok()) {
        artifact.next_thread_n = artifact.next_thread_n.max(n + 1);
    }
    if artifact.thread(&id).is_some() {
        return;
    }
    artifact.threads.push(Thread {
        id,
        target,
        quote: str_field(event, "quote"),
        blocking: event
            .data
            .get("blocking")
            .and_then(|b| b.as_bool())
            .unwrap_or(false),
        status: ThreadStatus::Open,
        messages: vec![msg],
    });
}

fn thread_message(review: &mut Review, event: &Event) {
    let id = str_field(event, "thread");
    let msg = message(event);
    if let Some(artifact) = artifact_mut(review, event) {
        if let Some(thread) = artifact.thread_mut(&id) {
            thread.messages.push(msg);
        }
    }
}

fn thread_edited(review: &mut Review, event: &Event) {
    let id = str_field(event, "thread");
    let text = str_field(event, "text");
    if let Some(artifact) = artifact_mut(review, event) {
        if let Some(thread) = artifact.thread_mut(&id) {
            if let Some(first) = thread.messages.first_mut() {
                first.text = text;
            }
        }
    }
}

fn thread_deleted(review: &mut Review, event: &Event) {
    let id = str_field(event, "thread");
    if let Some(artifact) = artifact_mut(review, event) {
        artifact.threads.retain(|t| t.id != id);
    }
}

fn thread_resolved(review: &mut Review, event: &Event) {
    let id = str_field(event, "thread");
    let status = match str_field(event, "status").as_str() {
        "changed" => ThreadStatus::Changed,
        "declined" => ThreadStatus::Declined,
        _ => return,
    };
    let note = str_field(event, "note");
    let ts = event.ts.clone();
    if let Some(artifact) = artifact_mut(review, event) {
        if let Some(thread) = artifact.thread_mut(&id) {
            thread.status = status;
            if !note.is_empty() {
                thread.messages.push(Message {
                    actor: "agent".to_string(),
                    text: note,
                    ts,
                });
            }
        }
    }
}

fn answered(review: &mut Review, event: &Event) {
    let question = str_field(event, "question");
    let text = str_field(event, "text");
    if let Some(artifact) = artifact_mut(review, event) {
        artifact.answers.insert(question, text);
    }
}

fn reviewed(review: &mut Review, event: &Event) {
    let target = str_field(event, "ref");
    let on = event
        .data
        .get("on")
        .and_then(|b| b.as_bool())
        .unwrap_or(true);
    if let Some(artifact) = artifact_mut(review, event) {
        if on {
            artifact.reviewed.insert(target);
        } else {
            artifact.reviewed.remove(&target);
        }
    }
}

fn chat(review: &mut Review, event: &Event) {
    // A thread-scoped chat is a thread message; a page-level one is not.
    let thread = str_field(event, "thread");
    let msg = message(event);
    let Some(artifact) = artifact_mut(review, event) else {
        return;
    };
    if thread.is_empty() {
        artifact.chat.push(msg);
    } else if let Some(t) = artifact.thread_mut(&thread) {
        t.messages.push(msg);
    }
}

fn submitted(review: &mut Review, event: &Event) {
    if let Some(artifact) = artifact_mut(review, event) {
        artifact.submitted = true;
    }
}

fn cursor(review: &mut Review, event: &Event) {
    let agent = str_field(event, "agent");
    let seq = event
        .data
        .get("acked_seq")
        .and_then(|s| s.as_u64())
        .unwrap_or(0);
    if agent.is_empty() {
        return;
    }
    // Defensively monotonic. The committer already refuses a backwards ack,
    // but a fold must never move a cursor back whatever the log holds.
    let entry = review.cursors.entry(agent).or_insert(0);
    *entry = (*entry).max(seq);
}

fn lease_taken(review: &mut Review, event: &Event) {
    let generation = event
        .data
        .get("generation")
        .and_then(|g| g.as_u64())
        .unwrap_or(0);
    let mode = match str_field(event, "mode").as_str() {
        "live" => Mode::Live,
        _ => Mode::Waiting,
    };
    review.lease = Some(LeaseRecord {
        name: str_field(event, "agent"),
        generation,
        token: str_field(event, "token"),
        pid: event
            .data
            .get("pid")
            .and_then(|p| p.as_u64())
            .map(|p| p as u32),
        mode,
    });
    review.lease_generation = review.lease_generation.max(generation);
}

fn lease_released(review: &mut Review, _event: &Event) {
    // The generation is deliberately left where it is: a token from a released
    // generation must never validate again.
    review.lease = None;
}
