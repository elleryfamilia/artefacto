//! What an agent receives, and when.
//!
//! The rule that carries this file is spec 6.4's: **there is no separate
//! passive buffer**. A frame's contents are always computed from the cursor,
//! so the same passive event cannot be delivered once by a poll and again by
//! the next wake-up.
//!
//! The second is spec 16's: delivery is **at-least-once**. The agent says
//! what it has dealt with (`--ack`, or `ack --seq`), the server never guesses,
//! and a call that acknowledges nothing is handed the same frame again. An
//! earlier version had the server remember the frame it last handed out and
//! acknowledge it on the next call, which is at-most-once: an agent that
//! received a frame and restarted before acting on it lost it.

mod support;

use artefacto::server::delivery;
use artefacto::server::event::Actor;
use artefacto::server::lease::{self, Claim};
use artefacto::server::review::LeaseRecord;
use support::InProcess;

fn session(s: &InProcess, name: &str) -> LeaseRecord {
    lease::acquire(&s.shared, Claim::waiting(name)).expect("the lease is free")
}

fn kinds(frame: &artefacto::server::event::Frame) -> Vec<String> {
    frame.events.iter().map(|e| e.r#type.clone()).collect()
}

#[test]
fn a_frame_stops_at_the_earliest_active_event() {
    // Spec 5: "When several active events are waiting, `await` returns at the
    // earliest one, and the frame stops there."
    let s = InProcess::start();
    s.seed_artifact();
    s.log_reviewer("thread.opened");
    s.log_reviewer("question.answered");
    s.log_reviewer("chat.sent");
    s.log_reviewer("review.submitted");

    let f = delivery::frame_since(&s.shared, 0).expect("something to deliver");
    assert_eq!(
        kinds(&f),
        ["thread.opened", "question.answered", "chat.sent"],
        "the passive events before it ride along; the later active one does not"
    );
    assert_eq!(
        f.seq,
        f.events.last().unwrap().seq,
        "spec 6.1: a frame's seq is its last event's"
    );
}

#[test]
fn passive_events_alone_produce_no_frame_in_digest_mode() {
    let s = InProcess::start();
    s.seed_artifact();
    s.log_reviewer("thread.opened");
    s.log_reviewer("element.reviewed");

    assert!(
        delivery::frame_since(&s.shared, s.cursor_of("claude")).is_none(),
        "spec 6.4: a passive event does not by itself cause a frame"
    );
}

#[test]
fn the_agent_never_receives_its_own_events() {
    // Spec 7 has the agent scanning frames for chat it must answer, not for
    // its own output.
    let s = InProcess::start();
    s.seed_artifact(); // revision.published, actor agent
    s.log_agent("thread.replied");
    s.log_agent("thread.resolved");
    s.log_reviewer("chat.sent");

    let f = delivery::frame_since(&s.shared, 0).unwrap();
    assert_eq!(kinds(&f), ["chat.sent"]);
    assert!(f.events.iter().all(|e| e.actor != Actor::Agent));
}

#[test]
fn an_agents_own_revision_does_not_wake_it() {
    let s = InProcess::start();
    s.seed_artifact();
    assert!(
        delivery::frame_since(&s.shared, 0).is_none(),
        "a log holding only the agent's own work has nothing to tell it"
    );
}

#[test]
fn internal_records_are_never_delivered() {
    // The earlier draft's own test failed on this: `ack` appends
    // `cursor.acked`, which is passive, so it rode along in the very next
    // frame — and in live mode each flush triggered the next one.
    let s = InProcess::start();
    s.seed_artifact();
    let claude = session(&s, "claude");
    s.log_reviewer("thread.opened");
    s.log_reviewer("chat.sent");

    let first = delivery::frame_since(&s.shared, 0).unwrap();
    delivery::ack(&s.shared, &claude, first.seq).unwrap();

    s.log_reviewer("chat.sent");
    let second = delivery::frame_since(&s.shared, s.cursor_of("claude")).unwrap();
    assert_eq!(
        kinds(&second),
        ["chat.sent"],
        "no bookkeeping, no lease record, no re-delivered passive event"
    );
}

#[test]
fn taking_the_lease_does_not_by_itself_wake_the_agent() {
    let s = InProcess::start();
    s.seed_artifact();
    session(&s, "claude");
    assert!(
        delivery::frame_since(&s.shared, 0).is_none(),
        "`lease.taken` is internal, so it can neither fill nor cause a frame"
    );
}

#[test]
fn an_unacked_frame_is_delivered_again() {
    let s = InProcess::start();
    s.seed_artifact();
    s.log_reviewer("chat.sent");
    let cursor = s.cursor_of("claude");

    let first = delivery::frame_since(&s.shared, cursor).unwrap();
    // The agent dies here, before acknowledging.
    let again = delivery::frame_since(&s.shared, cursor).unwrap();
    assert_eq!(
        first.seq, again.seq,
        "at-least-once: a crash replays rather than loses"
    );
}

#[test]
fn the_cursor_survives_a_restart() {
    let s = InProcess::start();
    s.seed_artifact();
    let claude = session(&s, "claude");
    s.log_reviewer("chat.sent");
    delivery::ack(&s.shared, &claude, s.last_seq()).unwrap();
    let expected = s.cursor_of("claude");
    assert!(expected > 0);

    let s = s.restart();
    assert_eq!(
        s.cursor_of("claude"),
        expected,
        "spec 5: an agent that restarts with no memory of where it was resumes \
         exactly where it left off"
    );
}

#[test]
fn an_ack_behind_the_cursor_is_a_no_op() {
    // At-least-once means acknowledgements get repeated: after a retry, after
    // a replay with `--since`. A repeat that failed the call would turn the
    // safe path into a failed tool call. `--since` is the way to ask for a
    // replay; nothing moves the cursor back.
    let s = InProcess::start();
    s.seed_artifact();
    let claude = session(&s, "claude");
    s.log_reviewer("chat.sent");
    let top = s.last_seq();
    delivery::ack(&s.shared, &claude, top).unwrap();
    let written = s.last_seq();

    delivery::ack(&s.shared, &claude, top - 1).expect("behind the cursor is not an error");
    assert_eq!(
        s.cursor_of("claude"),
        top,
        "and it does not move the cursor back"
    );
    assert_eq!(s.last_seq(), written, "nor write anything");
}

#[test]
fn an_ack_past_the_end_of_the_log_is_refused() {
    let s = InProcess::start();
    s.seed_artifact();
    let claude = session(&s, "claude");
    assert!(
        delivery::ack(&s.shared, &claude, s.last_seq() + 500).is_err(),
        "acknowledging what has not happened would silently skip it"
    );
    assert_eq!(s.cursor_of("claude"), 0);
}

#[test]
fn re_acking_the_same_seq_writes_nothing() {
    let s = InProcess::start();
    s.seed_artifact();
    let claude = session(&s, "claude");
    s.log_reviewer("chat.sent");
    delivery::ack(&s.shared, &claude, s.last_seq()).unwrap();
    let after_first = s.last_seq();

    delivery::ack(&s.shared, &claude, s.cursor_of("claude")).unwrap();
    assert_eq!(
        s.last_seq(),
        after_first,
        "an idempotent ack must not grow the log once per poll cycle forever"
    );
}

#[test]
fn an_ack_from_a_superseded_session_is_refused() {
    let s = InProcess::start();
    s.seed_artifact();
    let claude = session(&s, "claude");
    s.log_reviewer("chat.sent");
    lease::acquire(&s.shared, Claim::waiting("codex").with_takeover(true)).unwrap();

    assert!(
        delivery::ack(&s.shared, &claude, s.last_seq()).is_err(),
        "spec 4.2: every agent mutation carries the token, and a superseded one is refused"
    );
    assert_eq!(s.cursor_of("claude"), 0);
}

// ---------------------------------------------------------------------------
// The agent acknowledges; the server never guesses. Spec 16: at-least-once.
// ---------------------------------------------------------------------------

#[test]
fn a_call_that_names_the_previous_seq_acknowledges_it() {
    let s = InProcess::start();
    s.seed_artifact();
    let claude = session(&s, "claude");
    s.log_reviewer("thread.opened");
    s.log_reviewer("chat.sent");

    let first = delivery::read(&s.shared, &claude, None, None).unwrap();
    let frame = first.frame.expect("a chat wakes the agent");
    assert_eq!(kinds(&frame), ["thread.opened", "chat.sent"]);
    assert_eq!(
        s.cursor_of("claude"),
        0,
        "reading is not acknowledging: the agent has not acted on it yet"
    );

    let second = delivery::read(&s.shared, &claude, Some(frame.seq), None).unwrap();
    assert!(
        second.frame.is_none(),
        "acknowledged, and nothing new has happened"
    );
    assert_eq!(s.cursor_of("claude"), frame.seq);
}

#[test]
fn a_call_that_acknowledges_nothing_is_handed_the_same_frame_again() {
    // Spec 16: at-least-once. An earlier version had the server acknowledge
    // the previous frame on the session's next call, which is exactly the
    // at-most-once that "silently drops a review when an agent crashes at the
    // wrong moment": the agent that received this frame died before acting,
    // came back with no memory, and the server acknowledged on its behalf.
    let s = InProcess::start();
    s.seed_artifact();
    let claude = session(&s, "claude");
    s.log_reviewer("chat.sent");

    let first = delivery::read(&s.shared, &claude, None, None)
        .unwrap()
        .frame
        .unwrap();
    // The process dies here, with the frame in hand and nothing done about it.
    let again = delivery::read(&s.shared, &claude, None, None)
        .unwrap()
        .frame
        .expect("at-least-once");
    assert_eq!(first.seq, again.seq);
    assert_eq!(s.cursor_of("claude"), 0);
}

#[test]
fn a_server_restart_changes_nothing_about_that() {
    let s = InProcess::start();
    s.seed_artifact();
    let claude = session(&s, "claude");
    s.log_reviewer("chat.sent");
    let first = delivery::read(&s.shared, &claude, None, None)
        .unwrap()
        .frame
        .unwrap();

    let s = s.restart();
    let again = delivery::read(&s.shared, &claude, None, None)
        .unwrap()
        .frame
        .expect("the cursor is in the log, and it has not moved");
    assert_eq!(first.seq, again.seq);
}

#[test]
fn a_takeover_inherits_the_cursor_and_the_unacknowledged_frame_with_it() {
    let s = InProcess::start();
    s.seed_artifact();
    let claude = session(&s, "claude");
    s.log_reviewer("chat.sent");
    let unacked = delivery::read(&s.shared, &claude, None, None)
        .unwrap()
        .frame
        .unwrap();
    // claude dies without acknowledging. codex takes over.
    let codex = lease::acquire(&s.shared, Claim::waiting("codex").with_takeover(true)).unwrap();

    let next = delivery::read(&s.shared, &codex, None, None)
        .unwrap()
        .frame
        .expect("codex must receive what claude never acknowledged");
    assert_eq!(next.seq, unacked.seq);
}

#[test]
fn a_partial_ack_brings_the_rest_of_the_frame_back() {
    // Spec 5: `ack --seq N` exists "when an agent wants to acknowledge only
    // part of a frame".
    let s = InProcess::start();
    s.seed_artifact();
    let claude = session(&s, "claude");
    let opened = s.log_reviewer("thread.opened");
    let chat = s.log_reviewer("chat.sent");

    let frame = delivery::read(&s.shared, &claude, None, None)
        .unwrap()
        .frame
        .unwrap();
    assert_eq!(frame.seq, chat);

    delivery::ack(&s.shared, &claude, opened).unwrap();
    let next = delivery::read(&s.shared, &claude, None, None).unwrap();
    assert_eq!(
        next.frame.map(|f| kinds(&f)),
        Some(vec!["chat.sent".to_string()]),
        "the part that was not acknowledged comes back"
    );
    assert_eq!(s.cursor_of("claude"), opened);
}

#[test]
fn naming_a_cursor_replays_from_there_without_moving_anything() {
    let s = InProcess::start();
    s.seed_artifact();
    let claude = session(&s, "claude");
    s.log_reviewer("chat.sent");
    let first = delivery::read(&s.shared, &claude, None, None)
        .unwrap()
        .frame
        .unwrap();
    delivery::ack(&s.shared, &claude, first.seq).unwrap();

    // `events --since 0`: a replay.
    let replay = delivery::read(&s.shared, &claude, None, Some(0)).unwrap();
    assert_eq!(replay.since, 0);
    assert_eq!(replay.frame.unwrap().seq, first.seq);
    assert_eq!(
        s.cursor_of("claude"),
        first.seq,
        "a replay reads; it acknowledges nothing and moves nothing"
    );
}

#[test]
fn acknowledging_a_replayed_frame_behind_the_cursor_is_a_no_op() {
    // The ordinary sequel to a replay: the agent passes the replayed frame's
    // seq back as `--ack`. That seq is behind the cursor, and refusing it
    // would turn the documented restart path into a failed tool call.
    let s = InProcess::start();
    s.seed_artifact();
    let claude = session(&s, "claude");
    let early = s.log_reviewer("chat.sent");
    let late = s.log_reviewer("chat.sent");
    delivery::ack(&s.shared, &claude, late).unwrap();

    let replayed = delivery::read(&s.shared, &claude, None, Some(0))
        .unwrap()
        .frame
        .unwrap();
    assert_eq!(replayed.seq, early);
    let after = delivery::read(&s.shared, &claude, Some(replayed.seq), None)
        .expect("an ack behind the cursor is not an error");
    assert!(after.frame.is_none());
    assert_eq!(
        s.cursor_of("claude"),
        late,
        "and it does not move the cursor back"
    );
}

#[test]
fn the_timeout_tail_stops_before_the_first_active_event() {
    // A timeout says "nothing actionable", and the agent acknowledges its
    // seq. If the tail carried an active event — another artifact's chat
    // under `--artifact`, or one that landed as the wait gave up — that event
    // would be acknowledged without ever being delivered as what it is.
    let s = InProcess::start();
    s.seed_artifact();
    let opened = s.log_reviewer("thread.opened");
    s.log_reviewer("chat.sent");
    s.log_reviewer("thread.opened");

    let tail = delivery::passive_since(&s.shared, 0);
    assert_eq!(tail.iter().map(|e| e.seq).collect::<Vec<_>>(), [opened]);
}

#[test]
fn concurrent_reads_with_the_same_ack_leave_the_cursor_at_exactly_that_seq() {
    // One lease, but a retrying CLI can ask twice at once. Eight identical
    // acknowledgements are one cursor move and one log record.
    let s = InProcess::start();
    s.seed_artifact();
    let claude = session(&s, "claude");
    let first = s.log_reviewer("chat.sent");
    s.log_reviewer("chat.sent");

    std::thread::scope(|scope| {
        for _ in 0..8 {
            let shared = &s.shared;
            let claude = &claude;
            scope.spawn(move || delivery::read(shared, claude, Some(first), None).unwrap());
        }
    });
    assert_eq!(s.cursor_of("claude"), first);
    assert_eq!(
        s.count_events("cursor.acked"),
        1,
        "eight identical acks write one record"
    );
}
