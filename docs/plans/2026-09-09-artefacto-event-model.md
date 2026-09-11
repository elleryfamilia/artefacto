# artefacto Event Model and Agent Verbs Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> ## Implementation status — read this before executing anything
>
> **Tasks 1 and 2 are built and green** on the `feat/server-spine` branch.
> Tasks 3 through 8 are not started.
>
> **Where the code deliberately diverges from this plan:**
>
> - **Task 2 is wrong as written.** It puts ingress on WebSocket messages. The
>   socket is outbound only (see plan 2a's status note), so commands arrive at
>   `POST /a/<artifact>/cmd` instead, guarded by the cookie and a strict
>   `Origin`. The protocol, the assigned ids and the dedupe rule are unchanged;
>   only the transport moved. See `src/server/ingress.rs`.
> - **Commands carry `opened_revision`**, and the artifact comes from the URL
>   rather than the command body.
> - **The socket's first frame is `artefacto.hello/1`**, carrying the page's own
>   id so it can name itself in its POSTs and be skipped by the broadcast.
> - **`Committer` in `src/server/http.rs`** is the mutation gate this plan calls
>   for. Every remaining task must append through it; nothing else may touch the
>   log.
>
> **Still open from the reviews, and still true of tasks 3-8:** the lease
> check-then-act race, `append_all`'s atomicity (one `write_all` is not a
> transaction), and the missing record of which frame was last offered to a
> session, without which "the next call acknowledges the previous frame" cannot
> be implemented.

**Goal:** Turn the transport from plan 2a into a working review loop. The server folds its whole state from the log, accepts the reviewer's commands over the page socket, leases itself to one agent at a time, delivers frames against a persisted cursor, and serves `push`, `events`, `await`, `ack`, `reply`, and `resolve`. When this plan is done, an agent can publish a plan, hear a reviewer's question, answer it, and receive the submitted feedback document — all driven by a fake page client, because the real page is plan 3.

**Architecture:** Every piece of live state is a pure fold over the append-only log, rebuilt on start; nothing is memory-only. The page speaks a small command protocol over its WebSocket; the server assigns ids, suppresses duplicates, appends, and broadcasts. Agents hold a lease identified by a session token with a generation, and receive frames computed **only** from their cursor — never from a side buffer.

**Tech Stack:** Unchanged from plan 2a. No new dependencies.

**Spec:** `docs/specs/2026-09-06-artefacto-design.md`. Sections 4.2, 4.3, 5, 6, and 7 are this plan's contract.

**Depends on:** `docs/plans/2026-09-09-artefacto-server-transport.md` (plan 2a) must be complete and green. This plan adds to `Shared`, `Core`, and `EventLog`; it does not revisit the transport, the guards, the daemon, or the CSP.

## What this plan is defending against

An earlier combined plan drew 42 findings from two independent reviews. Plan 2a fixed the transport half. The findings below are the ones that land here, and each has a task and a test that fails if it regresses.

- **Acknowledging an event appended a record that was itself delivered.** The cursor was persisted as a log event, which is right, but nothing excluded it from delivery. In digest mode the agent received its own bookkeeping; in live mode each flush produced an event that triggered the next flush. Plan 2a defined `event::is_internal` for exactly this; Task 4 must use it.
- **The agent received its own events back.** Delivery never filtered on `actor`, so `revision.published` and the agent's own replies rode along in its next frame.
- **The current lease holder could not re-poll.** `acquire` refused any live lease without `--takeover`, and `await`/`events` had no `--session`, so the second poll of the loop the lease exists to support exited 6.
- **A lease tied to the `await` process was dead on arrival.** Liveness was a pid check, and `await` is a short-lived subprocess: the moment it returned, its own token stopped validating, so `reply` and `push` could never use it.
- **Nothing turned page input into events.** Threads, answers, reviewed marks, chat and submit had no handler at all, and five tasks' tests stood on helpers nothing implemented.
- **Push claimed an atomic batch the log could not provide**, and the submit handler was ordered impossibly: append the event, then compute the path, then put the path into the already-appended event.
- **`acquire` had a check-then-act race** that could hand out a token already superseded, returned as `Ok`.

## Global Constraints

These extend plan 2a's; both apply.

- Every task ends green on `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all`, including everything from plans 1 and 2a.
- **All live state folds from the log.** Revisions, threads, answers, reviewed marks, delivery cursors, **and the lease**. Nothing that survives a restart lives only in memory. Plan 2a's constraint said this; the earlier draft then kept the lease in memory anyway.
- **Lock order stays `log` → `core` → `sockets`**, and no blocking operation happens while `core` or `sockets` is held. `std::sync::Mutex` is not reentrant, so every lock-taking function has an inner `_locked` variant.
- **Delivery is computed only from the cursor.** There is no passive buffer. Any change that introduces a second source for a frame's contents is wrong, however convenient.
- **A frame carries only deliverable events**: not `is_internal`, and not `actor: Agent`. An agent never hears itself.
- **Delivery is at-least-once.** Every handler must be safe to run twice. The skill tells the agent to check a thread before replying.
- **Ids are server-assigned and stable.** Threads are `c-<n>` per artifact, never renumbered (spec 6.6). The page never chooses an id.
- **Every page write carries a client-generated `client_id`**, and the server ignores one it has already committed. A disconnect around submit otherwise forces a choice between losing the review and duplicating every comment in it.
- **No `Instant` arithmetic that can panic.** `Instant::now() - age` and `a -= b` panic on underflow, which on a machine booted minutes ago is reachable. Use `checked_sub`, `checked_add`, and `saturating_duration_since` everywhere.
- **Reviewer text is data, never markup.** Comment, reply, chat and answer text is escaped on render and delivered as a JSON string. Only plan markdown goes through the sanitizer.
- Exit codes: **4** no server, **6** lease held or superseded token, **7** stale `base_revision`, **2** usage.
- `await` exits **0** for every non-error outcome.

## A spec change this plan requires

Spec 6.2 classifies `reviewer.back` as **active**; spec 5's `await` status table has no `back` row. Plan 2a followed 6.2. This plan therefore returns `back` as an `await` status, and **spec section 5's table needs a `back` row**. Make that edit to the spec when this plan is executed; it is the one place the two documents disagree.

## Surface not in the spec

Three things the earlier draft invented without saying so. Decided here rather than discovered later:

- `/healthz` and `cursor.acked` are **internal**. They are not protocol: `/healthz` is unauthenticated and trivial, and `cursor.acked` is filtered from delivery by `is_internal`. Neither needs a spec entry.
- `reply --nudge` is **user-facing** and does need one. Spec 6.3 requires a `nudge` event but spec 5's `reply` surface has no flag for it. This plan defines `--nudge`; add it to spec section 5.

## File Structure

| file | responsibility |
|---|---|
| `src/server/fold.rs` | the pure fold: log to `Review` |
| `src/server/review.rs` | `Review`, `Artifact`, `Thread`, `Answer` types |
| `src/server/ingress.rs` | the page command protocol: parse, validate, assign ids, dedupe |
| `src/server/lease.rs` | lease, session tokens, generations, takeover |
| `src/server/delivery.rs` | cursors, deliverable filter, frame assembly, live-mode timing |
| `src/server/feedback.rs` | the `artefacto.feedback/1` document and where it is written |
| `src/commands/agent.rs` | `events`, `await`, `ack`, `reply`, `resolve` |
| `src/commands/plan.rs` | existing, plus `push` |
| `tests/server_fold.rs` | fold and restart equivalence |
| `tests/server_ingress.rs` | page commands, ids, dedupe, validation |
| `tests/server_lease.rs` | lease, TTL, takeover, superseded token, re-poll |
| `tests/server_delivery.rs` | cursors, at-least-once, digest and live rules |
| `tests/server_agent.rs` | `events`, `await`, `ack` |
| `tests/server_push.rs` | `push`, base revision, resolutions |
| `tests/server_loop.rs` | the whole loop end to end |

---

### Task 1: `Review` and the fold

**Why this task exists:** spec 4.2 says all state is a fold over the log, and 6.7 requires a restarted server to rebuild every piece of it. The earlier draft listed `fold.rs` in its file table and never created it, so `Shared::new` replayed nothing: revisions, threads, answers, reviewed marks, and the lease all began empty on every start, and the log-sufficiency test it claimed could not have passed.

**Files:**
- Create: `src/server/review.rs`, `src/server/fold.rs`, `tests/server_fold.rs`
- Modify: `src/server/http.rs` (`Core` gains `review`), `src/server/mod.rs`

**Interfaces:**
- Consumes: `event::{Event, Actor}`, `log::EventLog` (both plan 2a).
- Produces:
  - `pub struct Review { pub artifacts: BTreeMap<String, Artifact>, pub cursors: BTreeMap<String, u64>, pub lease: Option<LeaseRecord>, pub lease_generation: u64, pub committed: BTreeSet<String> }`
  - `pub struct Artifact { pub id, pub revision, pub plan, pub plan_hash, pub source_path, pub threads, pub next_thread_n, pub answers, pub reviewed, pub submitted }`
  - `pub struct Thread { pub id, pub target, pub quote, pub status, pub blocking, pub messages }`
  - `pub enum ThreadStatus { Open, Changed, Declined, Unanchored }`
  - `pub fn fold(events: &[Event]) -> Review`
  - `pub fn apply(review: &mut Review, event: &Event)` — one event; `fold` is a loop over it

**`committed`** holds every `client_id` the server has already acted on. It folds from the log like everything else, so duplicate suppression survives a restart — which is the only way it is worth anything, since the reconnect that retries a submit is exactly the case where the server may have restarted.

**Why `fold` is a free function over a slice:** it takes no locks and touches no I/O, so it is testable without a server and cannot violate the lock order. `Shared::new` calls it once with `log.since(0)`.

- [ ] **Step 1: Write the failing test**

Create `tests/server_fold.rs`:

```rust
mod support;
use artefacto::server::event::{Actor, Event};
use artefacto::server::fold::fold;
use artefacto::server::review::ThreadStatus;
use support::{ev, Harness};

#[test]
fn an_empty_log_folds_to_an_empty_review() {
    let r = fold(&[]);
    assert!(r.artifacts.is_empty());
    assert!(r.cursors.is_empty());
    assert!(r.lease.is_none());
    assert_eq!(r.lease_generation, 0);
}

#[test]
fn a_revision_event_creates_the_artifact_and_carries_the_whole_plan() {
    let events = vec![ev(
        1,
        Actor::Agent,
        "revision.published",
        serde_json::json!({
            "plan": { "format": "artefacto.plan/1", "meta": { "id": "demo" }, "phases": [] },
            "plan_hash": "sha256:abc",
            "source_path": "/tmp/demo.json",
            "summary": "first"
        }),
    )];
    let r = fold(&events);
    let a = r.artifacts.get("plan:demo").expect("the artifact exists");
    assert_eq!(a.revision, 1);
    assert_eq!(a.plan_hash, "sha256:abc");
    assert!(a.plan["meta"]["id"] == "demo", "the body can be rebuilt from the log alone");
}

#[test]
fn threads_get_stable_ids_that_are_never_renumbered() {
    let events = vec![
        ev(1, Actor::Agent, "revision.published", plan_data()),
        ev(2, Actor::Reviewer, "thread.opened",
           serde_json::json!({ "thread": "c-1", "ref": "task:t-a", "text": "why?" })),
        ev(3, Actor::Reviewer, "thread.opened",
           serde_json::json!({ "thread": "c-2", "ref": "task:t-b", "text": "and this?" })),
        ev(4, Actor::Reviewer, "thread.deleted", serde_json::json!({ "thread": "c-1" })),
        ev(5, Actor::Reviewer, "thread.opened",
           serde_json::json!({ "thread": "c-3", "ref": "task:t-c", "text": "third" })),
    ];
    let r = fold(&events);
    let a = &r.artifacts["plan:demo"];
    let ids: Vec<&str> = a.threads.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, vec!["c-2", "c-3"], "c-1 was deleted; c-3 did not reuse its number");
    assert_eq!(a.next_thread_n, 4, "the counter never goes backwards");
}

#[test]
fn a_resolution_sets_the_thread_status() {
    let events = vec![
        ev(1, Actor::Agent, "revision.published", plan_data()),
        ev(2, Actor::Reviewer, "thread.opened",
           serde_json::json!({ "thread": "c-1", "ref": "task:t-a", "text": "why?" })),
        ev(3, Actor::Agent, "thread.resolved",
           serde_json::json!({ "thread": "c-1", "status": "declined", "note": "out of scope" })),
    ];
    let r = fold(&events);
    let t = &r.artifacts["plan:demo"].threads[0];
    assert!(matches!(t.status, ThreadStatus::Declined));
    assert_eq!(t.messages.last().unwrap().text, "out of scope");
}

#[test]
fn a_thread_whose_ref_is_gone_after_a_push_becomes_unanchored() {
    let events = vec![
        ev(1, Actor::Agent, "revision.published", plan_data()),
        ev(2, Actor::Reviewer, "thread.opened",
           serde_json::json!({ "thread": "c-1", "ref": "task:t-gone", "text": "?" })),
        ev(3, Actor::Agent, "revision.published", plan_data()),
    ];
    let r = fold(&events);
    let t = &r.artifacts["plan:demo"].threads[0];
    assert!(
        matches!(t.status, ThreadStatus::Unanchored),
        "nothing a reviewer wrote is ever silently dropped; it is listed instead"
    );
}

#[test]
fn cursors_and_the_lease_fold_from_the_log() {
    let events = vec![
        ev(1, Actor::Server, "lease.taken",
           serde_json::json!({ "agent": "claude", "generation": 1, "token": "t1", "mode": "waiting" })),
        ev(2, Actor::Server, "cursor.acked",
           serde_json::json!({ "agent": "claude", "acked_seq": 7 })),
    ];
    let r = fold(&events);
    assert_eq!(r.cursors["claude"], 7, "an agent that restarts resumes where it left off");
    let lease = r.lease.expect("the lease is state like any other");
    assert_eq!(lease.generation, 1);
    assert_eq!(r.lease_generation, 1, "the generation must not restart at zero");
}

#[test]
fn a_released_lease_folds_to_none_but_keeps_the_generation() {
    let events = vec![
        ev(1, Actor::Server, "lease.taken",
           serde_json::json!({ "agent": "claude", "generation": 1, "token": "t1", "mode": "waiting" })),
        ev(2, Actor::Server, "lease.released", serde_json::json!({ "generation": 1 })),
    ];
    let r = fold(&events);
    assert!(r.lease.is_none());
    assert_eq!(
        r.lease_generation, 1,
        "a token from generation 1 must never validate again, even after a release"
    );
}

#[test]
fn committed_client_ids_fold_so_dedupe_survives_a_restart() {
    let events = vec![
        ev(1, Actor::Agent, "revision.published", plan_data()),
        ev(2, Actor::Reviewer, "thread.opened",
           serde_json::json!({ "thread": "c-1", "ref": "task:t-a", "text": "x", "client_id": "cid-1" })),
    ];
    let r = fold(&events);
    assert!(r.committed.contains("cid-1"), "a retry after a restart must not duplicate the comment");
}

#[test]
fn folding_is_the_only_source_of_state_after_a_restart() {
    let h = Harness::start();
    let session = h.take_lease("claude");
    h.push(&["--session", &session]);
    let thread = h.open_thread("task:t-a", "why a trait here?");
    let before = h.review_snapshot();

    let h = h.restart();
    assert_eq!(
        h.review_snapshot(),
        before,
        "kill the server, start it again with no other state, and everything comes back"
    );
    assert_eq!(h.thread_status(&thread), "open");
}
```

`plan_data()` and `ev()` are harness helpers; both are in Appendix A. `review_snapshot` returns a normalized, comparable projection of the fold — not the struct itself, so the comparison does not depend on field order or on `Instant`s.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test server_fold`
Expected: FAIL to compile, `could not find 'fold' in 'server'`.

- [ ] **Step 3: Implement `review.rs`**

The types above, with `#[derive(Debug, Clone, PartialEq)]` throughout so tests can compare snapshots and use `expect_err`. `Thread::messages` is a `Vec<ThreadMessage { actor: Actor, text: String, ts: String }>`. `Artifact::reviewed` is a `BTreeSet<String>` of refs, so the snapshot is order-stable.

- [ ] **Step 4: Implement the fold**

```rust
//! The log, folded into live state.
//!
//! A pure function over a slice: no locks, no I/O. `Shared::new` calls it once
//! with `log.since(0)`, and `apply` runs for each newly appended event so the
//! in-memory `Review` stays exactly what a fresh fold would produce.
//!
//! The invariant worth stating: **`fold(all_events)` must equal the result of
//! `apply`-ing those events one at a time.** `tests/server_fold.rs` pins it,
//! because a divergence would mean the server behaves differently before and
//! after a restart, which is the hardest class of bug to reproduce.

pub fn fold(events: &[Event]) -> Review {
    let mut review = Review::default();
    for event in events {
        apply(&mut review, event);
    }
    review
}

pub fn apply(review: &mut Review, event: &Event) {
    // Every branch records the client_id first, so a duplicate is suppressed
    // whether or not the specific handler cares about it.
    if let Some(cid) = event.data.get("client_id").and_then(|v| v.as_str()) {
        review.committed.insert(cid.to_string());
    }
    match event.r#type.as_str() {
        "revision.published" => apply_revision(review, event),
        "thread.opened" => apply_thread_opened(review, event),
        "thread.replied" | "thread.edited" | "thread.deleted" => apply_thread_change(review, event),
        "thread.resolved" => apply_resolution(review, event),
        "question.answered" => apply_answer(review, event),
        "element.reviewed" => apply_reviewed(review, event),
        "review.submitted" => apply_submitted(review, event),
        "cursor.acked" => apply_cursor(review, event),
        "lease.taken" => apply_lease_taken(review, event),
        "lease.released" => apply_lease_released(review, event),
        // Chat, presence and nudges are delivered, not folded: they carry no
        // state a restart needs to rebuild.
        _ => {}
    }
}
```

`apply_revision` replaces the artifact's plan, hash and revision, then walks every thread and marks as `Unanchored` any whose `target` no longer resolves in the new plan. That is the one place a push can change a thread, and it is why re-anchoring is a fold concern rather than a push concern.

`apply_lease_released` clears `lease` but leaves `lease_generation`, so a token from a released generation can never validate again.

- [ ] **Step 5: Wire it into `Shared`**

`Core` gains `pub review: Review`. `Shared::new` folds once:

```rust
let log = EventLog::open(dir)?;
let review = crate::server::fold::fold(log.since(0));
```

Every later append goes through one helper so the two can never drift:

```rust
/// Appends and folds under the correct lock order: `log` first, then `core`,
/// never held together with `sockets`. Returns the event so the caller can
/// broadcast it after both locks are released.
pub fn commit(shared: &Shared, artifact: &str, revision: u32, actor: Actor, kind: &str, data: serde_json::Value) -> Result<Event> {
    let event = { shared.log.lock().unwrap().append(artifact, revision, actor, kind, data)? };
    {
        let mut core = shared.core.lock().unwrap();
        crate::server::fold::apply(&mut core.review, &event);
    }
    Ok(event)
}
```

- [ ] **Step 6: Run the tests, verify the gate, commit**

Run: `cargo test --test server_fold`
Expected: PASS, 9 tests.

```bash
cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --all
git add src/server/review.rs src/server/fold.rs src/server/http.rs src/server/mod.rs tests/server_fold.rs
git commit -m "feat(server): fold every piece of live state from the event log"
```

---

### Task 2: Page ingress

**Why this task exists:** this is the subsystem the earlier plan forgot entirely. Its socket handler said inbound messages "are logged and dropped", and no task ever gave them meaning — so `thread.opened`, `question.answered`, `element.reviewed`, `chat.sent` and `review.submitted` had no production handler, while five later tasks' tests called helpers that assumed one. Without this task there is no review, only a page.

**Files:**
- Create: `src/server/ingress.rs`, `tests/server_ingress.rs`
- Modify: `src/server/socket.rs`, `src/server/mod.rs`, `tests/support/mod.rs`

**Interfaces:**
- Consumes: `fold::{apply, commit}` (Task 1), `socket::broadcast` (plan 2a).
- Produces:
  - `pub enum Command` — one variant per page command, `serde` tagged on `cmd`
  - `pub fn handle_message(shared: &Arc<Shared>, page_id: u64, raw: &str) -> Reply`
  - `pub struct Reply { pub ok: bool, pub client_id: String, pub assigned: Option<String>, pub error: Option<String> }`
  - `pub fn next_thread_id(artifact: &Artifact) -> String`

**The protocol.** One JSON object per message, discriminated on `cmd`. Every mutating command carries a `client_id` the page generates:

| `cmd` | fields | becomes |
|---|---|---|
| `thread.open` | `client_id`, `ref`, `text`, `blocking`, `quote` | `thread.opened`, id assigned |
| `thread.reply` | `client_id`, `thread`, `text` | `thread.replied` |
| `thread.edit` | `client_id`, `thread`, `text` | `thread.edited` |
| `thread.delete` | `client_id`, `thread` | `thread.deleted` |
| `question.answer` | `client_id`, `question`, `text` | `question.answered` |
| `element.reviewed` | `client_id`, `ref`, `on` | `element.reviewed` |
| `chat.send` | `client_id`, `text`, optional `thread` | `chat.sent` |
| `review.submit` | `client_id`, `verdict`, `base_revision` | `review.submitted` |
| `ping` | — | reviewer activity, no event |

**Four rules that are not negotiable:**

1. **The server assigns thread ids.** `c-<n>` per artifact from `next_thread_n`, which only ever increases. A page-chosen id would let two tabs collide and would break spec 6.6's "never renumbered".
2. **A `client_id` already in `review.committed` is acknowledged and ignored.** Not re-appended. This is what makes a reconnect-and-retry safe.
3. **Text is data.** It is stored and delivered as a JSON string and never interpreted as markup anywhere.
4. **The broadcast goes to the other tabs, not the sender.** The sender gets a direct `Reply` carrying any assigned id. Sending it the broadcast too would make a second tab and the originating tab disagree about ordering.

- [ ] **Step 1: Write the failing tests**

Create `tests/server_ingress.rs`:

```rust
mod support;
use support::Harness;

#[test]
fn opening_a_thread_assigns_a_server_side_id() {
    let h = Harness::start();
    h.seed_artifact();
    let mut page = h.connect_page();
    let reply = page.request(serde_json::json!({
        "cmd": "thread.open", "client_id": "cid-1",
        "ref": "task:t-a", "text": "why a trait here?", "blocking": false
    }));
    assert_eq!(reply["ok"], true);
    assert_eq!(reply["assigned"], "c-1", "the page never chooses an id");
    assert_eq!(h.thread_status("c-1"), "open");
}

#[test]
fn ids_increase_and_are_never_reused() {
    let h = Harness::start();
    h.seed_artifact();
    let mut page = h.connect_page();
    assert_eq!(page.open_thread("cid-1", "task:t-a"), "c-1");
    assert_eq!(page.open_thread("cid-2", "task:t-b"), "c-2");
    page.request(serde_json::json!({ "cmd": "thread.delete", "client_id": "cid-3", "thread": "c-1" }));
    assert_eq!(page.open_thread("cid-4", "task:t-c"), "c-3", "c-1 is gone, not recycled");
}

#[test]
fn a_repeated_client_id_is_acknowledged_and_not_reapplied() {
    let h = Harness::start();
    h.seed_artifact();
    let mut page = h.connect_page();
    let first = page.request(serde_json::json!({
        "cmd": "thread.open", "client_id": "cid-1", "ref": "task:t-a", "text": "x", "blocking": false
    }));
    let seq_after_first = h.last_seq();
    let second = page.request(serde_json::json!({
        "cmd": "thread.open", "client_id": "cid-1", "ref": "task:t-a", "text": "x", "blocking": false
    }));
    assert_eq!(second["ok"], true, "a retry is not an error");
    assert_eq!(second["assigned"], first["assigned"], "and returns the same id");
    assert_eq!(h.last_seq(), seq_after_first, "nothing was appended the second time");
    assert_eq!(h.thread_count(), 1);
}

#[test]
fn dedupe_survives_a_restart() {
    let h = Harness::start();
    h.seed_artifact();
    {
        let mut page = h.connect_page();
        page.open_thread("cid-1", "task:t-a");
    }
    let h = h.restart();
    let mut page = h.connect_page();
    let again = page.request(serde_json::json!({
        "cmd": "thread.open", "client_id": "cid-1", "ref": "task:t-a", "text": "x", "blocking": false
    }));
    assert_eq!(again["assigned"], "c-1");
    assert_eq!(h.thread_count(), 1, "the reconnect-and-retry case is exactly the restart case");
}

#[test]
fn a_command_without_a_client_id_is_refused() {
    let h = Harness::start();
    h.seed_artifact();
    let mut page = h.connect_page();
    let r = page.request(serde_json::json!({ "cmd": "thread.open", "ref": "task:t-a", "text": "x" }));
    assert_eq!(r["ok"], false);
    assert!(r["error"].as_str().unwrap().contains("client_id"));
}

#[test]
fn an_unknown_command_is_refused_without_killing_the_socket() {
    let h = Harness::start();
    h.seed_artifact();
    let mut page = h.connect_page();
    let r = page.request(serde_json::json!({ "cmd": "drop.database", "client_id": "cid-1" }));
    assert_eq!(r["ok"], false);
    // The socket must still work afterwards.
    assert_eq!(page.open_thread("cid-2", "task:t-a"), "c-1");
}

#[test]
fn a_reply_to_a_missing_thread_is_refused() {
    let h = Harness::start();
    h.seed_artifact();
    let mut page = h.connect_page();
    let r = page.request(serde_json::json!({
        "cmd": "thread.reply", "client_id": "cid-1", "thread": "c-99", "text": "x"
    }));
    assert_eq!(r["ok"], false);
    assert!(r["error"].as_str().unwrap().contains("c-99"));
}

#[test]
fn oversized_text_is_refused_rather_than_logged() {
    let h = Harness::start();
    h.seed_artifact();
    let mut page = h.connect_page();
    let huge = "x".repeat(200_000);
    let r = page.request(serde_json::json!({
        "cmd": "thread.open", "client_id": "cid-1", "ref": "task:t-a", "text": huge, "blocking": false
    }));
    assert_eq!(r["ok"], false, "the log is append-only; junk in it is permanent");
}

#[test]
fn a_second_tab_sees_the_first_tabs_comment() {
    let h = Harness::start();
    h.seed_artifact();
    let mut a = h.connect_page();
    let mut b = h.connect_page();
    h.wait_for(|| h.page_count() == 2, "both tabs connected");

    a.open_thread("cid-1", "task:t-a");
    let frame = b.next_frame();
    assert_eq!(frame["events"][0]["type"], "thread.opened");
    assert_eq!(frame["events"][0]["data"]["thread"], "c-1");
}

#[test]
fn the_sender_does_not_receive_its_own_broadcast() {
    let h = Harness::start();
    h.seed_artifact();
    let mut a = h.connect_page();
    a.open_thread("cid-1", "task:t-a");
    assert!(
        a.no_frame_within(std::time::Duration::from_millis(300)),
        "the sender already got a direct reply; a broadcast too would double-count it"
    );
}

#[test]
fn reviewer_text_is_stored_as_text_not_markup() {
    let h = Harness::start();
    h.seed_artifact();
    let mut a = h.connect_page();
    let mut b = h.connect_page();
    h.wait_for(|| h.page_count() == 2, "both tabs connected");

    let payload = "<img src=x onerror=alert(1)>";
    a.request(serde_json::json!({
        "cmd": "thread.open", "client_id": "cid-1", "ref": "task:t-a",
        "text": payload, "blocking": false
    }));

    // Assert against the JSON the socket actually sends, not against a
    // rendered body. The earlier draft asserted against a document that never
    // contained comments at all, so its escaping test could not fail.
    let frame = b.next_frame();
    let delivered = frame["events"][0]["data"]["text"].as_str().unwrap();
    assert_eq!(delivered, payload, "carried verbatim as a JSON string");
    let raw = frame.to_string();
    assert!(!raw.contains("<img src=x onerror=alert(1)>\""), "and never spliced into markup");
}

#[test]
fn a_ping_marks_reviewer_activity_and_appends_nothing() {
    let h = Harness::start();
    h.seed_artifact();
    let mut page = h.connect_page();
    let before = h.last_seq();
    page.send(serde_json::json!({ "cmd": "ping" }));
    h.wait_for(|| h.reviewer_active_recently(), "the ping should mark activity");
    assert_eq!(h.last_seq(), before, "a ping is not an event");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test server_ingress`
Expected: FAIL to compile, `could not find 'ingress' in 'server'`.

- [ ] **Step 3: Implement the protocol**

```rust
//! What the reviewer's page is allowed to say, and what the server does with it.
//!
//! Every command is validated before anything is appended. The log is
//! append-only, so a malformed or oversized message that reaches it is
//! permanent — validation is the only place to stop it.
//!
//! Lock discipline: takes `log` then `core` through `fold::commit`, releases
//! both, and only then broadcasts. It never holds a lock across a send.

use serde::Deserialize;

/// Reviewer text is bounded so a runaway page cannot fill the log. Generous
/// for a comment, far below anything that would matter for the file.
pub const MAX_TEXT: usize = 64 * 1024;

#[derive(Debug, Deserialize)]
#[serde(tag = "cmd")]
pub enum Command {
    #[serde(rename = "thread.open")]
    ThreadOpen { client_id: String, r#ref: String, text: String, #[serde(default)] blocking: bool, #[serde(default)] quote: String },
    #[serde(rename = "thread.reply")]
    ThreadReply { client_id: String, thread: String, text: String },
    #[serde(rename = "thread.edit")]
    ThreadEdit { client_id: String, thread: String, text: String },
    #[serde(rename = "thread.delete")]
    ThreadDelete { client_id: String, thread: String },
    #[serde(rename = "question.answer")]
    QuestionAnswer { client_id: String, question: String, text: String },
    #[serde(rename = "element.reviewed")]
    ElementReviewed { client_id: String, r#ref: String, on: bool },
    #[serde(rename = "chat.send")]
    ChatSend { client_id: String, text: String, #[serde(default)] thread: Option<String> },
    #[serde(rename = "review.submit")]
    ReviewSubmit { client_id: String, verdict: String, base_revision: u32 },
    #[serde(rename = "ping")]
    Ping,
}
```

`handle_message` does five things, in this order, and the order is the point:

1. Parse. A parse failure is a `Reply` with `ok: false` and a message, **never** a dropped connection: a page that cannot report an error is a page that silently loses a reviewer's comment.
2. Mark reviewer activity. Every command counts, not just `ping`.
3. Check `client_id` against `review.committed`. A hit returns the previously assigned id and appends nothing.
4. Validate: the thread or ref exists, the text is within `MAX_TEXT`, the verdict is one of the three.
5. `fold::commit`, then release the locks, then `broadcast_except(page_id, &frame)`.

`next_thread_id` reads `artifact.next_thread_n` and returns `c-<n>`; `fold` increments it. The id is assigned inside the same `core` guard that commits, so two tabs racing cannot receive the same one.

- [ ] **Step 4: Add `broadcast_except` to `socket.rs`**

Same shape as `broadcast` from plan 2a — clone the senders out under the lock, release, then send — skipping the originating page id.

- [ ] **Step 5: Run the tests, verify the gate, commit**

Run: `cargo test --test server_ingress`
Expected: PASS, 12 tests.

```bash
cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --all
git add src/server/ingress.rs src/server/socket.rs src/server/mod.rs tests/
git commit -m "feat(server): page command protocol with assigned ids and dedupe"
```

---

### Task 3: The lease and session tokens

**Why this task exists:** spec 4.2 makes the lease the thing that lets a poll-mode agent hold together across many short commands. Two of the review's fatal findings live here.

**What the earlier draft got wrong:**

- **The holder could not re-poll.** `acquire` returned `Held` whenever a live lease existed and `takeover` was false, with no check for "this is the same agent presenting its own token", and `await`/`events` had no `--session` flag to present one with. The second poll of the loop exited 6. Spec 4.2 says the token exists precisely "so a lease survives an `await` returning every 90 seconds instead of dropping and re-taking on every cycle".
- **Liveness was a pid check, and `await` is a short-lived subprocess.** Its pid was dead the instant the command returned, so the token it handed back stopped validating before `reply` or `push` could use it. The lease was unusable in the exact mode it was designed for.
- **Check-then-act.** `acquire` called `current`, which locked and unlocked, then locked again itself. Two agents could both observe no holder; the loser was handed a token that was already superseded, returned as `Ok`.

**The liveness decision, written down because the spec is ambiguous.** Spec 4.2 says an expired lease "or one whose recorded pid is dead" is released. That rule is about `events --follow`, which is a long-running process holding a connection. It cannot be about `await`, which exits between polls. So:

- **`Mode::Live`** (`events --follow`): liveness is the connection. A disconnect releases the lease immediately, and the recorded pid is checked.
- **`Mode::Waiting`** (`await`): liveness is the **TTL alone**. No pid is recorded, because there is no durable process to record. Five minutes of no agent call releases it.

**Files:**
- Create: `src/server/lease.rs`, `tests/server_lease.rs`
- Modify: `src/server/fold.rs`, `src/server/http.rs`, `src/server/mod.rs`

**Interfaces:**
- Consumes: `fold::commit` (Task 1), `state_dir::new_secret` (plan 2a).
- Produces:
  - `pub struct LeaseRecord { pub name: String, pub generation: u64, pub token: String, pub pid: Option<u32>, pub mode: Mode, pub taken_at: Instant }`
  - `pub enum Mode { Live, Waiting }`
  - `pub enum LeaseError { Held { holder: String, age_secs: u64 }, Superseded }`
  - `pub fn acquire(shared: &Shared, name: &str, mode: Mode, presenting: Option<&str>, takeover: bool) -> Result<LeaseRecord, LeaseError>`
  - `pub fn validate(shared: &Shared, token: &str) -> Result<LeaseRecord, LeaseError>`
  - `pub fn release(shared: &Shared, token: &str)`
  - `pub fn current(shared: &Shared) -> Option<LeaseRecord>`
  - `pub const TTL: Duration = Duration::from_secs(300);`

`acquire` takes `presenting`: the token the caller already holds, if any. That single parameter is what turns a refusal into a refresh.

- [ ] **Step 1: Write the failing tests**

Create `tests/server_lease.rs`:

```rust
mod support;
use artefacto::server::lease::{self, LeaseError, Mode};
use support::Harness;

#[test]
fn the_first_agent_takes_the_lease_and_it_is_logged() {
    let h = Harness::start();
    let l = lease::acquire(&h.shared, "claude", Mode::Waiting, None, false).unwrap();
    assert_eq!(l.generation, 1);
    assert!(!l.token.is_empty());
    assert_eq!(
        h.last_event_of_type("lease.taken")["data"]["agent"], "claude",
        "the lease is state, so it folds from the log like everything else"
    );
}

#[test]
fn the_holder_can_re_poll_with_its_own_token() {
    // This is the loop the lease exists to support. The earlier draft exited 6
    // on the second poll.
    let h = Harness::start();
    let first = lease::acquire(&h.shared, "claude", Mode::Waiting, None, false).unwrap();
    let second = lease::acquire(&h.shared, "claude", Mode::Waiting, Some(&first.token), false)
        .expect("presenting a valid token must refresh, not refuse");
    assert_eq!(second.token, first.token, "the same lease, not a new one");
    assert_eq!(second.generation, first.generation, "re-polling does not bump the generation");
}

#[test]
fn a_waiting_lease_survives_the_process_that_took_it() {
    // `await` is a short-lived subprocess: its pid is dead the moment it
    // returns. If liveness were a pid check, its own token would stop
    // validating before `reply` could use it.
    let h = Harness::start();
    let l = lease::acquire(&h.shared, "claude", Mode::Waiting, None, false).unwrap();
    assert!(l.pid.is_none(), "waiting mode records no pid, because there is no durable process");
    h.simulate_process_exit();
    lease::validate(&h.shared, &l.token).expect("the token must still work for reply and push");
}

#[test]
fn a_live_lease_records_a_pid_and_is_released_when_that_process_dies() {
    let h = Harness::start();
    let l = lease::acquire_live(&h.shared, "claude", 0, None, false).unwrap(); // pid 0 is never live
    assert_eq!(l.pid, Some(0));
    assert!(
        lease::current(&h.shared).is_none(),
        "a crashed --follow agent must not hold the lease until its TTL runs out"
    );
}

#[test]
fn a_second_agent_is_refused_and_told_who_holds_it() {
    let h = Harness::start();
    lease::acquire(&h.shared, "claude", Mode::Waiting, None, false).unwrap();
    match lease::acquire(&h.shared, "codex", Mode::Waiting, None, false) {
        Err(LeaseError::Held { holder, .. }) => assert_eq!(holder, "claude"),
        other => panic!("expected Held, got {other:?}"),
    }
}

#[test]
fn takeover_bumps_the_generation_and_supersedes_the_old_token() {
    let h = Harness::start();
    let first = lease::acquire(&h.shared, "claude", Mode::Waiting, None, false).unwrap();
    let second = lease::acquire(&h.shared, "codex", Mode::Waiting, None, true).unwrap();
    assert_eq!(second.generation, 2);
    assert!(matches!(lease::validate(&h.shared, &first.token), Err(LeaseError::Superseded)));
    assert!(lease::validate(&h.shared, &second.token).is_ok());
}

#[test]
fn a_superseded_token_cannot_re_acquire_by_presenting_itself() {
    let h = Harness::start();
    let first = lease::acquire(&h.shared, "claude", Mode::Waiting, None, false).unwrap();
    let _ = lease::acquire(&h.shared, "codex", Mode::Waiting, None, true).unwrap();
    assert!(
        matches!(
            lease::acquire(&h.shared, "claude", Mode::Waiting, Some(&first.token), false),
            Err(LeaseError::Superseded)
        ),
        "presenting a dead token is not a way back in"
    );
}

#[test]
fn an_expired_lease_is_released_without_needing_a_takeover() {
    let h = Harness::start();
    let l = lease::acquire(&h.shared, "claude", Mode::Waiting, None, false).unwrap();
    h.age_lease(lease::TTL + std::time::Duration::from_secs(1));
    assert!(lease::current(&h.shared).is_none());
    let next = lease::acquire(&h.shared, "codex", Mode::Waiting, None, false).unwrap();
    assert_eq!(next.generation, 2);
    assert!(matches!(lease::validate(&h.shared, &l.token), Err(LeaseError::Superseded)));
}

#[test]
fn any_agent_call_refreshes_the_ttl() {
    let h = Harness::start();
    let l = lease::acquire(&h.shared, "claude", Mode::Waiting, None, false).unwrap();
    h.age_lease(lease::TTL - std::time::Duration::from_secs(5));
    lease::validate(&h.shared, &l.token).expect("still inside the TTL");
    h.age_lease(lease::TTL - std::time::Duration::from_secs(5));
    lease::validate(&h.shared, &l.token).expect("the previous call refreshed it");
}

#[test]
fn the_generation_survives_a_restart() {
    let h = Harness::start();
    let first = lease::acquire(&h.shared, "claude", Mode::Waiting, None, false).unwrap();
    let h = h.restart();
    let next = lease::acquire(&h.shared, "codex", Mode::Waiting, None, false).unwrap();
    assert_eq!(next.generation, 2, "a restart must not reset the generation to zero");
    assert!(
        matches!(lease::validate(&h.shared, &first.token), Err(LeaseError::Superseded)),
        "a pre-restart token must never validate again"
    );
}

#[test]
fn aging_does_not_panic_on_a_freshly_booted_machine() {
    // `taken_at -= by` panics on Instant underflow. On a container booted less
    // than five minutes ago, that is reachable in the TTL tests themselves.
    let h = Harness::start();
    lease::acquire(&h.shared, "claude", Mode::Waiting, None, false).unwrap();
    h.age_lease(std::time::Duration::from_secs(86_400));
    assert!(lease::current(&h.shared).is_none());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test server_lease`
Expected: FAIL to compile, `could not find 'lease' in 'server'`.

- [ ] **Step 3: Implement the lease as one critical section**

```rust
//! One agent acts at a time. The token, not the process, is the identity.
//!
//! Lock discipline: the public functions take `core` once and do all their
//! work inside it, through `*_locked` helpers. `std::sync::Mutex` is not
//! reentrant, and an earlier draft's `acquire` called a public `current` that
//! locked and unlocked before `acquire` locked again — a check-then-act race
//! where two agents could both see no holder and the loser received a token
//! that was already superseded, returned as `Ok`.

pub const TTL: Duration = Duration::from_secs(300);

/// Collects an expired or dead lease. Takes the guard, so it composes with
/// the callers below instead of racing them.
fn collect_locked(review: &mut Review, now: Instant) {
    let dead = match &review.lease {
        None => false,
        Some(l) => {
            let expired = now.saturating_duration_since(l.taken_at) > TTL;
            // A pid is recorded only in Live mode; see the module docs.
            let gone = l.pid.is_some_and(|p| !state_dir::is_alive(p));
            expired || gone
        }
    };
    if dead {
        review.lease = None;
    }
}

pub fn acquire(
    shared: &Shared,
    name: &str,
    mode: Mode,
    presenting: Option<&str>,
    takeover: bool,
) -> Result<LeaseRecord, LeaseError> {
    let now = Instant::now();
    let outcome = {
        let mut core = shared.core.lock().unwrap();
        collect_locked(&mut core.review, now);

        match (&core.review.lease, presenting) {
            // The holder presenting its own token: refresh in place. This is
            // the poll loop, and it must not bump the generation or mint a
            // new token, or `reply` would be holding a stale one.
            (Some(l), Some(t)) if l.token == t => {
                let mut refreshed = l.clone();
                refreshed.taken_at = now;
                refreshed.mode = mode;
                core.review.lease = Some(refreshed.clone());
                Outcome::Refreshed(refreshed)
            }
            // A token that is not the current one is superseded, whether or
            // not a lease is held right now.
            (_, Some(_)) => return Err(LeaseError::Superseded),
            (Some(l), None) if !takeover => {
                return Err(LeaseError::Held {
                    holder: l.name.clone(),
                    age_secs: now.saturating_duration_since(l.taken_at).as_secs(),
                })
            }
            _ => Outcome::Fresh(core.review.lease_generation + 1),
        }
    };
    match outcome {
        Outcome::Refreshed(l) => Ok(l),
        // A fresh lease is a state change, so it goes through the log. The
        // generation was computed under the guard above and is carried here,
        // so two racers cannot both mint generation N.
        Outcome::Fresh(generation) => commit_lease(shared, name, mode, generation),
    }
}
```

`commit_lease` appends `lease.taken` with `{agent, generation, token, mode, pid}` through `fold::commit`, which applies it to the `Review` under the same discipline. `validate` takes the guard once, collects, compares the token, refreshes `taken_at`, and returns — never in two separate lockings. `release` appends `lease.released`.

**Cursor inheritance on takeover, decided here:** cursors are keyed by agent **name**. A takeover under a *different* name would otherwise start from that name's cursor — usually zero — and replay every passive event the previous agent already acknowledged. So a fresh lease whose name has no cursor **inherits the outgoing lease's cursor**. A name that does have one keeps it, because that is a genuine resume. Task 4 tests this.

- [ ] **Step 4: Add the lease to the fold**

`apply_lease_taken` sets `review.lease` and raises `review.lease_generation`. `apply_lease_released` clears `review.lease` and leaves the generation. `taken_at` is not in the log — a restart starts the TTL fresh, which is correct: the agent has to call again anyway, and a wall-clock timestamp would be wrong across a suspend.

- [ ] **Step 5: Run the tests, verify the gate, commit**

Run: `cargo test --test server_lease`
Expected: PASS, 11 tests.

```bash
cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --all
git add src/server/lease.rs src/server/fold.rs src/server/http.rs src/server/mod.rs tests/server_lease.rs
git commit -m "feat(server): lease with token refresh, logged generation, and mode-aware liveness"
```

---

### Task 4: Delivery cursors

**Why this task exists:** spec 6.4 contains the one rule that is easy to get subtly wrong and impossible to notice by hand: there is **no separate passive buffer**, because a frame's contents are always computed from the cursor. The earlier draft honoured that and then broke it from the other end, by putting bookkeeping into the stream the cursor reads.

**What the earlier draft got wrong:**

- `ack` appended `cursor.acked` to the same log `frame_since` read, and nothing filtered it. The agent received its own bookkeeping; in live mode each flush produced an event that triggered the next flush; and the log grew by one event per poll cycle forever.
- Delivery never filtered on `actor`, so `revision.published`, the agent's own `thread.replied`, and its own presence events all rode along in its next frame.
- `ack` accepted a cursor going backwards or arbitrarily into the future, ignored the append's error, and updated a different field from the one `status` read.
- Two representations of the same number: `Lease.acked_seq`, never updated, and `Core.cursors`, actually written. Spec 5 requires `status --json` to print "each lease's `acked_seq`", so the ambiguity reached the output.

**Files:**
- Create: `src/server/delivery.rs`, `tests/server_delivery.rs`
- Modify: `src/server/mod.rs`

**Interfaces:**
- Consumes: `event::{is_active, is_internal}` (plan 2a), `fold::commit` (Task 1), `lease` (Task 3).
- Produces:
  - `pub fn deliverable(event: &Event) -> bool`
  - `pub fn frame_since(shared: &Shared, cursor: u64) -> Option<Frame>`
  - `pub fn passive_since(shared: &Shared, cursor: u64) -> Vec<Event>`
  - `pub fn ack(shared: &Shared, name: &str, seq: u64) -> anyhow::Result<()>`
  - `pub fn cursor_for(shared: &Shared, name: &str) -> u64`

**There is exactly one cursor representation:** `Review.cursors`, keyed by agent name, folded from `cursor.acked`. `LeaseRecord` has no `acked_seq` field. `status --json` reads `Review.cursors`.

- [ ] **Step 1: Write the failing tests**

Create `tests/server_delivery.rs`:

```rust
mod support;
use artefacto::server::delivery;
use support::Harness;

#[test]
fn a_frame_stops_at_the_first_active_event() {
    let h = Harness::start();
    h.seed_artifact();
    h.log_passive("thread.opened");
    h.log_passive("question.answered");
    h.log_active("chat.sent");
    h.log_active("review.submitted");

    let f = delivery::frame_since(&h.shared, 0).expect("something to deliver");
    assert_eq!(f.events.len(), 3, "the passive events before it ride along");
    assert_eq!(f.events.last().unwrap().r#type, "chat.sent", "the earliest active event ends it");
}

#[test]
fn passive_events_alone_produce_no_frame_in_digest_mode() {
    let h = Harness::start();
    h.seed_artifact();
    h.log_passive("thread.opened");
    h.log_passive("element.reviewed");
    assert!(delivery::frame_since(&h.shared, 0).is_none());
}

#[test]
fn internal_records_are_never_delivered() {
    // The earlier draft's own test failed on this: ack appended cursor.acked,
    // which is passive, so it rode along in the very next frame.
    let h = Harness::start();
    h.seed_artifact();
    h.log_passive("thread.opened");
    h.log_active("chat.sent");

    let first = delivery::frame_since(&h.shared, 0).unwrap();
    delivery::ack(&h.shared, "claude", first.seq).unwrap();

    h.log_active("chat.sent");
    let second = delivery::frame_since(&h.shared, delivery::cursor_for(&h.shared, "claude")).unwrap();
    assert_eq!(second.events.len(), 1, "no bookkeeping, no re-delivered passive event");
    assert_eq!(second.events[0].r#type, "chat.sent");
}

#[test]
fn the_agent_never_receives_its_own_events() {
    let h = Harness::start();
    h.seed_artifact();
    let session = h.take_lease("claude");
    h.push(&["--session", &session]);              // revision.published, actor agent
    h.run_cli(&["reply", "--session", &session, "a note"]); // thread.replied, actor agent
    h.log_active("chat.sent");                      // actor reviewer

    let f = delivery::frame_since(&h.shared, 0).unwrap();
    assert!(
        f.events.iter().all(|e| e.actor != artefacto::server::event::Actor::Agent),
        "spec 7 has the agent scanning frames for chat it must answer, not for its own output"
    );
}

#[test]
fn an_unacked_frame_is_delivered_again() {
    let h = Harness::start();
    h.seed_artifact();
    h.log_active("chat.sent");
    let cursor = delivery::cursor_for(&h.shared, "claude");
    let first = delivery::frame_since(&h.shared, cursor).unwrap();
    // The agent dies here, before acking.
    let again = delivery::frame_since(&h.shared, cursor).unwrap();
    assert_eq!(first.seq, again.seq, "at-least-once: a crash replays rather than loses");
}

#[test]
fn the_cursor_survives_a_restart() {
    let h = Harness::start();
    h.seed_artifact();
    h.log_active("chat.sent");
    delivery::ack(&h.shared, "claude", h.last_seq()).unwrap();
    let expected = delivery::cursor_for(&h.shared, "claude");

    let h = h.restart();
    assert_eq!(
        delivery::cursor_for(&h.shared, "claude"),
        expected,
        "an agent that restarts with no memory resumes where it left off"
    );
}

#[test]
fn a_backwards_ack_is_refused() {
    let h = Harness::start();
    h.seed_artifact();
    h.log_active("chat.sent");
    let top = h.last_seq();
    delivery::ack(&h.shared, "claude", top).unwrap();
    assert!(
        delivery::ack(&h.shared, "claude", top - 1).is_err(),
        "a cursor only moves forward; going back would replay acknowledged work"
    );
}

#[test]
fn an_ack_past_the_end_of_the_log_is_refused() {
    let h = Harness::start();
    h.seed_artifact();
    assert!(
        delivery::ack(&h.shared, "claude", h.last_seq() + 500).is_err(),
        "acknowledging what has not happened would silently skip it"
    );
}

#[test]
fn a_takeover_under_a_new_name_inherits_the_cursor() {
    let h = Harness::start();
    h.seed_artifact();
    h.log_active("chat.sent");
    let session = h.take_lease("claude");
    delivery::ack(&h.shared, "claude", h.last_seq()).unwrap();
    let _ = session;

    let _codex = h.take_lease_with_takeover("codex");
    assert_eq!(
        delivery::cursor_for(&h.shared, "codex"),
        delivery::cursor_for(&h.shared, "claude"),
        "otherwise codex starts at zero and replays everything claude already handled"
    );
}

#[test]
fn a_named_agent_that_returns_keeps_its_own_cursor() {
    let h = Harness::start();
    h.seed_artifact();
    h.log_active("chat.sent");
    h.take_lease("claude");
    delivery::ack(&h.shared, "claude", h.last_seq()).unwrap();
    let claude_cursor = delivery::cursor_for(&h.shared, "claude");

    h.take_lease_with_takeover("codex");
    h.log_active("chat.sent");
    delivery::ack(&h.shared, "codex", h.last_seq()).unwrap();

    h.take_lease_with_takeover("claude");
    assert_eq!(
        delivery::cursor_for(&h.shared, "claude"),
        claude_cursor,
        "a name that already has a cursor is resuming, not inheriting"
    );
}

#[test]
fn a_failed_append_surfaces_rather_than_being_swallowed() {
    let h = Harness::start();
    h.seed_artifact();
    h.poison_log();
    assert!(
        delivery::ack(&h.shared, "claude", h.last_seq()).is_err(),
        "an ack that did not persist must not be reported as success"
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test server_delivery`
Expected: FAIL to compile, `could not find 'delivery' in 'server'`.

- [ ] **Step 3: Implement delivery**

```rust
//! What an agent receives, and when.
//!
//! **There is no passive buffer.** A frame's contents are always computed from
//! the cursor, so the same passive event cannot be delivered once by a poll
//! and again by the next wake-up. Every change here must preserve that.
//!
//! Two filters guard the stream, and both exist because of a real bug:
//!   - `is_internal` keeps control records out. Acknowledging a frame appends
//!     `cursor.acked`; without this filter the agent receives its own
//!     bookkeeping, and in live mode each flush triggers the next one.
//!   - the `actor` filter keeps the agent from hearing itself.

pub fn deliverable(event: &Event) -> bool {
    !crate::server::event::is_internal(&event.r#type) && event.actor != Actor::Agent
}

/// Everything after `cursor` up to and **including** the first active event.
/// `None` when nothing active is waiting: in digest mode passive traffic does
/// not wake the agent.
///
/// Takes `log` only, and never while holding `core`.
pub fn frame_since(shared: &Shared, cursor: u64) -> Option<Frame> {
    let log = shared.log.lock().unwrap();
    let pending: Vec<Event> = log.since(cursor).iter().filter(|e| deliverable(e)).cloned().collect();
    let stop = pending.iter().position(|e| is_active(&e.r#type))?;
    Some(Frame::of(pending[..=stop].to_vec()))
}

/// The cursor is persisted as a log record, because spec 4.2 says every piece
/// of state folds from the log — including this one. `is_internal` is what
/// keeps that record from being delivered.
pub fn ack(shared: &Shared, name: &str, seq: u64) -> Result<()> {
    {
        let core = shared.core.lock().unwrap();
        let current = core.review.cursors.get(name).copied().unwrap_or(0);
        if seq < current {
            bail!("cursor for {name} is at {current}; refusing to move it back to {seq}");
        }
        if seq == current {
            return Ok(());
        }
    }
    {
        let log = shared.log.lock().unwrap();
        if seq > log.last_seq() {
            bail!("cannot acknowledge seq {seq}; the log ends at {}", log.last_seq());
        }
    }
    crate::server::fold::commit(
        shared,
        "-",
        0,
        Actor::Server,
        "cursor.acked",
        serde_json::json!({ "agent": name, "acked_seq": seq }),
    )?;
    Ok(())
}
```

**A frame's `seq` is the seq of the last *deliverable* event in it**, and acknowledging that is correct: everything at or below it was either delivered or is not deliverable at all, so it can never be owed to anyone.

- [ ] **Step 4: Run the tests, verify the gate, commit**

Run: `cargo test --test server_delivery`
Expected: PASS, 11 tests.

```bash
cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --all
git add src/server/delivery.rs src/server/mod.rs tests/server_delivery.rs
git commit -m "feat(server): cursor-driven delivery that filters control and agent events"
```

---

### Task 5: Live passive mode

**Why this task exists:** spec 6.4 defines `--passive live` as a **rate limit, not a count**: at most one passive frame per 30 seconds, sent when the quiet gap or the 5-minute age cap is reached, with repeated edits to the same ref coalesced. The earlier draft implemented one third of that and tested none of it.

**What the earlier draft got wrong:** `ServeArgs::passive` was parsed and never consumed. `core.passive_live` was set only by a test helper. `PASSIVE_MAX_AGE` was declared and never used. The quiet gap was not implemented — the first event flushed immediately. Coalescing was absent. And the test named `live_mode_flushes_a_long_running_storm_at_the_age_cap` asserted only `flushes <= 3`, which an implementation that never flushes at all also satisfies.

**Files:**
- Modify: `src/server/delivery.rs`, `src/server/http.rs`, `src/commands/serve.rs`, `tests/server_delivery.rs`

**Interfaces:**
- Produces:
  - `pub const PASSIVE_MIN_GAP: Duration = Duration::from_secs(30);`
  - `pub const PASSIVE_QUIET_GAP: Duration = Duration::from_secs(2);`
  - `pub const PASSIVE_MAX_AGE: Duration = Duration::from_secs(300);`
  - `pub struct PassiveState { pub oldest_pending: Option<Instant>, pub newest_pending: Option<Instant>, pub last_flush: Option<Instant> }`
  - `pub fn passive_due(state: &PassiveState, now: Instant) -> bool`
  - `pub fn coalesce(events: Vec<Event>) -> Vec<Event>`
- `Core` gains `pub passive_live: bool` and `pub passive: PassiveState`, both set from `ServeArgs::passive`.

**The rule, stated once so the tests can be exact.** A passive flush happens when **all** of these hold:

1. Live mode is on.
2. There is at least one pending passive event.
3. At least `PASSIVE_MIN_GAP` since the last flush. This is the rate limit.
4. **Either** nothing new has arrived for `PASSIVE_QUIET_GAP` (the reviewer paused) **or** the oldest pending event is older than `PASSIVE_MAX_AGE` (a storm that never pauses must still surface).

`passive_due` is a pure function of `PassiveState` and `now`, so the tests drive it with a fixed clock and assert exact times rather than an inequality that anything satisfies.

- [ ] **Step 1: Write the failing tests**

```rust
use artefacto::server::delivery::{coalesce, passive_due, PassiveState, PASSIVE_MAX_AGE, PASSIVE_MIN_GAP};

fn at(secs: u64, base: std::time::Instant) -> std::time::Instant {
    base.checked_add(std::time::Duration::from_secs(secs)).unwrap()
}

#[test]
fn nothing_pending_never_flushes() {
    let base = std::time::Instant::now();
    let state = PassiveState { oldest_pending: None, newest_pending: None, last_flush: None };
    assert!(!passive_due(&state, at(600, base)));
}

#[test]
fn a_quiet_gap_flushes_but_a_still_active_reviewer_does_not() {
    let base = std::time::Instant::now();
    let state = PassiveState {
        oldest_pending: Some(base),
        newest_pending: Some(at(60, base)),
        last_flush: Some(base),
    };
    assert!(
        !passive_due(&state, at(61, base)),
        "one second after the last event is not a pause"
    );
    assert!(
        passive_due(&state, at(63, base)),
        "three seconds of quiet is, and the rate limit was satisfied long ago"
    );
}

#[test]
fn the_rate_limit_holds_even_when_the_reviewer_has_paused() {
    let base = std::time::Instant::now();
    let state = PassiveState {
        oldest_pending: Some(at(10, base)),
        newest_pending: Some(at(10, base)),
        last_flush: Some(at(9, base)),
    };
    assert!(!passive_due(&state, at(20, base)), "11s since the last flush is inside the 30s limit");
    assert!(passive_due(&state, at(40, base)), "31s is not");
}

#[test]
fn a_storm_that_never_pauses_still_flushes_at_the_age_cap() {
    // This is the case the earlier draft's test claimed to cover and did not.
    let base = std::time::Instant::now();
    let now = at(PASSIVE_MAX_AGE.as_secs() + 1, base);
    let state = PassiveState {
        oldest_pending: Some(base),
        // Still arriving: the quiet gap is never reached.
        newest_pending: Some(now),
        last_flush: Some(at(1, base)),
    };
    assert!(
        passive_due(&state, now),
        "five minutes of continuous edits must surface, not wait for a pause that never comes"
    );
}

/// Drives the rule the way the server does: a **timer** tick, independent of
/// when events arrive. Evaluating it only on arrival is a real bug — `newest`
/// is then always `now`, so the quiet gap can never be observed and a steady
/// reviewer is never flushed at all.
fn simulate(duration_secs: f64, every_secs: f64) -> Vec<u64> {
    let (mut oldest, mut newest, mut last_flush) = (None::<f64>, None::<f64>, None::<f64>);
    let mut flushes = Vec::new();
    let (mut t, mut next_event, tick) = (0.0f64, 0.0f64, 0.25f64);
    while t < duration_secs {
        if t >= next_event {
            if oldest.is_none() {
                oldest = Some(t);
            }
            newest = Some(t);
            next_event += every_secs;
        }
        let due = oldest.is_some_and(|o| {
            let rate_ok = last_flush.is_none_or(|l| t - l >= PASSIVE_MIN_GAP.as_secs_f64());
            let quiet = newest.is_some_and(|n| t - n >= PASSIVE_QUIET_GAP.as_secs_f64());
            rate_ok && (quiet || t - o >= PASSIVE_MAX_AGE.as_secs_f64())
        });
        if due {
            flushes.push(t as u64);
            oldest = None;
            newest = None;
            last_flush = Some(t);
        }
        t += tick;
    }
    flushes
}

#[test]
fn a_continuous_storm_produces_no_frame_until_the_age_cap() {
    // Spec 6.4 sends a frame when the quiet gap **or** the age cap is reached.
    // A storm reaches neither for five minutes, so it is silent — the "two
    // frames a minute" figure is an upper bound, not a promise.
    assert_eq!(
        simulate(60.0, 0.1).len(),
        0,
        "a minute of unbroken editing never pauses, so nothing is due yet"
    );
    let long = simulate(600.0, 0.1);
    assert_eq!(long.len(), 1, "ten minutes of it yields exactly one frame");
    assert_eq!(long[0], 300, "at the age cap, not before");
}

#[test]
fn a_reviewer_working_steadily_gets_at_most_two_frames_a_minute() {
    // A comment every ten seconds: each gap is longer than the quiet gap, so
    // the rate limit is what bounds the frames.
    let f = simulate(60.0, 10.0);
    assert_eq!(f.len(), 2, "one frame per 30 seconds is the bound spec 6.4 states");
    assert_eq!(f, vec![2, 32], "the first at the first pause, the second when the limit lifts");
}

#[test]
fn a_long_steady_session_holds_the_same_rate() {
    let f = simulate(300.0, 10.0);
    assert_eq!(f.len(), 10, "five minutes at one per thirty seconds");
    for pair in f.windows(2) {
        assert!(pair[1] - pair[0] >= 30, "no two frames closer than the rate limit");
    }
}

#[test]
fn repeated_edits_to_one_ref_coalesce_to_the_latest() {
    let evs = vec![
        support::ev_ref(1, "thread.edited", "c-1", "first"),
        support::ev_ref(2, "thread.edited", "c-1", "second"),
        support::ev_ref(3, "thread.edited", "c-2", "other"),
        support::ev_ref(4, "thread.edited", "c-1", "third"),
    ];
    let out = coalesce(evs);
    assert_eq!(out.len(), 2, "one per ref");
    assert_eq!(out[0].data["text"], "third", "the latest edit wins");
    assert_eq!(out[0].seq, 4, "and carries the latest sequence number");
    assert_eq!(out[1].data["text"], "other");
}

#[test]
fn coalescing_never_merges_different_event_types() {
    let evs = vec![
        support::ev_ref(1, "thread.edited", "c-1", "edit"),
        support::ev_ref(2, "thread.replied", "c-1", "reply"),
    ];
    assert_eq!(coalesce(evs).len(), 2, "an edit and a reply are different things to answer");
}

#[test]
fn digest_mode_never_flushes_passively() {
    let h = Harness::start(); // digest is the default
    h.seed_artifact();
    h.log_passive("thread.opened");
    assert!(!h.passive_flush_due(), "digest is the default because a passive frame buys nothing");
}
```

- [ ] **Step 2: Run the tests to verify they fail, then implement**

```rust
/// Spec 6.4. A rate limit, not a count: an earlier draft flushed at 100
/// buffered events, which bounds nothing — a thousand edits a minute would
/// produce ten frames a minute rather than the two this rule allows.
pub fn passive_due(state: &PassiveState, now: Instant) -> bool {
    let Some(oldest) = state.oldest_pending else {
        return false;
    };
    if let Some(last) = state.last_flush {
        if now.saturating_duration_since(last) < PASSIVE_MIN_GAP {
            return false;
        }
    }
    let quiet = state
        .newest_pending
        .is_some_and(|n| now.saturating_duration_since(n) >= PASSIVE_QUIET_GAP);
    let aged = now.saturating_duration_since(oldest) >= PASSIVE_MAX_AGE;
    quiet || aged
}
```

`coalesce` keeps the **last** event per `(type, ref)` pair, preserving first-appearance order so the agent reads them in the order the refs were first touched. Events with no ref never coalesce.

`ServeArgs::passive` sets `core.passive_live`. The accept loop's idle branch calls a `flush_passive_if_due` that assembles `coalesce(passive_since(cursor))`, delivers it, and resets `PassiveState`.

- [ ] **Step 3: Run the tests, verify the gate, commit**

Run: `cargo test --test server_delivery`
Expected: PASS, 19 tests.

```bash
cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --all
git add src/server/delivery.rs src/server/http.rs src/commands/serve.rs tests/server_delivery.rs
git commit -m "feat(server): live passive mode with a quiet gap, an age cap, and coalescing"
```

---

### Task 6: `events` and `await`

**Why this task exists:** these are the only two ways an agent hears anything, and their failure modes are invisible until an agent is actually driving a review.

**What the earlier draft got wrong:**

- **`events --follow` had no implementation step and no test.** It is the transport for monitor-capable agents (spec 6.5), the first thing the skill arms (spec 7), and the command line `status --json` must print (spec 5). It appeared in an interface list and nowhere else. `Mode::Live` was never set outside a test.
- **`await` and `events` had no `--session`**, so the holder could not re-poll — the other half of Task 3's finding.
- **The route consumed the `Request` and returned a `Response`.** Dropping an unresponded `tiny_http::Request` makes the library answer 500, and the unused parameter fails the `-D warnings` gate. Every handler must call `request.respond(...)` itself.
- **`await --artifact` was parsed and never used.**
- **The whole-plan verification ran `await` before any server existed**, which must exit 4, so the command substitution it fed was empty.

**Files:**
- Create: `src/commands/agent.rs`, `tests/server_agent.rs`
- Modify: `src/cli.rs`, `src/commands/mod.rs`, `src/server/http.rs`, `src/client.rs`

**Interfaces:**
- Produces:
  - `pub fn await_cmd(args: &AwaitArgs) -> anyhow::Result<()>`
  - `pub fn events(args: &EventsArgs) -> anyhow::Result<()>`
  - `pub fn ack_cmd(args: &AckArgs) -> anyhow::Result<()>`
  - Routes `GET /cli/await`, `GET /cli/events`, `POST /cli/ack`
  - `pub const DEFAULT_TIMEOUT_SECS: u64 = 90;`
- Result shape: `{"ok":true,"status":"…","seq":N,"session":"…","events":[…]}`

**The seven `await` statuses**, all exiting 0: `submitted`, `chat`, `idle`, `away`, `back`, `timeout`, `stopped`. `back` is here because spec 6.2 classifies `reviewer.back` as active; see the spec-change note at the top of this plan.

- [ ] **Step 1: Write the failing tests**

Create `tests/server_agent.rs`. The ones that carry the findings:

```rust
mod support;
use support::Harness;

#[test]
fn the_first_await_returns_a_session_and_the_second_reuses_it() {
    // The poll loop. The earlier draft exited 6 here.
    let h = Harness::start();
    h.seed_artifact();
    let first = h.run_json(&["await", "--timeout", "1s", "--agent", "claude"]);
    let session = first["session"].as_str().expect("await hands back a session").to_string();

    let second = h.run_json(&["await", "--timeout", "1s", "--agent", "claude", "--session", &session]);
    assert_eq!(second["ok"], true);
    assert_eq!(second["session"], session, "the same lease, not a new one");
}

#[test]
fn a_token_from_await_still_works_for_reply_after_await_exits() {
    // `await` is a short-lived subprocess. If liveness were a pid check, its
    // own token would be dead before this line.
    let h = Harness::start();
    h.seed_artifact();
    let session = h.run_json(&["await", "--timeout", "1s"])["session"].as_str().unwrap().to_string();
    let out = h.run_cli(&["reply", "--session", &session, "still holding the lease"]);
    assert_eq!(out.code, 0, "{}", out.stderr);
}

#[test]
fn await_returns_on_a_chat_event_with_the_passive_events_before_it() {
    let h = Harness::start();
    h.seed_artifact();
    let session = h.take_lease("claude");
    h.log_passive("thread.opened");
    h.log_active("chat.sent");

    let r = h.await_now(&session, 5);
    assert_eq!(r["status"], "chat");
    assert_eq!(r["ok"], true);
    assert_eq!(r["events"].as_array().unwrap().len(), 2);
}

#[test]
fn await_returns_timeout_with_exit_zero_when_nothing_happens() {
    let h = Harness::start();
    h.seed_artifact();
    let session = h.take_lease("claude");
    let out = h.run_cli(&["await", "--timeout", "1s", "--session", &session]);
    assert_eq!(out.code, 0, "agents treat a non-zero exit as a failed tool call, not 'poll again'");
    let v: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(v["status"], "timeout");
}

#[test]
fn await_returns_at_the_earliest_active_event() {
    let h = Harness::start();
    h.seed_artifact();
    let session = h.take_lease("claude");
    h.log_active("chat.sent");
    h.log_active("review.submitted");
    let r = h.await_now(&session, 5);
    assert_eq!(r["status"], "chat", "events are handled in the order they happened");
}

#[test]
fn await_returns_back_when_the_reviewer_reconnects() {
    let h = Harness::start();
    h.seed_artifact();
    let session = h.take_lease("claude");
    h.log_active("reviewer.back");
    assert_eq!(
        h.await_now(&session, 5)["status"], "back",
        "spec 6.2 lists reviewer.back as active, so it needs a status of its own"
    );
}

#[test]
fn await_filters_to_one_artifact_when_asked() {
    let h = Harness::start();
    h.seed_two_artifacts();
    let session = h.take_lease("claude");
    h.log_active_on("plan:other", "chat.sent");
    let out = h.run_cli(&["await", "--timeout", "1s", "--session", &session, "--artifact", "plan:demo"]);
    let v: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(v["status"], "timeout", "an event for another artifact must not wake this wait");
}

#[test]
fn calling_await_again_acknowledges_the_previous_frame() {
    let h = Harness::start();
    h.seed_artifact();
    let session = h.take_lease("claude");
    h.log_active("chat.sent");
    let first = h.await_now(&session, 5);
    h.log_active("chat.sent");
    let second = h.await_now(&session, 5);
    assert_eq!(second["events"].as_array().unwrap().len(), 1, "the first frame was acknowledged");
    assert!(second["seq"].as_u64().unwrap() > first["seq"].as_u64().unwrap());
}

#[test]
fn await_without_a_server_exits_4() {
    let repo = support::git_repo();
    support::run(&repo, &["await", "--timeout", "1s"]).code(4);
}

#[test]
fn a_mutation_with_a_superseded_token_exits_6() {
    let h = Harness::start();
    h.seed_artifact();
    let stale = h.take_lease("claude");
    let _fresh = h.take_lease_with_takeover("codex");
    assert_eq!(h.run_cli(&["ack", "--seq", "1", "--session", &stale]).code, 6);
}

#[test]
fn a_second_agent_without_takeover_exits_6_and_names_the_holder() {
    let h = Harness::start();
    h.seed_artifact();
    let _first = h.take_lease("claude");
    let out = h.run_cli(&["await", "--agent", "codex", "--timeout", "1s"]);
    assert_eq!(out.code, 6);
    assert!(out.stderr.contains("claude"), "the refusal names the holder and its age");
}

#[test]
fn events_since_prints_the_backlog_as_ndjson_and_exits() {
    let h = Harness::start();
    h.seed_artifact();
    h.log_active("chat.sent");
    h.log_active("chat.sent");
    let out = h.run_cli(&["events", "--since", "0"]);
    assert_eq!(out.code, 0);
    let lines: Vec<&str> = out.stdout.lines().filter(|l| !l.is_empty()).collect();
    assert!(!lines.is_empty());
    for line in lines {
        let v: serde_json::Value = serde_json::from_str(line).expect("one JSON frame per line");
        assert_eq!(v["format"], "artefacto.frame/1");
    }
}

#[test]
fn events_follow_streams_frames_and_holds_a_live_lease() {
    let h = Harness::start();
    h.seed_artifact();
    let mut follow = h.spawn_follow("claude");

    h.wait_for(|| h.lease_mode() == Some("live".to_string()), "follow should take a live lease");
    h.log_active("chat.sent");
    let frame = follow.next_frame();
    assert_eq!(frame["events"].as_array().unwrap().last().unwrap()["type"], "chat.sent");

    follow.kill();
    h.wait_for(|| h.lease_mode().is_none(), "a --follow disconnect releases the lease at once");
}

#[test]
fn events_follow_exits_when_the_server_stops() {
    let h = Harness::start();
    h.seed_artifact();
    let mut follow = h.spawn_follow("claude");
    h.wait_for(|| h.lease_mode().is_some(), "follow attached");
    h.request_stop();
    assert_eq!(follow.wait_code(), 0, "a shutdown is not a failed tool call");
}

#[test]
fn await_survives_a_server_restart_mid_wait() {
    let h = Harness::start();
    h.seed_artifact();
    let session = h.take_lease("claude");
    let waiting = h.await_in_background(&session, 20);
    h.restart_server_in_place();
    h.log_active("chat.sent");
    assert_eq!(
        waiting.join()["status"], "chat",
        "await reconnects against the same cursor rather than surfacing the restart"
    );
}
```

- [ ] **Step 2: Add the CLI types**

```rust
#[derive(Args, Debug)]
pub struct AwaitArgs {
    #[arg(long, default_value = "90s")]
    pub timeout: String,
    #[arg(long)]
    pub since: Option<u64>,
    #[arg(long)]
    pub artifact: Option<String>,
    #[arg(long, default_value = "agent")]
    pub agent: String,
    /// The session token from a previous call. Omitting it takes a fresh
    /// lease; presenting it refreshes the one you hold.
    #[arg(long)]
    pub session: Option<String>,
    #[arg(long)]
    pub takeover: bool,
}
```

`EventsArgs` is the same plus `--follow` and minus `--timeout`.

- [ ] **Step 3: Implement the long poll**

The route holds the request open. Blocking is the point: the thread is the wait.

```rust
/// `GET /cli/await`. **Responds inside this function.** Returning a
/// `Response` and dropping the `Request` makes tiny_http answer 500.
fn await_route(shared: Arc<Shared>, request: Request, q: AwaitQuery) {
    let deadline = Instant::now()
        .checked_add(q.timeout)
        .unwrap_or_else(|| Instant::now() + Duration::from_secs(DEFAULT_TIMEOUT_SECS));
    let cursor = q.since.unwrap_or_else(|| delivery::cursor_for(&shared, &q.agent));

    let body = loop {
        if let Some(frame) = delivery::frame_for(&shared, cursor, q.artifact.as_deref()) {
            break result_json(status_for(&frame), &frame, &q.session);
        }
        if shared.stopping() {
            break stopped_json(cursor, &q.session);
        }
        if Instant::now() >= deadline {
            // A timeout still carries whatever passive events accumulated, so
            // the agent's terminal is not silent about a busy review.
            break timeout_json(cursor, delivery::passive_since(&shared, cursor), &q.session);
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let _ = request.respond(json_response(200, &body));
}

/// The status is named by the event that ended the frame, always the last one.
fn status_for(frame: &Frame) -> &'static str {
    match frame.events.last().map(|e| e.r#type.as_str()) {
        Some("review.submitted") => "submitted",
        Some("chat.sent") => "chat",
        Some("reviewer.idle") => "idle",
        Some("reviewer.away") => "away",
        Some("reviewer.back") => "back",
        Some("server.stopping") => "stopped",
        _ => "timeout",
    }
}
```

Polling at 50 ms rather than a condition variable is deliberate: the wait is bounded by a deadline anyway, one reviewer generates events at human speed, and `frame_for` reads an in-memory slice rather than the disk. Revisit only if a profile says otherwise.

**Client side, the two behaviours the spec names.** `await` **reconnects on its own**: a connection error before the absolute deadline is retried against the same cursor after 250 ms, never surfaced. Only the deadline ends the wait, as `timeout`. And **calling `await` or `events` again acknowledges the previous frame**: the client sends the previous `seq` as its cursor and the server persists it.

- [ ] **Step 4: Implement `events --follow`**

Holds the lease as `Mode::Live` with the process pid recorded, writes one NDJSON frame per line, **flushes after every line** so a monitoring agent sees frames as they happen, and releases the lease when the connection drops or the process exits. It exits 0 on `server.stopping`.

- [ ] **Step 5: Run the tests, verify the gate, commit**

Run: `cargo test --test server_agent`
Expected: PASS, 15 tests.

```bash
cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --all
git add src/commands/agent.rs src/cli.rs src/commands/mod.rs src/server/http.rs src/client.rs tests/server_agent.rs
git commit -m "feat(agent): await and events with session reuse and a live follow stream"
```

---

### Task 7: `plan push`

**Why this task exists:** push is how a plan becomes a reviewable artifact, and it carries the only concurrency check in the system. Spec 5 is emphatic that `--base-revision` comes from the caller, because reading the current revision at push time would compare the server to itself and always pass.

**What the earlier draft got wrong:**

- It claimed resolutions were applied "in the same append batch", but `EventLog` had only a single-event `append`, so a crash could commit a revision and only some of its resolutions. This task adds `append_all`.
- The artifact id was hard-coded in tests with no rule for deriving it.
- The JSON result omitted `title` and the phase and task counts that spec 5 requires of every JSON result.
- The exit-2 path for a missing `--base-revision` was server-detected but never mapped to an exit code.

**Files:**
- Modify: `src/cli.rs`, `src/commands/plan.rs`, `src/server/log.rs` (adds `append_all`), `src/server/http.rs`
- Create: `tests/server_push.rs`

**Interfaces:**
- Produces:
  - `pub fn push(args: &PushArgs) -> anyhow::Result<()>`
  - `pub fn EventLog::append_all(&mut self, entries: Vec<PendingEvent>) -> anyhow::Result<Vec<Event>>`
  - Route `POST /cli/push`
  - `pub fn artifact_id(plan: &Plan) -> String` — `plan:<meta.id>`

**`append_all` is the transaction.** It serializes every entry, writes them in one `write_all`, then a single `fsync`. A crash leaves either all of them or a torn tail that Task 3's recovery truncates — never half a revision. It is not a general database transaction and does not need to be; it needs to make "a revision and its resolutions" atomic, and that is one contiguous write.

**The artifact id is `plan:<meta.id>`**, taken from the validated plan. `meta.id` is already constrained by the model's id rule, so it cannot contain a character that would break a URL or a header.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn the_first_push_needs_neither_flag() {
    let h = Harness::start();
    let session = h.take_lease("claude");
    let v = h.push_json(&["--session", &session]);
    assert_eq!(v["revision"], 1);
    assert_eq!(v["artifact"], "plan:auth-refactor", "plan:<meta.id>");
    assert!(v["url"].as_str().unwrap().contains("/b/"), "push returns a bootstrap URL");
    // Spec 5: every JSON result carries these.
    assert!(v["plan_hash"].as_str().unwrap().starts_with("sha256:"));
    assert!(v["title"].is_string());
    assert!(v["phases"].is_number());
    assert!(v["tasks"].is_number());
}

#[test]
fn a_later_push_must_pass_base_revision_or_force() {
    let h = Harness::start();
    let session = h.take_lease("claude");
    h.push(&["--session", &session]);
    let out = h.push(&["--session", &session]);
    assert_eq!(out.code, 2, "a usage error, even though the server is what detected it");
    assert!(out.stderr.contains("--base-revision"));
}

#[test]
fn a_stale_base_revision_is_refused_with_exit_7() {
    let h = Harness::start();
    let session = h.take_lease("claude");
    h.push(&["--session", &session]);
    h.push(&["--session", &session, "--base-revision", "1"]);
    let out = h.push(&["--session", &session, "--base-revision", "1"]);
    assert_eq!(out.code, 7);
    assert!(out.stderr.contains("status --json"), "the message says how to catch up");
}

#[test]
fn force_overrides_a_stale_base_revision() {
    let h = Harness::start();
    let session = h.take_lease("claude");
    h.push(&["--session", &session]);
    h.push(&["--session", &session, "--base-revision", "1"]);
    assert_eq!(h.push(&["--session", &session, "--force"]).code, 0);
}

#[test]
fn an_invalid_plan_is_refused_and_nothing_is_logged() {
    let h = Harness::start();
    let session = h.take_lease("claude");
    let before = h.last_seq();
    assert_eq!(h.push_file("invalid-cycle.json", &["--session", &session]).code, 1);
    assert_eq!(h.last_seq(), before, "validation happens before anything reaches the log");
}

#[test]
fn a_revision_event_carries_the_whole_plan_and_a_change_summary() {
    let h = Harness::start();
    let session = h.take_lease("claude");
    h.push(&["--session", &session]);
    let e = h.last_event_of_type("revision.published");
    assert!(e["data"]["plan"]["phases"].is_array(), "not just a summary; the log must rebuild the body");
    assert!(e["data"]["plan_hash"].as_str().unwrap().starts_with("sha256:"));
    assert!(e["data"]["summary"].is_string(), "spec 6.3 requires a change summary");
}

#[test]
fn a_revision_and_its_resolutions_commit_together_or_not_at_all() {
    let h = Harness::start();
    let session = h.take_lease("claude");
    h.push(&["--session", &session]);
    let t1 = h.open_thread("task:t-a", "one");
    let t2 = h.open_thread("task:t-b", "two");
    let file = h.write_resolutions(&[(&t1, "changed", "fixed"), (&t2, "declined", "out of scope")]);

    let before = h.last_seq();
    let out = h.push(&["--session", &session, "--base-revision", "1", "--resolutions", file.to_str().unwrap()]);
    assert_eq!(out.code, 0);
    assert_eq!(h.thread_status(&t1), "changed");
    assert_eq!(h.thread_status(&t2), "declined");
    assert_eq!(
        h.last_seq() - before,
        3,
        "one revision plus two resolutions, appended as one contiguous write"
    );
}

#[test]
fn the_page_body_survives_a_restart_from_the_log_alone() {
    let h = Harness::start();
    let session = h.take_lease("claude");
    h.push(&["--session", &session]);
    let before = h.rendered_body("plan:auth-refactor");
    let h = h.restart();
    assert_eq!(h.rendered_body("plan:auth-refactor"), before);
}

#[test]
fn a_push_with_a_superseded_session_exits_6() {
    let h = Harness::start();
    let stale = h.take_lease("claude");
    let _fresh = h.take_lease_with_takeover("codex");
    assert_eq!(h.push(&["--session", &stale]).code, 6);
}

#[test]
fn push_starts_a_server_when_none_is_running() {
    let repo = support::git_repo();
    let out = support::push_in(&repo, &[]);
    assert_eq!(out.code, 0, "push is the first command an agent runs");
    support::stop(&repo);
}
```

- [ ] **Step 2: Implement, in this order**

Each step exists to prevent one failure:

1. **Validate locally.** An invalid plan never reaches the server, so a bad push cannot append.
2. **Start the server if none is running**, because push is the first command an agent runs.
3. **Compare and append under one `log` guard.** Comparing and then appending in two steps would let two agents both pass the check.
4. **`append_all`** the revision plus every resolution, so a crash cannot split them.
5. **Open the browser on the first push only.**

```rust
/// `base_revision` comes from the caller, never from the server. Reading the
/// current revision here would compare the server to itself and always pass,
/// which is the bug this check exists to prevent.
fn check_base_revision(current: u32, base: Option<u32>, force: bool) -> Result<(), PushError> {
    if force || current == 0 {
        return Ok(());
    }
    match base {
        None => Err(PushError::MissingBaseRevision),
        Some(b) if b == current => Ok(()),
        Some(b) => Err(PushError::Stale { seen: b, current }),
    }
}
```

`PushError::MissingBaseRevision` maps to **exit 2** and prints `a later push needs --base-revision <N> or --force` on stderr. `PushError::Stale` maps to **exit 7** and prints `the server is at revision {current}, you pushed against {seen}; re-read with 'artefacto status --json' or pass --force`. The mapping lives in `main.rs` beside the existing `ReportedFailure` and `UsageReported` arms.

- [ ] **Step 3: Run the tests, verify the gate, commit**

Run: `cargo test --test server_push`
Expected: PASS, 10 tests.

```bash
cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --all
git add src/cli.rs src/commands/plan.rs src/server/log.rs src/server/http.rs tests/server_push.rs
git commit -m "feat(plan): push a revision and its resolutions as one atomic append"
```

---

### Task 8: `reply`, `resolve`, presence, and the feedback document

**Why this task exists:** it closes the loop and covers the last spec requirements with no home: `agent.attached`/`detached` and `nudge` from 6.3, the `reviewer.idle`/`away`/`back` timers from 6.2, and the repo-local feedback file from 6.7.

**What the earlier draft got wrong:**

- **The submit handler was ordered impossibly**: append the event, then obtain the path, then put that path into the already-appended event. The path must be computed and the file written **before** the event is appended, so the event can carry it.
- **`std::fs::write` is not atomic**, so a crash mid-write leaves a truncated feedback document where a reader expects a complete one.
- **One `last_activity` field served two timers.** Every HTTP request refreshed it, and an `await` long poll is an HTTP request every 90 seconds — so with an agent attached, `reviewer.idle` could never fire. Plan 2a already split it: `last_request_at` is for self-exit, and this task adds `last_reviewer_activity_at` for the nudges.
- **The nudge test used a `reply --nudge` flag no task defined.**
- **Adding `agent.attached` to `acquire` shifted every hard-coded `seq`** in the earlier `tests/server_agent.rs`. Presence lands in Task 3 here, before the delivery and agent tests are written, and those tests assert event **types and relative order**, never absolute sequence numbers.

**Files:**
- Create: `src/server/presence.rs`, `src/server/feedback.rs`, `tests/server_loop.rs`
- Modify: `src/commands/agent.rs`, `src/cli.rs`, `src/server/http.rs`, `src/server/socket.rs`

**Interfaces:**
- Produces:
  - `pub fn reply(args: &ReplyArgs) -> anyhow::Result<()>`, `resolve(args: &ResolveArgs)`
  - `pub fn on_reviewer_activity(shared: &Shared, now: Instant)` — throttled to one mark per 30 s
  - `pub fn tick(shared: &Shared, now: Instant)` — from the accept loop's idle branch
  - `pub fn write_feedback(source: &Path, doc: &serde_json::Value, override_path: Option<&Path>) -> anyhow::Result<PathBuf>`
  - `pub fn feedback_path(source: &Path) -> PathBuf` — `<stem>-feedback.json` beside the plan

```rust
#[derive(Args, Debug)]
#[command(group = clap::ArgGroup::new("verdict").required(true))]
pub struct ResolveArgs {
    pub thread: String,
    #[arg(long)]
    pub session: String,
    #[arg(long, group = "verdict")]
    pub changed: bool,
    #[arg(long, group = "verdict")]
    pub declined: bool,
    #[arg(long)]
    pub note: Option<String>,
}

#[derive(Args, Debug)]
pub struct ReplyArgs {
    #[arg(long)]
    pub session: String,
    #[arg(long, conflicts_with = "artifact")]
    pub thread: Option<String>,
    #[arg(long)]
    pub artifact: Option<String>,
    /// Post a banner rather than a thread message. Spec 6.3 requires a `nudge`
    /// event; spec section 5 needs this flag added to its `reply` surface.
    #[arg(long)]
    pub nudge: bool,
    #[arg(required_unless_present = "stdin")]
    pub text: Option<String>,
    #[arg(long, conflicts_with = "text")]
    pub stdin: bool,
}
```

- [ ] **Step 1: Write the failing tests**

Create `tests/server_loop.rs`. The whole-loop test is the one that matters:

```rust
#[test]
fn the_whole_loop_runs_once_through() {
    let h = Harness::start();

    // 1. the agent takes a lease and publishes
    let session = h.take_lease("claude");
    let pushed = h.push_json(&["--session", &session]);
    assert_eq!(pushed["revision"], 1);

    // 2. the reviewer opens the page and asks a question
    let mut page = h.connect_page();
    let thread = page.open_thread("cid-1", "task:t-session-store");
    page.request(serde_json::json!({
        "cmd": "chat.send", "client_id": "cid-2", "thread": thread, "text": "why a trait here?"
    }));

    // 3. the agent hears it
    let heard = h.await_now(&session, 5);
    assert_eq!(heard["status"], "chat");
    let last = heard["events"].as_array().unwrap().last().unwrap();
    assert_eq!(last["data"]["text"], "why a trait here?");

    // 4. the agent answers, and the reviewer sees it
    h.run_cli(&["reply", "--session", &session, "--thread", &thread, "so Redis can slot in"]);
    let frame = page.next_frame();
    assert_eq!(frame["events"][0]["type"], "thread.replied");
    assert_eq!(frame["events"][0]["data"]["text"], "so Redis can slot in");

    // 5. the reviewer submits, and the agent is told where the file landed
    page.request(serde_json::json!({
        "cmd": "review.submit", "client_id": "cid-3", "verdict": "approve", "base_revision": 1
    }));
    let submitted = h.await_now(&session, 5);
    assert_eq!(submitted["status"], "submitted");
    let path = submitted["events"].as_array().unwrap().last().unwrap()["data"]["path"]
        .as_str().unwrap().to_string();

    assert!(std::path::Path::new(&path).exists(), "the file-based loop keeps working");
    let doc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(doc["format"], "artefacto.feedback/1");
    assert_eq!(doc["verdict"], "approve");
    assert_eq!(doc["base_revision"], 1);
    assert_eq!(doc["comments"][0]["id"], "c-1", "ids are server-assigned and stable");
    assert!(doc["answers"].is_array());
    assert!(doc["reviewed"].is_array());
}

#[test]
fn the_feedback_file_is_written_before_the_event_that_names_it() {
    // The earlier draft appended the event, then computed the path, then put
    // the path into the already-appended event.
    let h = Harness::start();
    let session = h.take_lease("claude");
    h.push(&["--session", &session]);
    let mut page = h.connect_page();
    page.submit("cid-1", "request_changes", 1);

    let e = h.last_event_of_type("review.submitted");
    let path = e["data"]["path"].as_str().expect("the event carries the path");
    assert!(std::path::Path::new(path).exists(), "and the file was already there when it was written");
}

#[test]
fn a_feedback_write_is_atomic() {
    let h = Harness::start();
    let session = h.take_lease("claude");
    h.push(&["--session", &session]);
    let mut page = h.connect_page();
    page.submit("cid-1", "approve", 1);
    let path = h.feedback_path();
    // A temp-file-and-rename leaves no partial file behind.
    let siblings: Vec<_> = std::fs::read_dir(path.parent().unwrap()).unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains("feedback"))
        .collect();
    assert_eq!(siblings.len(), 1, "no .tmp left over: {siblings:?}");
}

#[test]
fn a_resubmit_with_the_same_client_id_does_not_duplicate_the_review() {
    let h = Harness::start();
    let session = h.take_lease("claude");
    h.push(&["--session", &session]);
    let mut page = h.connect_page();
    page.submit("cid-1", "approve", 1);
    let after = h.last_seq();
    page.submit("cid-1", "approve", 1);
    assert_eq!(h.last_seq(), after, "a disconnect around submit must not duplicate it");
}

#[test]
fn taking_and_releasing_the_lease_announces_presence_to_the_page() {
    let h = Harness::start();
    h.seed_artifact();
    let mut page = h.connect_page();
    let session = h.take_lease("claude");
    let attached = page.next_frame();
    assert_eq!(attached["events"][0]["type"], "agent.attached");
    assert_eq!(attached["events"][0]["data"]["mode"], "waiting");

    h.release_lease(&session);
    assert_eq!(page.next_frame()["events"][0]["type"], "agent.detached");
}

#[test]
fn a_nudge_reaches_the_page_as_a_banner() {
    let h = Harness::start();
    let session = h.take_lease("claude");
    h.push(&["--session", &session]);
    let mut page = h.connect_page();
    h.run_cli(&["reply", "--session", &session, "--nudge", "have a look at phase 2"]);
    assert_eq!(page.next_frame()["events"][0]["type"], "nudge");
}

#[test]
fn reply_reads_stdin_when_asked() {
    let h = Harness::start();
    let session = h.take_lease("claude");
    h.push(&["--session", &session]);
    let out = h.run_cli_stdin(&["reply", "--session", &session, "--stdin"], "from a pipe");
    assert_eq!(out.code, 0);
    assert_eq!(h.last_event_of_type("thread.replied")["data"]["text"], "from a pipe");
}

#[test]
fn resolve_requires_exactly_one_verdict() {
    let h = Harness::start();
    let session = h.take_lease("claude");
    h.push(&["--session", &session]);
    let thread = h.open_thread("task:t-a", "x");
    assert_eq!(h.run_cli(&["resolve", &thread, "--session", &session]).code, 2);
}

#[test]
fn an_agent_polling_does_not_reset_the_reviewers_idle_timer() {
    // An `await` is an HTTP request every 90 seconds. If one field served both
    // timers, reviewer.idle could never fire with an agent attached.
    let h = Harness::start_with_idle(std::time::Duration::from_secs(900));
    h.seed_artifact();
    let _page = h.connect_page();
    let session = h.take_lease("claude");
    let t0 = std::time::Instant::now();
    h.mark_reviewer_activity(t0);

    for minute in 0..20 {
        h.run_cli(&["await", "--timeout", "1s", "--session", &session]);
        presence::tick(&h.shared, t0 + std::time::Duration::from_secs(minute * 60));
    }
    assert_eq!(h.count_events("reviewer.idle"), 1, "the agent's polling is not the reviewer's activity");
}

#[test]
fn idle_fires_once_per_quiet_period_and_rearms_after_activity() {
    let h = Harness::start_with_idle(std::time::Duration::from_secs(900));
    h.seed_artifact();
    let _page = h.connect_page();
    let t0 = std::time::Instant::now();
    h.mark_reviewer_activity(t0);

    presence::tick(&h.shared, t0 + std::time::Duration::from_secs(901));
    assert_eq!(h.count_events("reviewer.idle"), 1);
    presence::tick(&h.shared, t0 + std::time::Duration::from_secs(1200));
    assert_eq!(h.count_events("reviewer.idle"), 1, "once per quiet period, not once per tick");

    h.mark_reviewer_activity(t0 + std::time::Duration::from_secs(1300));
    presence::tick(&h.shared, t0 + std::time::Duration::from_secs(2300));
    assert_eq!(h.count_events("reviewer.idle"), 2, "activity re-arms it");
}

#[test]
fn a_reviewer_who_reads_for_twenty_minutes_is_not_idle() {
    let h = Harness::start_with_idle(std::time::Duration::from_secs(900));
    h.seed_artifact();
    let _page = h.connect_page();
    let t0 = std::time::Instant::now();
    for minute in 0..20 {
        h.mark_reviewer_activity(t0 + std::time::Duration::from_secs(minute * 60));
        presence::tick(&h.shared, t0 + std::time::Duration::from_secs(minute * 60 + 30));
    }
    assert_eq!(
        h.count_events("reviewer.idle"), 0,
        "idle is measured from activity, not from the last comment"
    );
}

#[test]
fn away_fires_once_and_back_fires_on_return() {
    let h = Harness::start_with_away(std::time::Duration::from_secs(300));
    h.seed_artifact();
    let page = h.connect_page();
    let t0 = std::time::Instant::now();
    drop(page);
    h.wait_for(|| h.page_count() == 0, "the page should be gone");

    presence::tick(&h.shared, t0 + std::time::Duration::from_secs(301));
    assert_eq!(h.count_events("reviewer.away"), 1);
    presence::tick(&h.shared, t0 + std::time::Duration::from_secs(600));
    assert_eq!(h.count_events("reviewer.away"), 1, "once, not every tick");

    let _returned = h.connect_page();
    h.wait_for(|| h.count_events("reviewer.back") == 1, "reconnecting fires back");
}

#[test]
fn away_does_not_fire_once_the_review_is_submitted() {
    let h = Harness::start_with_away(std::time::Duration::from_secs(300));
    let session = h.take_lease("claude");
    h.push(&["--session", &session]);
    let mut page = h.connect_page();
    page.submit("cid-1", "approve", 1);
    let t0 = std::time::Instant::now();
    drop(page);
    h.wait_for(|| h.page_count() == 0, "the page should be gone");
    presence::tick(&h.shared, t0 + std::time::Duration::from_secs(301));
    assert_eq!(h.count_events("reviewer.away"), 0, "a finished review is not an abandoned one");
}

#[test]
fn off_disables_a_timer_entirely() {
    let h = Harness::start_with_idle_off();
    h.seed_artifact();
    let _page = h.connect_page();
    let t0 = std::time::Instant::now();
    h.mark_reviewer_activity(t0);
    presence::tick(&h.shared, t0 + std::time::Duration::from_secs(100_000));
    assert_eq!(h.count_events("reviewer.idle"), 0, "spec 16 allows off");
}
```

- [ ] **Step 2: Implement the feedback document**

```rust
/// `<stem>-feedback.json` beside the pushed plan, or the per-push override.
///
/// Written with a temp file and a rename, because a reader that finds a
/// truncated document cannot tell it from a complete one. The path is
/// returned so the caller can put it **into** the `review.submitted` event —
/// the file exists before the event that names it, never after.
pub fn write_feedback(source: &Path, doc: &Value, override_path: Option<&Path>) -> Result<PathBuf> {
    let final_path = override_path.map(Path::to_path_buf).unwrap_or_else(|| feedback_path(source));
    let tmp = final_path.with_extension("json.tmp");
    let file = std::fs::File::create(&tmp)?;
    serde_json::to_writer_pretty(&file, doc)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, &final_path)?;
    Ok(final_path)
}
```

The `review.submit` handler builds the `artefacto.feedback/1` document from the folded `Review` — **server-assigned** comment ids, not whatever the page sent — calls `write_feedback`, then appends `review.submitted` carrying both the document and `data.path`.

- [ ] **Step 3: Implement presence**

`reviewer.idle` and `reviewer.away` each fire **once** per quiet period, guarded by `idle_fired` and `away_fired`, both cleared by the condition that set them. `tick` runs from the accept loop's idle branch, four times a second, so no extra thread is needed. `on_reviewer_activity` is throttled to one mark per 30 seconds and is called from **ingress**, on every page command including `ping` — not from the HTTP layer, which is the agent's traffic. A `None` idle or away duration disables that timer.

`lease::acquire` and `release` append `agent.attached` / `agent.detached` with `{agent, mode}` alongside the internal `lease.taken` / `lease.released`. Deriving presence from the lease is what keeps the page's pill from flickering between poll cycles.

- [ ] **Step 4: Run the tests, verify the gate, commit**

Run: `cargo test --test server_loop`
Expected: PASS, 14 tests.

```bash
cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --all
git add src/server/presence.rs src/server/feedback.rs src/commands/agent.rs src/cli.rs src/server/http.rs src/server/socket.rs tests/server_loop.rs
git commit -m "feat(agent): reply, resolve, presence, and the feedback document"
```

---

## What this plan deliberately leaves out

- **The page rewrite.** Plan 3. Every socket path here is driven by a fake page client.
- **The artifact index, `list`, posters.** Plan 4.
- **`skill --print`, `--install`, cargo-dist.** Plan 5.
- **The loadout dispatcher.** Plan 6. Nothing here edits the rosita repository.
- **`clean`.** Plan 4. It truncates the log and rotates the secret, and it is easier to write once the index exists to be preserved across it.
- **An agent-role WebSocket.** Spec 6.5 defers it: it would save one process and cost a second auth path with a token in the transcript.
- **The file-based fallback transport.** Spec 13's contingency; three spikes cleared the loopback path.
- **Question `options`.** Spec 4.3 keeps v1 answers as free text.

**What this leaves uncovered, stated plainly:** there is still no browser-level test. "The server delivers a frame" is verified; "the reviewer sees it" is not. The escaping test here asserts against the JSON the socket sends rather than a rendered body, which is a real check but not the same as a real browser refusing to execute an injected tag. That closes in plan 3 with the headless-Chromium smoke.

## Verification for the whole plan

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --all
```

Then drive the loop by hand, in this order. **`push` first**, because it is the command that starts a server; `await` with no server exits 4, so running it first gets you nothing:

```bash
cargo run --quiet -- plan push tests/fixtures/plan/kitchen-sink.json --json
# note the "url" it prints, and open it
SESSION="$(cargo run --quiet -- await --timeout 5s | python3 -c 'import json,sys; print(json.load(sys.stdin)["session"])')"
cargo run --quiet -- status --json
```

In the browser: comment on a task, then check the agent side sees it.

```bash
cargo run --quiet -- await --timeout 30s --session "$SESSION"
cargo run --quiet -- reply --session "$SESSION" --thread c-1 "looking at it"
```

The reply should appear in the page without a reload. Then submit the review from the page and confirm `<stem>-feedback.json` appears beside the plan file with your comment in it, carrying id `c-1`.

Report what you saw. Do not claim the loop works without watching a comment cross in both directions.

## Appendix A: the test harness

Extends plan 2a's `tests/support/mod.rs`. Each helper is added by the task that first uses it, and the rule from plan 2a still holds: a helper may observe state or stand in for the reviewer, but never skip a code path the production caller would take.

| helper | signature | contract | task |
|---|---|---|---|
| `ev` | `fn ev(seq: u64, actor: Actor, kind: &str, data: Value) -> Event` | Builds an event for fold tests without a server. | 1 |
| `ev_ref` | `fn ev_ref(seq: u64, kind: &str, r: &str, text: &str) -> Event` | The same with a `ref` and `text`, for coalescing. | 5 |
| `plan_data` | `fn plan_data() -> Value` | The `revision.published` payload for a minimal valid plan. | 1 |
| `seed_artifact` | `fn seed_artifact(&self)` | Pushes the default fixture through the real path so an artifact exists. | 2 |
| `seed_two_artifacts` | `fn seed_two_artifacts(&self)` | Two artifacts, for the `--artifact` filter test. | 6 |
| `review_snapshot` | `fn review_snapshot(&self) -> Value` | A normalized, comparable projection of the fold — not the struct, so no `Instant` or field order leaks in. | 1 |
| `thread_status` | `fn thread_status(&self, thread: &str) -> String` | `open`, `changed`, `declined`, `unanchored`. | 1 |
| `thread_count` | `fn thread_count(&self) -> usize` | Live threads on the default artifact. | 2 |
| `last_seq` | `fn last_seq(&self) -> u64` | The log's highest sequence number. | 2 |
| `last_event_of_type` | `fn last_event_of_type(&self, kind: &str) -> Value` | Most recent of that type; panics if none, so a missing event fails loudly. | 3 |
| `count_events` | `fn count_events(&self, kind: &str) -> usize` | How many of that type are in the log. | 8 |
| `open_thread` | `fn open_thread(&self, target: &str, text: &str) -> String` | Opens a thread as the reviewer through ingress; returns the assigned id. | 1 |
| `FakePage::request` | `fn request(&mut self, cmd: Value) -> Value` | Sends a command and returns the direct reply. | 2 |
| `FakePage::open_thread` | `fn open_thread(&mut self, client_id: &str, target: &str) -> String` | Shorthand returning the assigned id. | 2 |
| `FakePage::submit` | `fn submit(&mut self, client_id: &str, verdict: &str, base: u32)` | Sends `review.submit`. | 8 |
| `FakePage::no_frame_within` | `fn no_frame_within(&mut self, d: Duration) -> bool` | True if nothing arrives, for the no-self-broadcast test. | 2 |
| `reviewer_active_recently` | `fn reviewer_active_recently(&self) -> bool` | Whether the reviewer-activity mark is fresh. | 2 |
| `mark_reviewer_activity` | `fn mark_reviewer_activity(&self, at: Instant)` | Drives the nudge clock without a real page ping. | 8 |
| `take_lease` | `fn take_lease(&self, name: &str) -> String` | Acquires through `lease::acquire`; returns the token. | 3 |
| `take_lease_with_takeover` | `fn take_lease_with_takeover(&self, name: &str) -> String` | The same with takeover. | 3 |
| `release_lease` | `fn release_lease(&self, session: &str)` | As a `--follow` disconnect does. | 3 |
| `lease_mode` | `fn lease_mode(&self) -> Option<String>` | `live`, `waiting`, or none. | 6 |
| `age_lease` | `fn age_lease(&self, by: Duration)` | Backdates with `checked_sub`, so a freshly booted machine does not panic. | 3 |
| `simulate_process_exit` | `fn simulate_process_exit(&self)` | Stands in for `await` returning, without killing the test. | 3 |
| `log_passive` / `log_active` | `fn log_…(&self, kind: &str)` | Appends directly, bypassing the page. | 4 |
| `log_active_on` | `fn log_active_on(&self, artifact: &str, kind: &str)` | The same, for a named artifact. | 6 |
| `poison_log` | `fn poison_log(&self)` | Forces the log into its poisoned state, for the failed-append test. | 4 |
| `passive_flush_due` | `fn passive_flush_due(&self) -> bool` | Whether a live-mode flush is due right now. | 5 |
| `run_cli` | `fn run_cli(&self, args: &[&str]) -> CliOut` | Runs against **this** harness's server. `CliOut` is `{code, stdout, stderr}`. | 4 |
| `run_cli_stdin` | `fn run_cli_stdin(&self, args: &[&str], input: &str) -> CliOut` | The same, with stdin, for `reply --stdin`. | 8 |
| `run_json` | `fn run_json(&self, args: &[&str]) -> Value` | `run_cli` plus a parse; panics on a non-zero exit. | 6 |
| `await_now` | `fn await_now(&self, session: &str, secs: u64) -> Value` | Runs `await` to completion. | 6 |
| `await_in_background` | `fn await_in_background(&self, session: &str, secs: u64) -> Waiting` | Starts it on a thread; `Waiting::join` blocks. | 6 |
| `spawn_follow` | `fn spawn_follow(&self, name: &str) -> Follow` | Starts `events --follow`; `Follow::{next_frame, kill, wait_code}`. | 6 |
| `restart_server_in_place` | `fn restart_server_in_place(&self)` | Restarts the listener on the same port without dropping the harness. | 6 |
| `request_stop` | `fn request_stop(&self)` | Sets the stopping flag and appends `server.stopping`. | 6 |
| `push` / `push_json` / `push_file` | `fn …(&self, …) -> …` | Pushes the default or a named fixture. | 7 |
| `write_resolutions` | `fn write_resolutions(&self, entries: &[(&str, &str, &str)]) -> PathBuf` | Writes a `--resolutions` file. | 7 |
| `rendered_body` | `fn rendered_body(&self, artifact: &str) -> String` | The HTML the server would serve now. | 7 |
| `feedback_path` | `fn feedback_path(&self) -> PathBuf` | Where submit wrote the document. | 8 |
| `start_with_away` | `fn start_with_away(after: Duration) -> Harness` | Harness with a real away window. | 8 |
| `start_with_idle_off` | `fn start_with_idle_off() -> Harness` | Idle disabled, for the `off` test. | 8 |
| `support::push_in` | `fn push_in(repo: &TempDir, extra: &[&str]) -> CliOut` | Pushes in a bare repo with no server running. | 7 |
