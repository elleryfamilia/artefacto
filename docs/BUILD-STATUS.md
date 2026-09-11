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

315 tests, `cargo fmt --all --check` and `cargo clippy --all-targets -D warnings`
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
| delivery | `src/server/delivery.rs` | cursor-driven frames, and the offer that makes the next call an ack |
| `await`, `events`, `ack` | `src/server/poll.rs`, `src/commands/agent.rs` | long poll on a thread |
| `push` | `src/server/push.rs`, `src/commands/plan.rs` | base-revision check and resolutions in one commit |
| `reply`, `resolve` | `src/server/verbs.rs` | agent writes, validated inside the append's gate |
| presence and nudges | `src/server/presence.rs` | announced vs. logged, and the two timers |
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

## Where the code diverges from plan 2b, with the reason

- **The lease survives a restart.** Plan 2b's Task 3 test asserts a pre-restart
  token stops validating. Spec 4.2 folds the lease from the log like everything
  else, 6.7 rebuilds "every piece of state", and spec 5 promises `await` retries
  after "the server restarts mid-wait" — which it could not, if the restart
  killed its token.
- **The TTL lets another agent in; it does not punish the holder.** While
  nobody else has taken the lease, presenting its own token revives it.
- **`events --follow` is a loop of long polls, not a streamed response body.**
  Streaming would mean a chunked `tiny_http` response fed by a pipe whose
  flushing is not ours to control, for no gain. The held connection's one real
  benefit — "a `--follow` disconnect releases the lease immediately" — comes
  from `Mode::Live` recording the process's pid instead.
- **The offer is memory-only.** Losing it on a restart costs one redelivery,
  which spec 5 already requires every handler to tolerate.
- **`--artifact` chooses what wakes an agent, not what it may see.** Dropping
  another artifact's events from the middle of a frame would move the
  acknowledged cursor past events nobody received.
- **The change summary is derived**, by diffing the previous plan against the
  new one, because spec 5's push surface has no flag for it.
- **Spec 6.2's 30-second activity throttle is the page's, not the server's.**
  Throttling the server's record instead leaves the recorded time up to half a
  minute stale and fires the idle nudge early.

## Spec edits made

Both plans named two, and building found a third. All three are now in
`docs/specs/2026-09-06-artefacto-design.md`:

1. Spec 5's `await` status table gained a `back` row, matching 6.2's active list.
2. `reply` gained `--nudge`, which 6.3's `nudge` event needed and 5 had no flag for.
3. `plan push` gained optional `--session`, `--agent` and `--takeover`, because
   push is usually an agent's first command and has no token to present yet.

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

## How this was checked

Each slice was built, then its rules were mutated one at a time to confirm a
test fails. The mutations that were tried, and all of which were caught:
deciding the lease outside the gate (eight callers all compute generation 1),
dropping the dead-pid rule, making a refresh mint a new lease, disabling the
TTL, making internal records deliverable, letting an agent hear itself, keying
the offer without a token, running a frame past the first active event,
acknowledging under `--since`, returning from the long poll without waiting,
dropping `--follow`'s pid, skipping the implicit ack, reading the base revision
from the server, appending a revision and its resolutions separately, storing
only a summary instead of the plan, dropping the batch mark, writing the
feedback file after the event, logging presence instead of announcing it,
firing idle every tick, ignoring submission in the away rule, and truncating
the feedback file in place.

The loop was then driven by hand against a real daemon: push with no server
running, bootstrap a page, comment, ask, `await`, `reply`, `resolve`, a stale
push refused with exit 7, submit, and the feedback document on disk with
server-assigned ids. `events --follow` attached as a live lease with its pid,
streamed a frame, survived a same-name push, and exited 0 on `stop`.
