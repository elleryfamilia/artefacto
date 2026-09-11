# Server build status

Current as of 2026-09-10, branch `feat/server-spine`.

## Why this file exists

Plan-set item 2 was written as one implementation plan, reviewed by two
independent models, and drew 42 findings — six of them fatal. It was split into
plans 2a and 2b and rewritten. A second review of the rewrite found that **five
of the ten claimed fixes were wrong or incomplete**, and that several of them
reproduced the original failure in a new form.

The pattern was unambiguous. Every fix that had been compiled and run was
correct. Every fix that had only been written carefully was not. So the
approach changed: build the concurrency-critical parts for real, and let the
plans stand as reasoning rather than as instructions.

**Both plan documents carry a status block naming where the code deliberately
diverges from them. Read that before following either.** Several more
divergences were found by building the rest; they are listed below.

## What is built and green

328 tests, `cargo fmt --all --check` and `cargo clippy --all-targets -D warnings`
clean.

| area | file | notes |
|---|---|---|
| state directory, `server.json`, startup lock | `src/server/state_dir.rs` | atomic write at 0600, page credential derived from the secret |
| event and frame envelope | `src/server/event.rs` | `await_status` is the one table behind spec 6.2's active list and spec 5's status table |
| append-only log | `src/server/log.rs` | byte-offset replay, in-memory tail, strict corruption rules, **atomic multi-event commits** |
| the fold | `src/server/fold.rs`, `review.rs` | all state folds from the log |
| accept loop, guards, drain | `src/server/http.rs` | also holds `Committer`, the mutation gate |
| bootstrap, cookie, CSP | `src/server/page.rs` | strips the document's own meta CSP, stamps every inline tag |
| WebSocket | `src/server/socket.rs` | **outbound only** — see below |
| page commands | `src/server/ingress.rs` | `POST /a/<artifact>/cmd`; submit writes the feedback file first |
| the lease | `src/server/lease.rs` | one gate around decide, mint, append and fold |
| delivery | `src/server/delivery.rs` | cursor-driven frames; the cursor moves only on the agent's `--ack` |
| `await`, `events`, `ack` | `src/server/poll.rs`, `src/commands/agent.rs` | long poll on a thread |
| `push` | `src/server/push.rs`, `src/commands/plan.rs` | base-revision check and resolutions in one commit |
| `reply`, `resolve` | `src/server/verbs.rs` | agent writes, validated inside the append's gate |
| presence and nudges | `src/server/presence.rs` | presence synced from the lease every tick; the two timers |
| the feedback document | `src/server/feedback.rs` | `artefacto.feedback/1`, written atomically before the event |
| CLI client | `src/client.rs` | bearer auth, exit codes, the reconnect rule |
| `serve`, `stop`, `status` | `src/commands/serve.rs` | real double-fork daemon |

## What only running could establish

Each of these was written one way, built, and found to be wrong.

1. **`tiny_http::Server` cannot be built before forking.** `from_listener`
   spawns its accept thread at construction and `fork` keeps only the calling
   thread. `serve` binds a raw `TcpListener`, forks, and builds the server in
   the grandchild.
2. **The upgraded socket is unreachable.** `tiny_http`'s `ReadWrite` is `Read +
   Write` with a blanket impl — no timeout, no non-blocking, no fd. So no
   design that reads on one thread and writes on another is sound, and the
   socket is outbound only with commands over HTTP.
3. **A write to a departed peer succeeds until its RST arrives.** With no
   reader, disconnect detection needs repeated writes, so the writer sends a
   Ping every 20 seconds.
4. **One `write_all` is not a transaction.** A crash can leave two complete,
   newline-terminated records of a three-record commit on disk, and the log's
   torn-tail rule then accepts a revision whose threads were never resolved.
   Every record of a commit carries a batch mark; a log whose last record says
   it is 1 of 3 has that whole group dropped on the next start. Removing the
   mark fails four log tests that simulate the crash on disk.
5. **`server.stopping` and presence must not be logged.** Writing them down
   means the next server to read that log tells every agent it is shutting down,
   and tells every page an agent is here that left hours ago. They are
   delivered and never folded, which is the rule `nudge` follows too.
6. **A second push was refused by its own earlier lease.** Spec 4.2 refuses "a
   second agent", and the lease name is what says which agent a caller is, so a
   claim under the holder's own name rejoins and gets the same token back.
7. **`page_seen` cannot be set from the nudge tick.** A page that connects and
   leaves inside one 250 ms window is never seen by a tick, so the away clock
   never started. The socket layer sets it when the page arrives.
8. **clap reads `Option<T>` on a field as "optional argument".** A
   `value_parser` yielding the `Option` itself is a panic at parse time, not a
   compile error. `--idle off` needs a newtype.

## Review round three: a fresh cross-model pass, and what it changed

After the five slices landed, a fresh reviewer on a different model read the
code against the spec with no context from the build. It found the locking
sound — every append through the gate, lock order held everywhere it traced —
and the **delivery contract wrong in three ordinary ways**, plus four things
the page or a monitor-mode agent would have hit on day one. All of these are
now fixed, each with a test that fails without it:

1. **Delivery was at-most-once across an agent crash.** The server remembered
   the frame it last handed out and acknowledged it on the session's next
   call. An agent that received a frame and restarted before acting on it
   called again with no memory, and the server acknowledged on its behalf —
   the exact failure spec 16 names. Now the agent says what it has dealt with
   (`--ack <seq>` on the next call, or `ack --seq`), the server never guesses,
   and a call that acknowledges nothing is handed the same frame again. The
   test that claimed to cover an agent crash actually exercised a *server*
   restart; it now covers both.
2. **`events` and `--follow` never printed the session token**, so a
   monitor-mode agent could not `reply`. Both print an `artefacto.session/1`
   line first. The follow's first poll returns at once, because a first poll
   that waited its full minute held the token back for as long.
3. **A `--since` replay followed by a plain call exited 2**, because the
   replayed frame's ack was behind the cursor and refused. Acknowledging is
   idempotent now: at or behind the cursor is a no-op.
4. **The timeout tail swept active events into "nothing actionable".** Under
   `--artifact`, another artifact's chat rode along in a timeout and was
   acknowledged without ever being delivered as a chat — and the test
   `await_wakes_only_for_the_artifact_it_was_given` asserted exactly that. The
   tail is passive-only and stops before the first active event of any
   artifact; the wait takes one last look for a frame before giving up.
5. **`push` did not validate the token inside the commit gate.** `reply` and
   `resolve` did; the biggest mutation did not, and a takeover during plan
   validation let a superseded agent publish.
6. **A same-name `push` demoted a live `--follow` lease** to waiting with no
   pid, so killing the follow stopped releasing it, and the pill flipped twice
   per push. A `Waiting` claim never demotes a `Live` lease.
7. **A running server never announced `agent.detached`.** Nothing calls
   `release` in production, and expiry was the lease simply no longer being
   current. Presence is now a view over the lease, synced on every tick; a
   takeover, a release, an expiry and a dead follow all reach the page the
   same way. The tick itself now runs on every accept-loop iteration,
   rate-limited, rather than only when no request arrived.
8. `await` after the server dies mid-wait returned exit 2. Spec 5 says
   `timeout`; it does that now, and the next call finds no server and exits 4.
9. `submitted` never reset on a new revision, so round two of a review could
   never be away or idle. It resets.
10. A page opened against a server older than the idle window was nudged for
    idleness on the first tick. A page's arrival now counts as activity.
11. `--follow` in digest mode printed passive-only timeout frames, waking the
    monitor for nothing. It prints only frames that end at an active event.
12. A corrupt batch mark panicked the server on every start (an integer
    underflow). It is a hard error, and a group missing its earlier members is
    refused whether or not its mark says it is complete.

One reviewer note was adopted rather than argued with: **expiry is a release.**
The build had let an expired lease's holder revive it by presenting its token,
which left `status` saying no agent while a `reply` with the old token still
worked. Now `status`, the pill, and a write all agree.

**Still open from that review**, all notes for plan 3 and the skill rather
than defects: the default `--agent agent` means two harnesses on one repo are
never refused (#13); a feedback-file write failure blocks the submit rather
than committing the event without a path (#14); the push frame is the raw plan
plus resolutions, not a rendered snapshot, so the page recomputes anchoring
(#15); announced events reuse `seq = last_seq()`, so the page must not dedupe
by seq (#16); a follow that receives `stopped` with no events prints nothing
before exiting (#17); a replayed lease after a restart blocks other names for
up to five minutes with an age measured from server start (#18); every push
prints a fresh bootstrap URL into the transcript (#19).

## Review round four: the fix slice, reviewed fresh

A second fresh reviewer on a different model read the fix slice against the
spec and drove twelve probe scenarios against the real binary. Its verdict:
every one of the twelve findings is closed in the code, and it could not make
the server lose or double-acknowledge an event in any scenario, including
expiry mid-wait, takeover mid-wait, a killed follow, and a killed server. It
found three sharp edges, all now fixed with a test each:

- **A long `await` could expire its own lease mid-wait.** The wait now
  re-validates the token every tick, which refreshes the lease and ends the
  wait with exit 6 the moment the lease changes hands.
- **`--takeover` was ignored when a dead token was presented.** A takeover now
  falls through a dead token and claims fresh.
- **The follow still printed a passive-only frame on `stopped`.** A stop is
  signalled by exiting 0, and by nothing else.

And three contract facts the skill (plan 5) and the page (plan 3) must carry,
which are not defects:

- **A page that connects after the agent attached is never told there is an
  agent.** Presence is announced on change only. Plan 3 needs an initial
  presence snapshot on socket connect.
- **A same-name `events --follow` adopts the push's token, and killing the
  follow kills that token.** That is "the token is the identity" plus "a
  follow disconnect releases immediately". The skill must say: on exit 6,
  retry without `--session` under the same name; the cursor is keyed by name
  and nothing is lost.
- **`push` reports `revision_seq`, never `seq`.** Acknowledging a push's own
  event would skip reviewer events the agent never saw. Acknowledge only
  `await` and `events` seqs.

One transient it left alone: `sync_presence` reads the lease and compares
under two separate `core` locks, so a takeover landing in between can announce
the outgoing holder once before the next tick announces the real one. The
final state is always right.

## Where the code diverges from plan 2b, with the reason

- **The lease survives a restart.** Plan 2b's Task 3 test asserts a pre-restart
  token stops validating. Spec 4.2 folds the lease from the log like everything
  else, 6.7 rebuilds "every piece of state", and spec 5 promises `await` retries
  after "the server restarts mid-wait" — which it could not, if the restart
  killed its token.
- **The agent acknowledges; nothing is implicit.** Spec 5's "calling `await`
  or `events` again acknowledges everything the previous call returned" is
  at-most-once, and spec 16 forbids it. The next call acknowledges only what
  the agent names with `--ack`.
- **`events --follow` is a loop of long polls, not a streamed response body.**
  Streaming would mean a chunked `tiny_http` response fed by a pipe whose
  flushing is not ours to control, for no gain. The held connection's one real
  benefit — "a `--follow` disconnect releases the lease immediately" — comes
  from `Mode::Live` recording the process's pid instead. The follow never
  acknowledges on its own; it advances its own read position and the agent
  runs `ack --seq` after acting.
- **`--artifact` chooses what wakes an agent, not what it may see.** Dropping
  another artifact's events from the middle of a frame would move the
  acknowledged cursor past events nobody received. The timeout tail is the
  exception: it stops before any active event.
- **The change summary is derived**, by diffing the previous plan against the
  new one, because spec 5's push surface has no flag for it.
- **Spec 6.2's 30-second activity throttle is the page's, not the server's.**
  Throttling the server's record instead leaves the recorded time up to half a
  minute stale and fires the idle nudge early.

## Spec edits made

Both plans named two, building found a third, and the review found a fourth.
All are in `docs/specs/2026-09-06-artefacto-design.md`:

1. Spec 5's `await` status table gained a `back` row, matching 6.2's active list.
2. `reply` gained `--nudge`, which 6.3's `nudge` event needed and 5 had no flag for.
3. `plan push` gained optional `--session`, `--agent` and `--takeover`, because
   push is usually an agent's first command and has no token to present yet.
4. `await` and `events` gained `--ack SEQ`, and spec 5's delivery paragraph and
   spec 7's rules say the agent acknowledges after acting. The sentence that
   had the next call acknowledge by itself is marked as the at-most-once it
   was. `events` prints an `artefacto.session/1` line first.

## What is not built

- **`status --json` returns a minimal shape.** It has the port, the last seq and
  the lease; spec 5 also wants artifacts, revisions, thread counts, reviewer
  presence, and the `events --follow` command line for the skill to arm.
- **`open` is not implemented.** `push` mints a bootstrap URL, so there is no
  way to get a fresh one without pushing.
- **`--passive live` is not implemented.** Delivery is digest-only, which is
  spec 16's default. Live mode's rate limit (one passive frame per 30 seconds,
  a 5 minute age cap, coalescing repeated edits to the same ref) is plan 2b's
  Task 5 and is untouched.
- **`list`, the artifact index, and posters** are plan 4.
- **`clean`** is plan 4.
- **`skill --print` / `--install`** are plan 5.
- **The page rewrite** is plan 3.

## What is not covered by a test

Stated plainly, because a passing suite is not the same as a covered one.

- **There is still no browser-level test.** Every socket path is driven by a
  fake client, which proves the server's half and nothing about a real
  browser's. "The server delivers a frame" is verified; "the reviewer sees it"
  is not. That closes in plan 3 with a headless-Chromium smoke test.
- **The poisoned log has no test.** `EventLog` refuses every append after a
  failed write, and nothing exercises that path: there is no way to make a
  write fail from a test without a hook that exists only for tests.
- **The `away` timer's real-time path is not exercised end to end.** The tests
  drive `presence::tick` with an explicit clock. The accept loop calls it with
  the real one, which is a one-line difference, but it is a difference.
- **A follow that restarts mid-stream is not tested as a process.** That it
  replays what was printed but never acknowledged follows from the follow
  acknowledging nothing (tested) and the cursor being in the log (tested); no
  test kills a follow after a frame and starts another.

## How this was checked

Each slice was built, then its rules were mutated one at a time to confirm a
test fails. The mutations that were tried, and all of which were caught:
deciding the lease outside the gate (eight callers all compute generation 1),
dropping the dead-pid rule, making a refresh mint a new lease, disabling the
TTL, making internal records deliverable, letting an agent hear itself,
running a frame past the first active event, returning from the long poll
without waiting, dropping `--follow`'s pid, reading the base revision from the
server, appending a revision and its resolutions separately, storing only a
summary instead of the plan, dropping the batch mark, writing the feedback
file after the event, logging presence instead of announcing it, firing idle
every tick, ignoring submission in the away rule, and truncating the feedback
file in place.

The fix slice after the review was checked the same way: having the server
acknowledge the frame it hands out, printing no session line, refusing an ack
behind the cursor, letting the timeout tail carry active events, skipping the
token check in push's gate, demoting a live lease on a waiting claim, never
announcing detached, never resetting `submitted`, and reverting the batch-mark
subtraction to one that underflows. Each fails the test that names it. The
follow-up slice added three more: removing the per-tick validation from the
wait, refusing a dead token even with `--takeover`, and printing a passive
tail on `stopped`.

The loop was then driven by hand against a real daemon, twice. First: push
with no server running, bootstrap a page, comment, ask, `await`, `reply`,
`resolve`, a stale push refused with exit 7, submit, and the feedback document
on disk with server-assigned ids. After the fixes: a second `await` without
`--ack` handed back the same frame and `--ack` moved the cursor; `events
--follow` printed its session line at once, that token worked for `reply`, the
lease was live with the follow's pid and gone within half a second of `kill
-9`; and an `await` whose daemon was killed mid-wait returned `timeout` with
exit 0, with the next call exiting 4.
