//! The fold, and the invariant that makes a restart safe.
//!
//! The test that carries this file is `concurrent_commits_keep_the_fold_equal_to_the_log`.
//! Appending and folding as two separate critical sections lets two threads
//! interleave, after which the in-memory state is no longer `fold(log)` — and
//! nothing detects it at the time.

mod support;
use artefacto::server::event::{Actor, Event, EVENT_FORMAT};
use artefacto::server::fold::fold;
use artefacto::server::http::{with_review, Committer};
use artefacto::server::review::ThreadStatus;
use support::*;

fn ev(seq: u64, actor: Actor, kind: &str, data: serde_json::Value) -> Event {
    Event {
        format: EVENT_FORMAT.to_string(),
        seq,
        ts: "2026-09-09T00:00:00Z".to_string(),
        artifact: "plan:demo".to_string(),
        revision: 1,
        actor,
        r#type: kind.to_string(),
        data,
        batch: None,
    }
}

fn plan_data() -> serde_json::Value {
    serde_json::json!({
        "plan": {
            "format": "artefacto.plan/1",
            "meta": { "id": "demo", "title": "Demo" },
            "phases": [{ "id": "p-one", "tasks": [{ "id": "t-a" }, { "id": "t-b" }] }]
        },
        "plan_hash": "sha256:abc",
        "source_path": "/tmp/demo.json",
        "summary": "first"
    })
}

#[test]
fn an_empty_log_folds_to_an_empty_review() {
    let r = fold(&[]);
    assert!(r.artifacts.is_empty());
    assert!(r.cursors.is_empty());
    assert!(r.lease.is_none());
    assert_eq!(r.lease_generation, 0);
}

#[test]
fn a_revision_carries_the_whole_plan_so_the_log_can_rebuild_the_body() {
    let r = fold(&[ev(1, Actor::Agent, "revision.published", plan_data())]);
    let a = r.artifacts.get("plan:demo").expect("the artifact exists");
    assert_eq!(a.revision, 1);
    assert_eq!(a.plan_hash, "sha256:abc");
    assert_eq!(a.plan["meta"]["id"], "demo", "not just a change summary");
}

#[test]
fn threads_get_stable_ids_that_are_never_reused() {
    let r = fold(&[
        ev(1, Actor::Agent, "revision.published", plan_data()),
        ev(
            2,
            Actor::Reviewer,
            "thread.opened",
            serde_json::json!({ "thread": "c-1", "ref": "task:t-a", "text": "why?" }),
        ),
        ev(
            3,
            Actor::Reviewer,
            "thread.opened",
            serde_json::json!({ "thread": "c-2", "ref": "task:t-b", "text": "and this?" }),
        ),
        ev(
            4,
            Actor::Reviewer,
            "thread.deleted",
            serde_json::json!({ "thread": "c-1" }),
        ),
        ev(
            5,
            Actor::Reviewer,
            "thread.opened",
            serde_json::json!({ "thread": "c-3", "ref": "task:t-a", "text": "third" }),
        ),
    ]);
    let a = &r.artifacts["plan:demo"];
    let ids: Vec<&str> = a.threads.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["c-2", "c-3"],
        "c-1 was deleted; c-3 did not take its number"
    );
    assert_eq!(a.next_thread_n, 4, "the counter never goes backwards");
}

#[test]
fn a_resolution_sets_the_status_and_keeps_the_note() {
    let r = fold(&[
        ev(1, Actor::Agent, "revision.published", plan_data()),
        ev(
            2,
            Actor::Reviewer,
            "thread.opened",
            serde_json::json!({ "thread": "c-1", "ref": "task:t-a", "text": "why?" }),
        ),
        ev(
            3,
            Actor::Agent,
            "thread.resolved",
            serde_json::json!({ "thread": "c-1", "status": "declined", "note": "out of scope" }),
        ),
    ]);
    let t = &r.artifacts["plan:demo"].threads[0];
    assert_eq!(t.status, ThreadStatus::Declined);
    assert_eq!(t.messages.last().unwrap().text, "out of scope");
}

#[test]
fn a_thread_whose_target_disappears_becomes_unanchored_and_can_come_back() {
    let mut gone = plan_data();
    gone["plan"]["phases"][0]["tasks"] = serde_json::json!([{ "id": "t-b" }]);

    let r = fold(&[
        ev(1, Actor::Agent, "revision.published", plan_data()),
        ev(
            2,
            Actor::Reviewer,
            "thread.opened",
            serde_json::json!({ "thread": "c-1", "ref": "task:t-a", "text": "?" }),
        ),
        ev(3, Actor::Agent, "revision.published", gone),
    ]);
    assert_eq!(
        r.artifacts["plan:demo"].threads[0].status,
        ThreadStatus::Unanchored,
        "nothing a reviewer wrote is silently dropped; it is listed instead"
    );

    let restored = fold(&[
        ev(1, Actor::Agent, "revision.published", plan_data()),
        ev(
            2,
            Actor::Reviewer,
            "thread.opened",
            serde_json::json!({ "thread": "c-1", "ref": "task:t-a", "text": "?" }),
        ),
        ev(3, Actor::Agent, "revision.published", {
            let mut p = plan_data();
            p["plan"]["phases"][0]["tasks"] = serde_json::json!([{ "id": "t-b" }]);
            p
        }),
        ev(4, Actor::Agent, "revision.published", plan_data()),
    ]);
    assert_eq!(
        restored.artifacts["plan:demo"].threads[0].status,
        ThreadStatus::Open,
        "a target that comes back re-anchors the thread"
    );
}

#[test]
fn cursors_and_the_lease_fold_from_the_log() {
    let r = fold(&[
        ev(
            1,
            Actor::Server,
            "lease.taken",
            serde_json::json!({ "agent": "claude", "generation": 1, "token": "t1", "mode": "waiting" }),
        ),
        ev(
            2,
            Actor::Server,
            "cursor.acked",
            serde_json::json!({ "agent": "claude", "acked_seq": 7 }),
        ),
    ]);
    assert_eq!(
        r.cursors["claude"], 7,
        "an agent that restarts resumes where it left off"
    );
    let lease = r.lease.expect("the lease is state like any other");
    assert_eq!(lease.generation, 1);
    assert_eq!(lease.pid, None, "a waiting lease records no pid");
    assert_eq!(r.lease_generation, 1);
}

#[test]
fn a_released_lease_keeps_its_generation() {
    let r = fold(&[
        ev(
            1,
            Actor::Server,
            "lease.taken",
            serde_json::json!({ "agent": "claude", "generation": 1, "token": "t1", "mode": "waiting" }),
        ),
        ev(
            2,
            Actor::Server,
            "lease.released",
            serde_json::json!({ "generation": 1 }),
        ),
    ]);
    assert!(r.lease.is_none());
    assert_eq!(
        r.lease_generation, 1,
        "a token from generation 1 must never validate again, even after a release"
    );
}

#[test]
fn a_cursor_never_folds_backwards() {
    let r = fold(&[
        ev(
            1,
            Actor::Server,
            "cursor.acked",
            serde_json::json!({ "agent": "a", "acked_seq": 9 }),
        ),
        ev(
            2,
            Actor::Server,
            "cursor.acked",
            serde_json::json!({ "agent": "a", "acked_seq": 4 }),
        ),
    ]);
    assert_eq!(
        r.cursors["a"], 9,
        "defensively monotonic whatever the log holds"
    );
}

#[test]
fn a_client_id_folds_with_the_id_it_assigned() {
    let r = fold(&[
        ev(1, Actor::Agent, "revision.published", plan_data()),
        ev(
            2,
            Actor::Reviewer,
            "thread.opened",
            serde_json::json!({ "thread": "c-1", "ref": "task:t-a", "text": "x", "client_id": "cid-1" }),
        ),
    ]);
    assert_eq!(
        r.committed.get("cid-1"),
        Some(&Some("c-1".to_string())),
        "a retry must be answered with the same id, which a set could not do"
    );
}

// --- the invariant --------------------------------------------------------

#[test]
fn a_restart_rebuilds_the_review_from_the_log_alone() {
    let s = InProcess::start();
    {
        let c = Committer::open(&s.shared);
        c.append(
            "plan:demo",
            1,
            Actor::Agent,
            "revision.published",
            plan_data(),
        )
        .unwrap();
        c.append(
            "plan:demo",
            1,
            Actor::Reviewer,
            "thread.opened",
            serde_json::json!({ "thread": "c-1", "ref": "task:t-a", "text": "why?" }),
        )
        .unwrap();
    }
    let before = with_review(&s.shared, |r| r.snapshot());

    let s = s.restart();
    let after = with_review(&s.shared, |r| r.snapshot());
    assert_eq!(
        after, before,
        "kill it, start it again, and everything comes back"
    );
}

#[test]
fn concurrent_commits_keep_the_fold_equal_to_the_log() {
    // Eight threads appending at once. If appending and folding were two
    // separate critical sections they would interleave, and the in-memory
    // Review would stop matching a fresh fold of the same log.
    let s = InProcess::start();
    {
        let c = Committer::open(&s.shared);
        c.append(
            "plan:demo",
            1,
            Actor::Agent,
            "revision.published",
            plan_data(),
        )
        .unwrap();
    }

    std::thread::scope(|scope| {
        for t in 0..8 {
            let shared = &s.shared;
            scope.spawn(move || {
                for i in 0..12 {
                    let c = Committer::open(shared);
                    let id = format!("c-{}", t * 12 + i + 1);
                    c.append(
                        "plan:demo",
                        1,
                        Actor::Reviewer,
                        "thread.opened",
                        serde_json::json!({ "thread": id, "ref": "task:t-a", "text": "x" }),
                    )
                    .unwrap();
                }
            });
        }
    });

    let in_memory = with_review(&s.shared, |r| r.snapshot());
    let from_log = {
        let log = s.shared.log.lock().unwrap();
        fold(log.since(0)).snapshot()
    };
    assert_eq!(
        in_memory, from_log,
        "the in-memory state must be exactly fold(log)"
    );
    assert_eq!(
        in_memory["artifacts"]["plan:demo"]["threads"]
            .as_array()
            .unwrap()
            .len(),
        96,
        "every append landed exactly once"
    );
}

#[test]
fn sequence_numbers_are_unique_under_concurrency() {
    let s = InProcess::start();
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let shared = &s.shared;
            scope.spawn(move || {
                for _ in 0..12 {
                    let c = Committer::open(shared);
                    c.append(
                        "plan:demo",
                        1,
                        Actor::Reviewer,
                        "element.reviewed",
                        serde_json::json!({ "ref": "task:t-a", "on": true }),
                    )
                    .unwrap();
                }
            });
        }
    });
    let log = s.shared.log.lock().unwrap();
    let seqs: Vec<u64> = log.since(0).iter().map(|e| e.seq).collect();
    let unique: std::collections::BTreeSet<u64> = seqs.iter().copied().collect();
    assert_eq!(seqs.len(), 96);
    assert_eq!(unique.len(), 96, "no sequence number handed out twice");
    assert_eq!(*unique.iter().next_back().unwrap(), 96, "and no gaps");
}

#[test]
fn a_new_revision_reopens_a_submitted_review() {
    // Spec 7 rule 4: after a submit the agent pushes the next revision "and
    // keeps the monitor armed for the next round". A review that stayed
    // submitted would never be away again, and would name no artifact for its
    // timer events.
    let r = fold(&[
        ev(1, Actor::Agent, "revision.published", plan_data()),
        ev(
            2,
            Actor::Reviewer,
            "review.submitted",
            serde_json::json!({ "verdict": "approve", "base_revision": 1 }),
        ),
        ev(3, Actor::Agent, "revision.published", plan_data()),
    ]);
    assert!(!r.artifacts["plan:demo"].submitted);
    assert_eq!(r.artifacts["plan:demo"].revision, 2);
}
