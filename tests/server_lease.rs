//! The lease: one agent acts at a time, and the token is the identity.
//!
//! Every assertion here is checked against spec 4.2 directly rather than
//! against plan 2b's Task 3, which is wrong in two places:
//!
//! 1. Its restart test asserts that a pre-restart token stops validating.
//!    Spec 4.2 says all state folds from the log "and the lease", and 6.7 says
//!    a restarted server "replays the log to rebuild every piece of state".
//!    A token that died on restart would also break spec 5's promise that
//!    `await` "retries against the same cursor" when "the server restarts
//!    mid-wait". So the holder keeps its lease across a restart here.
//! 2. It implies an expired lease kills its own holder's token. The TTL exists
//!    to let *another* agent in; while nobody else has taken it, the holder
//!    presenting its own token revives it. See
//!    `the_holder_may_revive_its_own_expired_lease`.

mod support;

use artefacto::server::event::Actor;
use artefacto::server::http::{with_review, Committer};
use artefacto::server::lease::{self, Claim, LeaseError};
use artefacto::server::review::Mode;
use std::time::Duration;
use support::{status_of, wait_for, InProcess};

fn take(s: &InProcess, name: &str) -> artefacto::server::review::LeaseRecord {
    lease::acquire(&s.shared, Claim::waiting(name)).expect("the lease is free")
}

/// Set an agent's delivery cursor the way `ack` will, so the inheritance rule
/// can be tested before delivery exists.
fn set_cursor(s: &InProcess, name: &str, seq: u64) {
    let c = Committer::open(&s.shared);
    c.append(
        "",
        0,
        Actor::Server,
        "cursor.acked",
        serde_json::json!({ "agent": name, "acked_seq": seq }),
    )
    .expect("append");
}

fn cursor(s: &InProcess, name: &str) -> u64 {
    with_review(&s.shared, |r| r.cursors.get(name).copied().unwrap_or(0))
}

#[test]
fn the_first_agent_takes_the_lease_and_it_is_logged() {
    let s = InProcess::start();
    let l = take(&s, "claude");

    assert_eq!(l.generation, 1, "generations start at one");
    assert_eq!(l.name, "claude");
    assert!(
        l.token.starts_with("1."),
        "spec 4.2: the session token carries a generation number, got {}",
        l.token
    );
    assert!(l.token.len() > 40, "the rest of the token is unguessable");

    let e = s.last_event_of_type("lease.taken");
    assert_eq!(
        e["data"]["agent"], "claude",
        "the lease is state, so it folds from the log like everything else"
    );
    assert_eq!(e["data"]["generation"], 1);
    assert_eq!(e["data"]["mode"], "waiting");
    assert_eq!(e["actor"], "server", "the server records its own decisions");
}

#[test]
fn a_waiting_lease_records_no_pid_because_await_exits_between_polls() {
    let s = InProcess::start();
    let l = take(&s, "claude");

    assert!(
        l.pid.is_none(),
        "`await` is a short-lived subprocess: recording its pid would stop its \
         own token validating the moment the command returned"
    );
    assert!(lease::current(&s.shared).is_some());
    lease::validate(&s.shared, &l.token).expect("the token must still work for reply and push");
    assert_eq!(
        s.last_event_of_type("lease.taken")["data"]["pid"],
        serde_json::Value::Null
    );
}

#[test]
fn a_live_lease_records_its_pid() {
    let s = InProcess::start();
    let me = std::process::id();
    let l = lease::acquire(&s.shared, Claim::live("claude", me)).unwrap();

    assert_eq!(l.pid, Some(me));
    assert_eq!(l.mode, Mode::Live);
    assert_eq!(s.last_event_of_type("lease.taken")["data"]["pid"], me);
}

#[test]
fn a_live_lease_whose_process_died_is_released_without_a_takeover() {
    // Spec 4.2: "an expired lease, or one whose recorded pid is dead, is
    // released by the server".
    let s = InProcess::start();
    let dead = lease::acquire(&s.shared, Claim::live("claude", 0)).unwrap(); // pid 0 is never a live user process
    assert_eq!(dead.pid, Some(0));

    assert!(
        lease::current(&s.shared).is_none(),
        "a crashed --follow agent must not hold the lease until its TTL runs out"
    );
    let next = lease::acquire(&s.shared, Claim::waiting("codex"))
        .expect("a dead holder needs no takeover");
    assert_eq!(next.generation, 2);
    assert!(matches!(
        lease::validate(&s.shared, &dead.token),
        Err(LeaseError::Superseded)
    ));
}

#[test]
fn the_holder_re_polls_with_its_own_token_and_keeps_the_same_lease() {
    // This is the loop the lease exists to support: spec 4.2 says the token
    // exists "so a lease survives an `await` returning every 90 seconds
    // instead of dropping and re-taking on every cycle".
    let s = InProcess::start();
    let first = take(&s, "claude");
    let second = lease::acquire(
        &s.shared,
        Claim::waiting("claude").with_token(Some(&first.token)),
    )
    .expect("presenting a valid token must refresh, not refuse");

    assert_eq!(second.token, first.token, "the same lease, not a new one");
    assert_eq!(
        second.generation, first.generation,
        "re-polling does not bump the generation"
    );
    assert_eq!(
        s.events_of_type("lease.taken").len(),
        1,
        "a re-poll changes nothing, so it writes nothing"
    );
}

#[test]
fn switching_from_await_to_follow_keeps_the_token_and_logs_the_new_mode() {
    let s = InProcess::start();
    let me = std::process::id();
    let waiting = take(&s, "claude");
    let live = lease::acquire(
        &s.shared,
        Claim::live("claude", me).with_token(Some(&waiting.token)),
    )
    .expect("the holder may change transport without losing its lease");

    assert_eq!(live.token, waiting.token);
    assert_eq!(live.generation, waiting.generation);
    assert_eq!(live.mode, Mode::Live);
    assert_eq!(live.pid, Some(me));
    assert_eq!(
        s.events_of_type("lease.taken").len(),
        2,
        "the presence pill reads the mode, so a mode change is state and is logged"
    );

    let s = s.restart();
    let holder = lease::current(&s.shared).expect("the lease folds from the log");
    assert_eq!(holder.mode, Mode::Live, "including the mode it was last in");
    assert_eq!(holder.pid, Some(me));
}

#[test]
fn a_second_agent_is_refused_and_told_who_holds_it_and_for_how_long() {
    let s = InProcess::start();
    take(&s, "claude");
    s.age_lease(Duration::from_secs(42));

    match lease::acquire(&s.shared, Claim::waiting("codex")) {
        Err(LeaseError::Held { holder, age_secs }) => {
            assert_eq!(holder, "claude");
            assert!(
                (42..=43).contains(&age_secs),
                "spec 4.2: the refusal names the holder and its age, got {age_secs}s"
            );
        }
        other => panic!("expected Held, got {other:?}"),
    }
    assert_eq!(
        s.events_of_type("lease.taken").len(),
        1,
        "a refused acquire must not write anything"
    );
}

#[test]
fn takeover_bumps_the_generation_and_supersedes_the_old_token() {
    let s = InProcess::start();
    let first = take(&s, "claude");
    let second = lease::acquire(&s.shared, Claim::waiting("codex").with_takeover(true)).unwrap();

    assert_eq!(second.generation, 2);
    assert!(matches!(
        lease::validate(&s.shared, &first.token),
        Err(LeaseError::Superseded)
    ));
    lease::validate(&s.shared, &second.token).expect("the new token is the live one");
}

#[test]
fn a_superseded_token_cannot_re_acquire_by_presenting_itself() {
    let s = InProcess::start();
    let first = take(&s, "claude");
    lease::acquire(&s.shared, Claim::waiting("codex").with_takeover(true)).unwrap();

    assert!(
        matches!(
            lease::acquire(
                &s.shared,
                Claim::waiting("claude").with_token(Some(&first.token))
            ),
            Err(LeaseError::Superseded)
        ),
        "presenting a dead token is not a way back in"
    );
}

#[test]
fn an_expired_lease_lets_another_agent_in_without_a_takeover() {
    let s = InProcess::start();
    let first = take(&s, "claude");
    s.age_lease(lease::TTL + Duration::from_secs(1));

    assert!(lease::current(&s.shared).is_none());
    let next = lease::acquire(&s.shared, Claim::waiting("codex")).unwrap();
    assert_eq!(next.generation, 2);
    assert!(matches!(
        lease::validate(&s.shared, &first.token),
        Err(LeaseError::Superseded)
    ));
}

#[test]
fn the_holder_may_revive_its_own_expired_lease() {
    // The TTL exists to let another agent in, not to punish the holder. While
    // nobody else has taken it, the token is still the current one.
    let s = InProcess::start();
    let first = take(&s, "claude");
    s.age_lease(lease::TTL + Duration::from_secs(1));
    assert!(
        lease::current(&s.shared).is_none(),
        "expired: another agent may now walk in"
    );

    let same = lease::acquire(
        &s.shared,
        Claim::waiting("claude").with_token(Some(&first.token)),
    )
    .expect("nobody took it, so the holder's own token still names the lease");
    assert_eq!(same.generation, first.generation);
    assert_eq!(same.token, first.token);
    assert!(
        lease::current(&s.shared).is_some(),
        "and the TTL is refreshed"
    );
}

#[test]
fn any_agent_call_refreshes_the_ttl() {
    let s = InProcess::start();
    let l = take(&s, "claude");

    s.age_lease(lease::TTL - Duration::from_secs(5));
    lease::validate(&s.shared, &l.token).expect("still inside the TTL");
    s.age_lease(lease::TTL - Duration::from_secs(5));

    assert!(
        lease::current(&s.shared).is_some(),
        "two gaps of just under the TTL are not one gap of twice it"
    );
    assert!(matches!(
        lease::acquire(&s.shared, Claim::waiting("codex")),
        Err(LeaseError::Held { .. })
    ));
}

#[test]
fn releasing_frees_the_lease_immediately_and_keeps_the_generation() {
    // Spec 4.2: "a --follow disconnect releases it immediately".
    let s = InProcess::start();
    let l = take(&s, "claude");
    lease::release(&s.shared, &l.token);

    assert!(lease::current(&s.shared).is_none());
    assert_eq!(s.events_of_type("lease.released").len(), 1);
    assert!(matches!(
        lease::validate(&s.shared, &l.token),
        Err(LeaseError::Superseded)
    ));

    let next = lease::acquire(&s.shared, Claim::waiting("codex")).unwrap();
    assert_eq!(
        next.generation, 2,
        "a released generation is never reissued"
    );
}

#[test]
fn only_the_holder_can_release() {
    let s = InProcess::start();
    take(&s, "claude");
    lease::release(&s.shared, "1.not-the-token");

    assert!(
        lease::current(&s.shared).is_some(),
        "a stranger's release must not evict the holder"
    );
    assert!(s.events_of_type("lease.released").is_empty());
}

#[test]
fn the_lease_and_its_generation_survive_a_restart() {
    // Spec 6.7: a restarted server "replays the log to rebuild every piece of
    // state". Spec 5: `await` "retries against the same cursor" when "the
    // server restarts mid-wait" — which it could not, if the restart killed
    // its token.
    let s = InProcess::start();
    let first = take(&s, "claude");
    let s = s.restart();

    lease::validate(&s.shared, &first.token).expect("the holder keeps its lease across a restart");
    match lease::acquire(&s.shared, Claim::waiting("codex")) {
        Err(LeaseError::Held { holder, .. }) => assert_eq!(holder, "claude"),
        other => panic!("expected Held, got {other:?}"),
    }
    let taken = lease::acquire(&s.shared, Claim::waiting("codex").with_takeover(true)).unwrap();
    assert_eq!(
        taken.generation, 2,
        "a restart must not reset the generation to zero"
    );
    assert!(matches!(
        lease::validate(&s.shared, &first.token),
        Err(LeaseError::Superseded)
    ));
}

#[test]
fn a_fresh_lease_under_a_new_name_inherits_the_outgoing_cursor() {
    // Cursors are keyed by agent name. Without this, a takeover under a
    // different name starts from that name's cursor — zero — and replays every
    // passive event the previous agent already acknowledged.
    let s = InProcess::start();
    take(&s, "claude");
    set_cursor(&s, "claude", 7);

    lease::acquire(&s.shared, Claim::waiting("codex").with_takeover(true)).unwrap();
    assert_eq!(cursor(&s, "codex"), 7);
    assert_eq!(cursor(&s, "claude"), 7, "the old cursor is not moved");
}

#[test]
fn a_name_that_already_has_a_cursor_keeps_it() {
    // A genuine resume: this agent has been here before and knows where it was.
    let s = InProcess::start();
    take(&s, "claude");
    set_cursor(&s, "claude", 7);
    set_cursor(&s, "codex", 3);

    lease::acquire(&s.shared, Claim::waiting("codex").with_takeover(true)).unwrap();
    assert_eq!(cursor(&s, "codex"), 3, "inheriting would skip events 4..7");
}

#[test]
fn concurrent_takeovers_hand_out_distinct_contiguous_generations() {
    // Codex critical #7. With the decision taken outside the gate, two callers
    // both compute generation 1 and the loser is handed an already-superseded
    // token returned as `Ok`.
    let s = InProcess::start();
    let n = 8usize;
    let tokens: Vec<artefacto::server::review::LeaseRecord> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..n)
            .map(|i| {
                let shared = &s.shared;
                scope.spawn(move || {
                    let name = format!("agent-{i}");
                    lease::acquire(shared, Claim::waiting(&name).with_takeover(true))
                        .expect("a takeover is never refused")
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    let mut generations: Vec<u64> = tokens.iter().map(|l| l.generation).collect();
    generations.sort_unstable();
    assert_eq!(
        generations,
        (1..=n as u64).collect::<Vec<_>>(),
        "every caller must get its own generation"
    );
    assert_eq!(s.events_of_type("lease.taken").len(), n);

    let live = tokens
        .iter()
        .filter(|l| lease::validate(&s.shared, &l.token).is_ok())
        .count();
    assert_eq!(live, 1, "exactly one token is the current one");
}

#[test]
fn concurrent_first_acquires_produce_exactly_one_winner() {
    let s = InProcess::start();
    let n = 8usize;
    let results: Vec<Result<_, LeaseError>> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..n)
            .map(|i| {
                let shared = &s.shared;
                scope.spawn(move || {
                    let name = format!("agent-{i}");
                    lease::acquire(shared, Claim::waiting(&name))
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    assert_eq!(
        results.iter().filter(|r| r.is_ok()).count(),
        1,
        "one agent acts at a time"
    );
    assert!(results
        .iter()
        .filter_map(|r| r.as_ref().err())
        .all(|e| matches!(e, LeaseError::Held { .. })));
    assert_eq!(s.events_of_type("lease.taken").len(), 1);
}

#[test]
fn status_reports_the_holder_and_its_age_and_never_the_token() {
    let s = InProcess::start();
    let l = take(&s, "claude");
    set_cursor(&s, "claude", 5);
    s.age_lease(Duration::from_secs(9));

    let raw = s.cli_raw("status");
    let body: serde_json::Value =
        serde_json::from_str(raw.split("\r\n\r\n").nth(1).unwrap_or("")).expect("json");
    assert_eq!(body["lease"]["agent"], "claude");
    assert_eq!(body["lease"]["generation"], 1);
    assert_eq!(body["lease"]["mode"], "waiting");
    assert_eq!(body["lease"]["acked_seq"], 5);
    let age = body["lease"]["age_secs"].as_u64().expect("an age");
    assert!((9..=10).contains(&age), "got {age}s");
    assert!(
        !raw.contains(&l.token),
        "spec 5: `status --json` never prints the session token"
    );
}

#[test]
fn status_reports_no_lease_when_none_is_held() {
    let s = InProcess::start();
    let raw = s.cli_raw("status");
    let body: serde_json::Value =
        serde_json::from_str(raw.split("\r\n\r\n").nth(1).unwrap_or("")).expect("json");
    assert_eq!(body["lease"], serde_json::Value::Null);
}

#[test]
fn a_held_lease_keeps_the_server_alive_and_an_expired_one_stops() {
    // Spec 4.2: self-exit is "no page and no agent", and an agent that polls
    // every 90 seconds sends nothing in between that the idle clock can see.
    let s = InProcess::start_with_idle(Duration::from_millis(200));
    take(&s, "claude");
    std::thread::sleep(Duration::from_millis(700));
    assert_eq!(
        status_of(&s.get("/healthz", &[])),
        200,
        "an attached agent must not let the daemon exit under it"
    );

    s.age_lease(lease::TTL + Duration::from_secs(1));
    wait_for(
        || !s.serving(),
        "an expired lease must stop holding the daemon open",
    );
}
