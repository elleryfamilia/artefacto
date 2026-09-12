# Server build status

Current as of 2026-09-11, branch `feat/skill`, with plan 4 built on top of plan 5.

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

508 tests, `cargo fmt --all --check` and `cargo clippy --all-targets -D warnings`
clean. Sixty-two of the tests run the served pages in a headless Chromium;
they skip with a printed line on a machine without one (see "Plan 3" below).

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
| the served page | `src/server/page.rs` | the rendered plan at `/a/<artifact>`, the JSON snapshot at `/a/<artifact>/state`, both read under the commit gate |
| the page itself | `src/plan/assets/plan.js`, `plan.css` | one pure fold, a re-entrant `mount(root)`, the socket client, POSTed commands, composers with drafts, the body swap and its restore, presence, notices, the recovery panel; the static export in the same file |
| the browser harness | `tests/support/browser.rs`, `tests/browser.rs` | headless Chromium over the DevTools protocol, tungstenite as the client |
| `open` | `src/commands/serve.rs`, `src/server/page.rs` | a fresh one-time link over the CLI route; starts the server if none |
| `status --json` | `src/server/status.rs` | artifacts, threads, cursors, presence, and the follow line, read at one moment |
| the skill | `src/commands/skill.rs`, `skills/artefacto-plan/` | `--print` manifest and `--install DIR`; the package is compiled in |
| the skill's proofs | `tests/skill_package.rs`, `tests/skill_loop.rs` | every prescribed command parses; the loop runs against a real daemon in both modes |
| the artifact index | `src/index.rs` | `index.json` in the state directory, one row per artifact, under an advisory lock; a newer file is read as empty and never rewritten, a corrupt one is repaired by the next write, nothing is pruned |
| the poster | `src/plan/poster.rs` | a deterministic SVG card from the plan model and the review state, pinned by a golden fixture |
| `list` | `src/commands/list.rs` | the registry, newest first, with no server |
| the row's upkeep | `src/server/http.rs` (`Committer::append_all`), `src/server/fold.rs` | every commit that changes a row rewrites it and redraws the poster; the fold keeps the last verdict and the revision's time |
| the index page | `src/server/index_page.rs` | `/`, cookie-gated, posters inline, per-row remove as a page write; `open` lands here with several artifacts |
| `clean` | `src/commands/clean.rs`, `src/server/log.rs` (`clean`) | sent reviews out of the log at their numbers, a `log.cleaned` record carrying the high-water mark, the secret rotated, the index kept |
| releases | `dist-workspace.toml`, `.github/workflows/release.yml` | cargo-dist 0.32: four macOS and Linux targets, a shell installer, GitHub releases on a `v*` tag |

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

## Plan 3: the page

There was no plan document for the page; spec 4.3 and the notes the earlier
reviews left for plan 3 were the plan. It was built as three slices, each
tested, mutated, and reviewed fresh before the next: the server side of the
page, the browser harness, and the page itself.

### What the server does for the page

- **`GET /a/<artifact>`** serves the rendered plan from the fold, nonce-stamped,
  with `data-artefacto-artifact` and `data-artefacto-revision` on `<body>`.
  Those two attributes are how the script knows it was served; a body without
  them is a static export.
- **`GET /a/<artifact>/state`** returns everything a page needs to show a
  review from nothing: the rendered `<body>` fragment (markers and all, the
  page's own script stripped, the data island kept), the raw plan, threads,
  answers, marks, chat, `submitted`, the lease holder, and `last_seq`. The fold
  and `last_seq` are read under the commit gate, so they describe one moment.
- **The socket's hello carries presence.** Presence is announced on change
  only, so a page that connects after the agent attached would otherwise never
  be told. It carries nothing else: a `last_seq` read outside the gate is not a
  catch-up threshold, and a page that used it would apply an event twice.
- **A push's frame to pages carries the rendered body** as a top-level `html`
  field, so the page gets spec 4.3's "one snapshot holding the rendered body,
  thread state, and resolutions together". Frames to agents are built from
  the log and never carry it.
- **Broadcasts run under the commit gate.** They take the innermost lock and
  `try_send`, and never block. Released first, two commits could reach a page
  as 6 then 5.
- **A page is registered before its 101 goes out.** tiny_http flushes the
  handshake before `upgrade` returns, and the browser's `open` event fires on
  receipt and fetches `/state` at once; a page registered afterwards could
  miss a commit that landed in between.
- **The anchor set is every element the renderer marks**: the plan summary,
  each question and risk, each phase and task. The page offers a comment
  button on all of them; the server had refused everything but phases and
  tasks.
- **A stop closes every page socket after announcing itself**, so pages start
  reconnecting now rather than when the process dies.
- The CSP's `connect-src` names the page's own origin as well as the socket;
  page routes strip the query and accept only `/a/<id>`, `/a/<id>/cmd` and
  `/a/<id>/state`.

### What the page does

- **One pure fold** (`core.applyEvent`) mirrors `fold.rs`: thread ids,
  replies, edits, deletes, resolutions with the note as an agent message,
  answers, marks, chat, `submitted`, presence, and re-anchoring from the raw
  plan a revision event carries. A frame, a snapshot, and the page's own
  command reply all go through it.
- **The server is the only store.** Nothing on a served page reads
  localStorage for review state. Every command carries a client id, minted
  when its composer opens and stored with the draft, so a send repeated after
  a swap, a reload, or a retry is the same command. The page applies its own
  write from the reply, because the broadcast skips the page that posted; a
  socket frame carrying one of the page's own client ids is skipped too,
  because after a reconnect a write in flight may have named the old socket.
  A reply with `seq: 0` is the server saying it already did this, and the
  page resyncs rather than guess.
- **Catch-up.** On every socket open the page fetches `/state`, replaces its
  state, then drains what was buffered meanwhile — socket frames and the
  page's own replies alike — skipping *logged* events with `seq <= last_seq`
  and never skipping announced ones (`agent.attached`, `agent.detached`,
  `nudge`, `server.stopping`), which borrow that seq. A resync started while
  another is in flight supersedes it. A write whose reply was lost after
  every retry is looked up with a resync, because only a snapshot can show a
  page its own write.
- **Only this artifact's events apply.** The socket carries every artifact's
  frames; an event naming another artifact is dropped before the fold, so
  another plan's comment does not count here and its push does not swap
  this body. Events with no artifact (presence, the stop) apply.
- **One fold path.** Every event — a socket frame, a buffered reply, a
  direct reply — passes through `applyEvents`, which applies the artifact
  filter, the own-client-id filter, and a per-key highest-seq rule for
  set-valued writes (a reviewed mark per element, an answer per question),
  so two replies for one mark settle on the server's value whichever order
  they arrive in. The buffer drains in log order.
- **A push swaps the body**, mounts again, and restores disclosure by element
  id, focus and caret by composer id, and scroll by element anchor. Every
  composer keeps its draft in sessionStorage under an id minted when it
  opened, with the revision it opened against, and sends that as
  `opened_revision`. A thread or draft whose element is gone is listed in a
  recovery panel at the top; nothing is dropped.
- Presence pill, revision banner with the previous title on hover, nudge and
  stop notices, reconnect with backoff (eight tries, then a loud "gone" with
  Retry), a lost session (401) as a notice, activity pings throttled to one
  per 30 seconds.
- The static export keeps localStorage and the clipboard, and gains the
  Approve toggle spec 4.3 gives it.

### The browser harness

Spec 14 offered two options. The CDP one was built: `tests/support/browser.rs`
launches a headless Chromium with a DevTools port, opens each tab as its own
target, evaluates JavaScript in the page with real waits, and records console
errors and browser log entries, which is where a CSP refusal shows up. No new
dependency: tungstenite already speaks WebSocket. It finds a browser in
Playwright's cache, `/Applications`, `/usr/bin`, or `ARTEFACTO_CHROME`; without
one the tests print a skip line and pass, and `ARTEFACTO_REQUIRE_BROWSER=1`
makes that a failure. **CI must install a Chromium or set that variable**, or
the browser suite is silently green.

The sixty-one tests cover the loop end to end and the races spec 14 names:
a push while typing (draft kept, `opened_revision` is the old one), a draft
and a thread whose element was removed (recovery panel, re-anchoring when it
returns), focus and caret across a push, scroll anchored to an element across
a push that inserts a phase above, two tabs on one artifact, duplicate
suppression after a real daemon restart, a write made while catching up on
either side of the snapshot, a frame received while catching up, the page's
own write delivered back to it, a push and a reload during a send, and a
lost cookie. Tests shape the page's `fetch` from inside the page (hold a
request, hold a response, drop a response) to put a write on a chosen side
of a snapshot. The static export's own `#selftest` harness runs in the same
browser, which is the smoke rosita had.

### What only running could establish

1. **Smooth scrolling never advances in a headless page.** `plan.css` asks for
   it, so `scrollIntoView` left `scrollY` at zero and the restore's `scrollBy`
   would have read mid-animation. Both scroll with `behavior: "instant"`.
2. **`element.focus()` is a no-op without focus emulation**, and an element
   inside a closed `<details>` has no layout and cannot be focused at all. The
   harness enables `Emulation.setFocusEmulationEnabled`; the test opens the
   phase, as a reviewer would have to.
3. **An in-process server restart hangs the page's next fetch forever.** The
   browser keeps idle HTTP connections alive and reuses one; tiny_http decides
   keep-alive from the request only and drops any `Connection` header a
   handler sets, and a browser fetch cannot set one either. A process exit
   resets the connections, so the reconnect test restarts the real daemon.
4. **The push that serves a page holds the lease**, so the pill's first word
   is "agent waiting" and the hello presence is what the test actually
   proves. Expiry then flips it to "no agent" through the tick.

## Review round five: the page's server slice, reviewed fresh

A fresh reviewer on a different model read the server slice (commits
`fe9cf81`, `f7b45b0`) against the spec and drove it in a detached worktree.
It found the lock order sound on every new acquisition, the snapshot atomic,
both catch-up orderings correct, and hostile text in every plan field escaped.
Five confirmed defects, all fixed with a test each:

1. **A page was registered for broadcasts after the 101 was on the wire.**
   Observed once in 300 real handshakes. Registration now precedes the
   upgrade; hello is still first because the channel drains after it.
2. **The body fragment dropped the served body's markers**, so a whole-body
   swap would have put the page into static mode. The fragment now carries
   them, with the new revision.
3. **The placeholder's inline script was injectable through the raw path**;
   `serde_json` does not escape `</script>`. Not reachable from a browser,
   which percent-encodes the path. Escaped like the render's data island.
4. **A query string or a trailing slash on a page route served the
   placeholder with a 200.** Page routes strip the query and 404 anything but
   the three shapes.
5. **A stored plan the strict parser refuses gave a 500 JSON body to a
   navigation.** The read path parses leniently; a plan that still will not
   render gets an HTML page that says so.

Two suspicions adopted: broadcasts now run under the gate (frames could reach a
page out of log order), and hello no longer carries `last_seq`. One left: the
snapshot has no revision summary, which only matters if the banner had to
survive a reload.

## Review round six: the page, reviewed fresh

A fresh reviewer on a different model read the page slice (commits `94c133c`,
`ee7a5cd`) and drove it in a real Chromium with probe tests. Nine confirmed
defects, all fixed with a browser test each (commit `cce2067`):

1. **The page's own write was lost when a resync snapshot landed after the
   reply.** A reply that arrives while catching up is now buffered as a
   one-event frame with the seq the server assigned; the snapshot's `last_seq`
   decides whether it is already inside.
2. **A push during an in-flight send left the sent composer open, and a
   second click sent a new client id.** The client id is minted with the
   draft, and the reply closes every composer with that draft's id.
3. **A thread was rendered once per acceptance row of its task**, because
   every row carries the task's ref. Threads and composers live on the first
   element carrying a ref; a row's button still quotes the row.
4. **A write posted with the old socket's id after a reconnect was broadcast
   to the new socket and applied twice.** A frame carrying one of the page's
   own client ids is skipped, and a closed socket forgets its id.
5. **A reply, ask or edit draft was invisible after a reload**, because drafts
   were restored before the threads existed. They are restored after every
   render.
6. **A push while typing in the chat panel hid the panel.** Its open state
   lives in the session.
7. **Reply and Ask on an unanchored thread did nothing.** The recovery panel's
   thread host is keyed and kept, and a composer opens there.
8. **A lost cookie on reconnect read as "server gone".** The handshake's 401 is
   invisible to script; one `/state` request at the end of the backoff tells
   the two apart.
9. **No fetch had a deadline, and Send review re-enabled mid-flight.**

Suspicions adopted: a resync generation counter, a banner for a revision
learned from a snapshot (the fold now keeps the summary and `/state` returns
it), a pending reviewed mark keeps the reviewer's choice, a swap no longer
delays the next ping, a `/state` 404 marks the page lost, notices render their
backtick spans as code. Left as is: the theme toggle reads localStorage on a
served page — a viewer preference, not review state.

The review also found that two of the original tests could not detect the
server broadcasting to the poster, because opening a thread is idempotent by
id; both now assert on a reply as well. The mutation pass over the fixes
caught nine of eleven. The two survivors are redundancy: the chat panel's
creation-time `hidden` is re-applied by `renderChat` (removing both fails the
test), and the server's skip-the-poster rule is now shadowed by the page's
own-client-id filter, which is the robust direction — a page that hears its
own write once more is unaffected.

## Review round seven: the fix slice, reviewed fresh

A third fresh reviewer read the page's fix slice (commit `cce2067`) and drove
it with probe tests. Eight confirmed defects, two of them pre-existing and
serious, all fixed with a browser test each (commit `c1c5613`):

1. **Another artifact's frames were applied to this page**, including its
   push's body swap: the socket carries every artifact's frames and the page
   never checked. Events naming another artifact are dropped.
2. **A phase's Comment button opened its composer inside the phase's first
   task**, because the host was found by descent and a phase contains its
   tasks; with a task composer open, the phase click was swallowed as a
   twin. Composer hosts are keyed by ref, like the thread hosts.
3. **The catch-up test could not see a double apply of the page's own
   frame**, because it opened a thread, which is idempotent by id. It
   replies now.
4. **A pending reviewed mark did not survive a body swap.** Pending marks
   live in the session, not on a label the swap rebuilds.
5. **Nine of the smaller fixes had no test.** Six do now; three still do
   not (below).
6. **A write whose reply was lost after every retry was never shown** until
   the next resync, which could be never. The final failure triggers one.
7. **A revision learned from a snapshot reported no addressed or declined
   counts**; they are read off the two states.
8. **A chat draft restored after a reload was invisible** behind the closed
   panel. The panel opens for it.

Suspicions adopted: a reply answered after the newest snapshot that already
held it is not applied on top; the end-of-backoff probe takes a 200 as the
state it is and retries the socket; orphaned-draft rows are rebuilt only when
the set changes. Left as is, and noted: two reviewed clicks whose replies
arrive reversed could end on the wrong value (the page shows the last click
until both replies are in, then the server's fold); Cancel on a composer the
swap re-created while its send is in flight still lets the write land; the
test hooks on `window.artefactoPlan` are reachable only by script already
running under the nonce CSP.

The mutation pass over these fixes caught ten of eleven; the survivor was the
snapshot-covered-reply guard, which round eight then gave a test.

## Review round eight: the second fix slice, reviewed fresh

A fourth fresh reviewer read commit `c1c5613` and drove it. Three confirmed
defects, one of them a regression from round seven's chat-draft fix, plus a
contrived fourth and three fixes with no test. All fixed or tested in commit
`919b46f`:

1. **The chat panel reopened on every render while a chat draft existed**,
   because the reload fix ran inside every render and a draft is saved the
   moment its composer opens. A draft now opens the panel once, the first
   time the page finds it with text in it; opening or closing the panel by
   hand counts as having seen it; closing with nothing written drops the
   empty draft.
2. **Two reviewed-mark replies arriving in reverse order settled the page on
   the earlier write.** A set-valued write (a mark per ref, an answer per
   question) records the highest seq applied for its key; an older reply
   arriving later is not applied on top. Thread messages are append-only and
   are not reordered; a resync shows the server's order.
3. **A mark answered while catching up flickered off** until its buffered
   reply drained. A pending mark stays pending until then.
4. **The end-of-backoff probe could cycle forever** while HTTP answered and
   the socket kept failing. Three cycles, then "gone" with Retry. Reachable
   only with a socket that fails while `/state` answers, which nothing on
   loopback produces; fixed because spec 4.3 promises a bounded number of
   retries.

Tests were added for the three fixes that had none: the snapshot-covered
reply guard, the probe's recovery path, and the probe's bound. Adopted
suspicions: the probe carries a resync generation and reads a 404 as lost; a
failed resync applies the page's own buffered replies whatever their seq; a
thread going from changed to declined between snapshots counts in the banner;
an error line clears on the next success. Left as is: the orphaned-draft
row key is an optimisation with no observable effect to test.

The mutation pass caught six of seven. The survivor is the null own-cursor
on a failed resync: a write made while catching up always has a higher seq
than anything applied before, so both branches behave the same and the rule
cannot be reached.

## Review round nine: the third fix slice, reviewed fresh

A fifth fresh reviewer read commit `919b46f` and drove it. Five confirmed
defects, all fixed with a browser test each (commit `95a9f1a`):

1. **The per-key seq rule guarded only the direct-reply path.** A frame from
   another tab, or a buffered own reply drained after a newer one, bypassed
   it, so an older reply still won. Every event now passes through one
   function, and the buffer drains in log order.
2. **The end-of-backoff probe applied a snapshot without marking the page as
   catching up**, so a write whose reply landed while the probe's request
   was in flight was dropped by the snapshot: the same shape as round six's
   first defect, in a path round seven had added. The probe is now an
   ordinary resync.
3. **A toggle's error line was appended inside its label**, so reading it
   flipped the mark. It sits after the label.
4. **Five changes from round eight had no test.** Four do now (the failed
   resync's own cursor remains unreachable, above).
5. **A `/state` 404 set the page lost without re-rendering** the pill and
   the bar.

Suspicions adopted: three tests that depended on fixed delays now hold
responses until the test releases them; a socket that opens and closes at
once no longer resets the retry budget (it comes back only after the socket
has stayed open three seconds); the "gone" notice says whether HTTP answers;
a sent chat message leaves a fresh composer in the open panel.

The mutation pass caught seven of nine. The survivors: the buffer sort is
redundant with the per-key rule for set-valued writes (round ten gave it a
test with appended messages); and the "probe not marked as catching up"
mutation was a no-op, because the probe is now the resync that marks it.

## Review round ten: the fourth fix slice, reviewed fresh

A sixth fresh reviewer read commit `95a9f1a` and drove it. Two confirmed
defects and three test findings, all closed in commit `270cdef`:

1. **A resync that overtook the probe's own read as "server gone".** A
   retried write's reply starts a resync; if it superseded the probe's, the
   probe took "superseded" for "not answering", declared the server gone
   and stopped reconnecting while HTTP answered and a socket was there. A
   resync now reports applied, superseded, or failed, and the probe asks
   again on superseded.
2. **A phase's error line toggled the phase**: it sits after the label
   inside the summary, and the summary's click handler ignored only
   controls. The line stops its own clicks and the handler ignores it.
3. **The probe-write test asserted after a real reconnect had already
   repaired the page.** It keeps the socket failing and asserts on the
   probe's own snapshot.
4. **The buffer sort had no detecting test**, because set-valued marks
   settle by seq whatever the order. A test with appended messages shows
   it.
5. **Two mark tests waited by sleeping.** They wait on the page's pending
   marks, which `debug()` now reports; the fetch shim releases held
   responses per path.

Adopted: a lost page clears its catching-up state; a chat composer opened
after a send stores no draft until typed into. Left, and noted: a socket
that lives under three seconds each time takes a few minutes of cycles
before "gone" (bounded); a re-push during a socket outage can flicker the
"gone" notice once; the end-of-backoff wording says whether HTTP answered.

The mutation pass caught five of six (two after adding the assertions they
lacked). The survivor was the summary handler's exclusion of error lines,
which the line's own click handler made redundant; round eleven removed it.

## Review round eleven: the fifth fix slice, reviewed fresh

A seventh fresh reviewer read commit `270cdef` and drove it. Two confirmed
defects, one a regression from round ten, and three test findings, all
closed in commit `3403a82`:

1. **A push after a sent chat message left the open panel with nowhere to
   write.** Round ten's lazy composer stores no draft until typed into, and
   a body swap restores composers from drafts only. An open panel now always
   gets a composer after the drafts are restored.
2. **The superseded-probe timer could probe again on a live socket.** A
   superseding resync that swapped the body reconnected the socket through
   mount; the timer then ran the probe anyway, and a failed snapshot read as
   "server gone" while frames were still arriving. A superseded probe now
   counts as an answered one, bounded like it; nothing schedules a reconnect
   while a socket is open; Retry clears the notice it answers.
3. **Three waits were satisfied before the reply they named**, because the
   pending-mark count counts keys, not replies. They wait on the page's
   buffer and applied counts.
4. **The lost block in a resync had no test**; a test found it also left a
   pending mark behind, which it now clears.
5. **The summary handler's exclusion of error lines was dead code**; the
   line's `preventDefault` is the mechanism. Removed, with the redundant
   `stopPropagation`.

The mutation pass caught three of five. The survivors were the two
live-socket guards: with the timer gone, a superseded probe's own
`connect()` is a no-op on a live socket, so nothing reaches them. Round
twelve found the first of them dead by construction and removed it; the
probe-callback guard stays as defence.

## Review round twelve: the sixth fix slice, reviewed fresh, and why the loop stops

An eighth fresh reviewer read commit `3403a82` and drove it. Three confirmed
defects, all low to medium and none in the reconnect logic, two test-strength
gaps, and two suspicions, all closed in commit `44376f9`:

1. **Focus in a chat composer nobody had typed into was lost across a
   push**: it had no draft to be restored from, and its replacement got a
   new id the focus restore could not find. The panel's composer keeps one
   id per page load, however it was opened.
2. **Retry cleared the notice but left the pill saying "server gone"** until
   the next attempt settled. It renders the pill.
3. **Cancel on the chat's only composer was undone by the next render**,
   which gave the open panel a composer back. Cancel closes the chat.
4. A guard in `scheduleReconnect` was dead by construction (the socket is
   always null there) and is gone; the lost path's render had no test and
   has one.
5. **A lost page discarded the reviewer's own buffered replies**, so a mark
   the server had accepted showed unchecked. It applies them before it
   stops.

The mutation pass caught four of four.

**The loop stops here.** Eight rounds on the page found 5, 9, 8, 3, 5, 2, 2
and 3 confirmed defects. Every real finding after round seven was in the
reconnect and catch-up machinery, and from round eight on the most serious
finding in each round was a regression introduced by the previous round's
fix. Round twelve's reviewer found nothing in that machinery. What remains
is the polish and test-strength level, and a ninth round would be paid for
in the same coin as the last three: one narrow finding, one regression from
fixing it. The judgement is recorded here rather than tested for: the
next real finding on the page will come from use, not from another read.

## What the first real use found

The owner opened the page against a real daemon after the twelve rounds and
found three things in the first minute that no review had (commit
`4320e9b`):

1. **Send review worked and looked like it did nothing.** The log held four
   `review.submitted` events, one per click; the only sign had been small
   grey text in the bar. A sent review now puts a notice at the top saying
   what went out, pulses the button, stamps the time, and relabels the
   button "Send again".
2. **A phase's comment landed after the phase's last task**, where it read
   as a comment on that task. A phase's threads and composers sit under the
   phase's own header, above its tasks.
3. **A comment and an answer looked like two different things, and an
   answer could not be edited or removed.** Both are one card now; answers
   have Edit and Remove, and the feedback document leaves an emptied answer
   out.

Every one of these was visible in a screenshot, which is what the reviews
never took. The browser harness can take one now (`Page::screenshot`), and
the orientation banner on a served page says "send your review" rather
than "copy your feedback". The lesson goes with the one from round twelve:
the reviews found what a read finds; what a look finds is different.

## Plan 5: `open`, `status --json`, and the skill

Spec 4.5 and 7, with `open` and the fuller `status --json` from spec 5 built
first because the skill needs both. Three slices, each built, tested,
mutated, and committed before the next; then the prose; then a hand-drive.

### What was built

- **`artefacto open [--artifact ID] [--no-open] [--json]`.** Asks the server
  for a fresh bootstrap link over the authenticated CLI route
  (`POST /cli/open`), prints it, and opens the browser unless `--no-open` or
  `--json`. With no name it follows `reply`'s rule: one artifact needs no
  name, several do. An id the fold does not know is refused (exit 2) rather
  than minted for, because a link that lands on the placeholder page is a
  link that lied. **It starts the server if none is running**, as `push`
  does: the page's advice for a dead link or a gone server is "run
  `artefacto open`", and after a self-exit the log is still there, so one
  command should be enough.
- **`status --json`** now carries what spec 5 lists: each artifact with its
  kind, title, revision, hash, source and feedback paths, `submitted`, open,
  unanchored and blocking counts, page-level chat, answer and reviewed
  counts; every lease's cursor; the holder with its age and `acked_seq`;
  reviewer presence (`pages`, `present`, `seen`, `last_activity_secs`,
  `idle`, `away`); and the follow line as both `follow.command` and
  `follow.argv`, under the holder's name. The artifacts and `last_seq` are
  read under the commit gate, as `/state` reads them, so they describe one
  moment. Beyond the spec's list: each artifact's **thread list with its
  messages**, and the page-level chat, as `{actor, text, ts}` in log order,
  because spec 7's rule 3 ("check the thread for an existing agent reply
  first") was not executable from anything an agent could run. The token is
  never in the output; `Holder` has no field for it. The text mode prints
  the same facts in five lines.
- **`artefacto skill --print | --install DIR`.** The manifest is
  `{"format":"artefacto.skill/1","artefacto":"<version>","skills":[{"name",
  "files":[{"path","contents"}]}]}`; the directory form writes the same
  files, replacing what is there. Both are `include_str!` of
  `skills/artefacto-plan/`, so a binary is a complete distribution of the
  skill that matches it, and `plan schema` prints the same reference.
- **The skill.** `SKILL.md` is written to be invoked by the model, with a
  description that says when it applies, a generic section (push, the poll
  loop, act by the frame's last event, acknowledge, address a review with
  `--resolutions`), and a Claude Code section (arm a Monitor on the follow
  line, `ack` after each frame, `PushNotification` on `away`, `TaskStop`
  when done, and what each monitor exit code means). `reference.md` keeps
  its schema half unchanged and replaces the command half with the real
  surface: synopsis, every result's shape, the event table with each type's
  `data` and whether it wakes the agent, the feedback document as the server
  writes it, the resolutions file, and `status --json`.

### How the prose is kept honest

- **Every command line in a fenced `bash` block of either file is parsed by
  the real clap definition** (`tests/skill_package.rs`), after `<seq>` and
  `"$SESSION"` placeholders are filled. A synopsis lives in a `text` block,
  which the extractor ignores. Renaming a flag in the binary, or misspelling
  one in the prose, fails the test; both were mutated to prove it.
- **`tests/skill_loop.rs` drives a real daemon with exactly the commands the
  skill prescribes**, in both modes. Monitor mode: push with no server, arm
  the `follow.argv` status prints, the session line equals the push's
  token, a chat frame with its passive event, the thread's messages before
  and after the reply, `ack` twice, the submitted frame's document equals the
  file on disk, a revision with resolutions in one push, exit 7 on a stale
  base, kill the follow, exit 6 on the dead token, re-arm without
  `--session` and resume at the cursor with nothing replayed, exit 0 on
  `stop`. Poll mode: rejoin by name with no token, the same frame again
  without `--ack`, the cursor moving with it, the page-level chat's
  messages before and after a reply, and a submitted review acknowledged by
  the next call. A third test ages a waiting lease past its TTL and drives
  the recovery the skill prescribes. The reviewer's cookie comes from `open`, as a browser's would;
  nothing reaches into the server.
- **The reference's JSON plan examples still validate** under the real
  deserializer, as before.

### What only running could establish

1. **macOS has no `timeout`.** The first hand-drive's follow never started
   and every later step read an empty file. A harness fact, not a product
   one; the second drive used a background process and `kill -9`.
2. **A departed page is noticed only on a failed write**, so a status test
   that closes a page must drive writes (`broadcast_test_frame`) as the
   loop and page tests already do, or wait twenty seconds for the
   heartbeat.
3. **A "last actor" rule survived its own test** with one page-level
   message, where first and last coincide, and was caught only by the loop
   test; the review then found the rule itself wrong (round thirteen), and
   the field is gone.

### Where this diverges from the spec, with the reason

- `open` has `--no-open` and `--json`, which spec 5 did not list; every
  other browser-opening command has them, and a test cannot open a
  browser. Recorded in the spec's CLI surface. It starts the server only
  when the state directory's log holds an artifact (a `revision.published`
  record, found by a byte search that survives a torn tail); with nothing
  ever pushed it starts nothing and says so.
- `--agent` refuses an empty name, a leading `-`, and control characters.
  The name comes back out of `status --json` as a command line, and a name
  clap could not read back is a follow line that cannot be armed. The rule
  lives in the lease (`lease::valid_name`), which every claim passes
  through, so a caller speaking HTTP with the bearer is refused the same
  way (400, `invalid_agent`, exit 2); the CLI's parser delegates to it.
- `await` and `events` results carry `cursor`, where the call started
  reading, alongside `seq`, the acknowledgement point. The `events` session
  record's `seq` is that cursor, which is what spec 5 calls it.
- The follow line is unscoped: `artefacto events --follow --agent <name>`,
  no `--artifact`. The status route does not know which artifact the
  caller is working on, and a scoped follow would miss another artifact's
  review on the same server.
- `status --json` prints more than spec 5 lists (above). Nothing it lists
  is missing.
- The manifest's shape is this plan's choice; spec 4.5 says only "a JSON
  manifest of relative paths and their contents".

### What is not covered by a test

- **`open_browser` is never exercised**: every test passes `--no-open` or
  `--json`, because a test that opened a real browser would open one on the
  developer's machine. The rule that `--json` implies no browser is a unit
  test on `should_open`, shared with `render`.
- **The Claude Code section describes another tool.** Monitor,
  PushNotification and TaskStop are named from their current definitions;
  no test runs them. The follow line, the session line, and the per-frame
  `ack` are what the loop test proves.
- **`reviewer.idle` and `reviewer.away` are not in the loop test.** The
  nudge command parses, and the events themselves are tested in
  `server_loop.rs`; the skill's handling of them is prose.
- **A follow whose server dies without a `stop` also exits 0** (the follow
  treats a vanished server as the stop, by design). The skill says exit 0
  means the server stopped and tells the agent to push or `serve` if the
  review is still open, which covers both, but the two are not told apart.
- `status --json`'s `answers` count has no fixture with a question behind
  it in the status test; it mirrors `feedback.rs`'s non-empty rule, which
  is tested there. `last_activity_secs` is asserted to exist, not for its
  value. The `cursor` on `await`'s synthesised unreachable timeout is
  asserted nowhere: no test reaches that path.
- **The `revision_seq` hazard is prose only.** Nothing stops an agent from
  running `ack --seq <revision_seq>` and skipping reviewer events it never
  saw. A server-side guard (refuse an ack beyond the highest seq delivered
  to that name) would make the rule structural; it is noted, not built,
  because delivery is not logged and the guard would have a hole across a
  restart.

### The hand-drive

Against a real daemon in a scratch repository with the built binary, twice.
First: push with no server, `status` in both modes, `open --json` and the
cookie from its link, a thread and a chat from the page, `reply`,
`resolve`, a push with resolutions, a stale push refused with exit 7 and a
message naming `status --json`, `stop`, then `status` exiting 4. Second,
the monitor half: the follow line copied from `status`, its session line
carrying the push's token and the lease live with the follow's pid, the
chat frame with its passive event first, the thread's messages showing the
reply, `ack`, the submitted frame naming the feedback file, the agent's
own push not delivered back, `kill -9` releasing the lease within half a
second, the dead token refused with exit 6 and a message saying why, the
re-armed follow's session line at the acknowledged cursor with a
generation-2 token, and `stop` exiting it 0 with nothing printed but that
line.

### Review round thirteen: plan 5, reviewed fresh

A fresh reviewer on a different model read the five commits against the
spec in a detached worktree, followed SKILL.md step by step against a real
daemon, ran a five-and-a-half-minute lease-expiry experiment and a hostile
agent-name experiment, and found the locking sound, no credential in
`status`, the open route gated, the manifest as described, and every spec 7
rule present. Five confirmed defects, two suspicions, ten prose findings,
and eight test-strength findings. What changed:

1. **The "check first" rule lost the second of two back-to-back questions**
   (high). The skill said: if the thread's last message is yours, skip.
   After answering the first of two questions the last message is the
   agent's and the second is unanswered, and no seq comparison can tell the
   two apart either, because the reply to the first comes after the second
   in the log. `status --json` now carries every thread's `messages` and
   the page-level `chat` as arrays of `{actor, text, ts}` in log order, and
   the rule is what spec 7 literally says: read the thread and reply to what
   is unanswered. `last_actor` and `chat_last_actor` are gone; they invited
   the wrong check. The loop test drives two questions in a row and asserts
   the data the rule reads.
2. **The poll loop was told the wrong recovery for its own expired token.**
   Five minutes without a call releases a waiting lease, and every call
   with the old token then exits 6 — the same code as "another agent holds
   it" — where the skill said to consider `--takeover`. One rule for both
   modes now: call again without `--session` under the same name; if that
   answers, its token is the new one and the cursor was kept; if it exits 6
   too, another agent has the review. A test ages the lease and drives the
   recovery.
3. **An agent name starting with `-` produced an unrunnable follow line.**
   clap refuses a hyphen-leading value in separated form, and `--agent=-x`
   was the one way to choose such a name. `--agent` now refuses an empty
   name, a leading `-`, and control characters, on every command that
   takes it.
4. **`open` on a repository with nothing pushed started a daemon and
   abandoned it** for half an hour. It starts one only when the state
   directory's log holds an artifact; otherwise it says "push a plan first"
   and starts nothing. The test that had run `serve` first hid this; it now
   runs `serve`, an `await`, and `stop` first — a state directory and a log
   with a lease record in it, and no artifact — and asserts no server was
   started.
5. `revision_seq` is the seq of the **last** event the push appended (the
   last resolution, when there are any), not of `revision.published`. The
   prose said the latter.

Suspicion adopted: **the session record's `seq` was the first poll's result
seq, not the cursor.** With a frame pending when a follow re-armed, the line
named that frame's seq; an agent that took it for its position and
acknowledged it would have skipped the frame. The `await` and `events`
results now carry `cursor` (where the call started reading), the session
line prints that, and the loop test re-arms with a chat pending and asserts
the line's seq is the acknowledged cursor and the pending frame follows.
The hand-drive's "at the acknowledged cursor" had been true only because
nothing was pending.

Prose findings adopted: the Monitor example lacked the tool's required
`timeout_ms`; `PushNotification` is "if available" in both places; a
redelivered `review.submitted` has a check-first step (round fourteen then
rewrote it); `--base-revision` after exit 7
is stated once, plainly (the re-read revision is the new base); a restart
after exit 0 is `artefacto serve`, not a push that mints a revision the
reviewer sees; "the next call exits 4" holds only when the server is gone;
`quote` is `""` in the event and `null` in the document. Test-strength
findings adopted: the open route's 401 is asserted; the two paths that had
failed by hand are driven. Left as is, and noted: the negative
`no_frame_within` assertions (the positive frame that follows each one
covers them); the reference contributes no `bash` block to the parse test
(its synopsis was checked against `--help` by hand, flag for flag);
`--json` implies `--no-open` for `open` and `render` but not `push`, which
opens the reviewer's browser on the first push by design; `ack --seq
<revision_seq>` is accepted by the server, recorded above as prose-only by
decision.

The second suspicion — that `PushNotification` might not exist as a tool —
is answered by its definition in this harness; the prose says "if
available" because another harness may lack it.

### Review round fourteen: the fix slice, reviewed fresh

A fresh reviewer on a different model read the fix slice, ran a real
five-and-a-half-minute expiry, drove every follow case for the session
line's cursor, and ran five mutations against the new tests (all caught).
Seven confirmed defects, one medium and six low, three of them regressions
from the fix slice; all closed:

1. **The new check-first step for a review skipped an `approve` with
   nothing open** (medium, regression). "Every comment the review lists as
   open is already resolved" is vacuously true of a review that lists none,
   which is the ordinary end of a review, so a literal reader never reached
   "say so and stop". The same step also skipped a review whose threads had
   been resolved in place with `resolve --changed` when the push that
   carries the change never landed (low, regression). The step now has four
   cases: no open comment, go to the verdict; all resolved and the
   artifact's revision above the review's `base_revision`, addressed and
   landed, skip; all resolved and the revision unchanged, push now;
   otherwise address what is open. The revision comparison alone was not
   enough either: a chat answered with a push between the review and its
   frame would read as "already addressed".
2. **`open` still started a daemon for a log with only lease records**
   (`serve`, one `await`, `stop`). The check is now for an artifact in the
   log, not for bytes; the test leaves exactly that log behind.
3. **The name rule lived only in the CLI**: a bearer holder speaking HTTP
   could still record `-x`. Already closed, in the commit after the
   reviewer's worktree was cut: `lease::valid_name` runs inside
   `lease::acquire`, the CLI's parser delegates to it, and the refusal maps
   to 400 `invalid_agent` (exit 2) through the lease error's own table,
   which every refusal site — `push`'s included, since round fifteen —
   reads. A server test sends the reviewer's own requests and asserts
   nothing was written.
4. **The two exit-4 rows still said push** while the new exit-0 bullet said
   `serve` (regression). Both say `serve` now.
5. `await`'s synthesised unreachable timeout carried no `cursor`, and the
   reference never named the field. Both fixed.
6. The record said the no-log test no longer ran `serve` first; it does,
   then `stop`. The sentence is fixed and the test now leaves a lease
   record in the log too (item 2).

Prose findings adopted: the session line's `seq` is `--since` when one was
passed; exit 6's stderr names the holder only when another agent has it;
`quote` is `""` in status when nothing was selected; the earlier-session
paragraph is worded in the same terms as `--base-revision`; `status --json`
is the whole review, so the skill says to read only the thread it needs.
Suspicions noted, not acted on: the token is shared between a follow and a
poll under one name, so killing the follow kills the poll side's token too
(by design; the exit-6 section covers it); cursors are never
garbage-collected (a name that claimed once keeps its entry); the reviewer's
harness had no `PushNotification` tool, which is why the prose says "if
available". Test-strength note accepted as stated: the two-questions test
proves the data the rule reads, in log order with the agent's own reply
included, not the rule, which is prose.

Round thirteen found one high defect in the skill's rule and four in the
code around it; round fourteen found one medium in the prose the first
round's fixes added, one incomplete fix in the lease (the name rule was
CLI-only), and nothing in the server's delivery or status code.

### Review round fifteen: the second fix slice, reviewed fresh, and why the loop stops

A third fresh reviewer read the round-fourteen fixes narrowly, walked the
review check-first step through eleven scenarios against the binary, and
tried to plant and to break the artifact check. Four confirmed defects,
one medium and three low, three of them regressions from the slice; all
closed:

1. **The check-first step's "resolved in place, push now" case addressed
   every thread twice** (medium, regression): "do step 4" meant pushing
   with `--resolutions`, the server appended each note a second time, and
   a review whose comments were all declined in place got a revision for
   nothing. Two things changed. The prose splits the case by status (any
   `changed`, push the revision without resolutions; all `declined`,
   nothing to push) and every case now ends at step 5, then 6, so a
   verdict is never dropped (the second case had said "acknowledge and
   skip", which lost an `approve` whose snapshot still listed open
   comments — the third defect). And **the server ignores a resolution
   that repeats a thread's current status and note**, in `push
   --resolutions` and in `resolve` alike (`resolve` answers `seq: 0` with
   `repeated: true`, the page's own "already done" shape), so "safe to run
   twice" holds for resolving whatever the prose says. A different note or
   a change of mind is still recorded. Two tests pin it both ways.
2. **`has_artifact` read the log as a string** and a torn tail ending
   inside a multibyte character made `open` say there was nothing to open
   for a repository with a review, while `serve` truncated the tail and
   served it (low, regression). It searches bytes now; a test tears the
   log inside `é`.
3. An `unanchored` thread fell through every case of the step (low,
   pre-existing): it counts as open and is resolved with a note that its
   element is gone. `resolve` accepts an unanchored thread.

Prose findings adopted: the step names its three fields; `--base-revision`
is the last push's revision only if you pushed since the review was made;
exit 6's stderr says the token is no longer valid rather than why; `open`
starts a server only when the log holds an artifact; two doc comments and
three sentences of this record that had drifted (the `open` divergence
bullet, the round-fourteen closing count, and "one table", which `push`'s
own refusal table had contradicted; it delegates now, and `push` answers a
lease error with the lease's status rather than 409 for everything).
Suspicions noted: a torn first push whose only record is a complete-looking
`revision.published` still starts a daemon that reports nothing to open; a
repeated submit against one revision is accepted by design and is what
makes a stale snapshot reachable without a crash; the synthesised
unreachable timeout's `seq` now agrees with a live empty timeout under
`--since`. Test-strength findings adopted: the server-side name test
asserts nothing was written; `status` asserts a non-empty `quote`.

**The loop stops here for plan 5.** Three rounds found 5, 7 and 4 defects,
and after the first every medium finding was in the check-first prose the
previous round's fix had added: a rule that reads the review's snapshot
against live state has a case for each way the two can disagree, and each
round found one more. What ends that is not another sentence but the
server refusing to record the same resolution twice, which is now the
case, with the prose leaning on it. What remains is the low level: a
daemon that idles for a torn first push, a snapshot semantics note, and
prose that a fourth reader would rephrase. The next real finding will come
from an agent running the skill, not from another read.

## Plan 4: the artifact index, `list`, posters, and `clean`

Spec 4.4, 5, 6.7, and 8, with cargo-dist (the rest of plan 5) at the end.
Four slices, each built, tested, mutated, and committed before the next;
then the release configuration; then the prose; then a hand-drive.

### What was built

- **The registry.** `index.json` in the state directory, format
  `artefacto.index/1`, one row per artifact keyed by the server's own id
  (`plan:<meta.id>`), so a render and a push of one plan are one row. A row
  carries kind, title, hash, the absolute source path, a static render's
  output path, the revision (0 for never pushed), `revised_at` (the last
  revision's time, which the age is computed from), `recorded_at`, the
  open and unanchored counts, `submitted`, and the last verdict; fields a
  newer artefacto wrote are carried through a rewrite. Every write takes an
  advisory lock on `index.lock`, reloads, builds its row from the one
  already there, and writes through a rename, so a `render` in a shell and
  the server recording a thread cannot lose each other's row (eight threads
  racing in a test lose nothing). Rosita's Recents rules hold: a file whose
  format number is higher than this binary's is read as empty and never
  rewritten (checked before the rows are parsed, so a newer row shape
  cannot read as corruption); rows parse one at a time, and a row this
  binary cannot read costs that row's listing and nothing else, carried
  through the next write as it was and counted in `list` and on the page
  (a readable row recorded under the same id replaces it, and `remove`
  under that id forgets it, so one id is never two rows); a file that is
  not an index at all reads as empty, is said so by `list` and the page
  rather than shown as "no artifacts yet", and is kept aside as
  `index.json.corrupt` (then `.corrupt.1`, `.corrupt.2`) when the next
  write repairs it; a row whose source file is gone is greyed and never
  pruned.
- **The poster.** `plan::poster::poster_svg(plan, state)`: a 320 by 180
  card with a kind badge, the revision or "not pushed", the title on up to
  two lines, phase and task counts, a bar per phase sized by task count
  with its done share filled (minimums give way when there are more phases
  than fit), a marker per risk by severity, and one review line. Pure,
  pinned by `tests/fixtures/plan/kitchen-sink-poster.svg` as the graph is,
  with its own `<style>` under `ap-` classes so a standalone file looks
  right and an inlined one can be restyled.
- **`render` records** a row and writes `posters/<id>.svg`, best effort:
  the page is already written, and the JSON result carries `index:
  {recorded, poster}` or `{recorded: false, reason}`, with a stderr note in
  text mode. Outside a git repository there is no index to record into and
  the render still succeeds. A render knows the plan and where its page
  went and nothing of the review, so for an artifact that has been pushed
  it keeps the row's revision, the revision's time, the counts, and the
  verdict from the row already there, and draws the poster with them; only
  a plan never pushed takes the render as its revision.
- **The server keeps rows current.** `Committer::append_all` rewrites the
  row and redraws the poster for every artifact named by a
  `revision.published`, `thread.opened`, `thread.deleted`,
  `thread.resolved`, or `review.submitted` event, after the fold and after
  `core` is released, still under the commit gate so rows land in log
  order. A reply, an answer, a mark, or a chat changes nothing the index
  shows and writes nothing. The fold keeps `verdict` and `revised_at` on
  the artifact; `status --json` prints both.
- **`artefacto list [--json]`**: the registry newest first, id, kind,
  title, revision, age, thread counts, verdict, source path with
  `(missing)`, and the static render's path; `--json` adds the absolute
  timestamps, `source_exists`, the poster path, `readonly`, `corrupt`, and
  `unreadable_rows`. Needs no server. "No artifacts yet" is said only when
  there is nothing to say about the file.
- **The index page at `/`.** Cookie-gated with a navigation origin like the
  plan page, under the same nonce policy (every `<style>` of every inlined
  poster is stamped). One row per registry entry with the poster inline,
  the age with the exact time in the `title` attribute, the review state,
  the source and whether it exists, and where the artifact is: a row this
  server holds links to `/a/<id>`, a static render names its page, a
  cleaned review says "push it again". A row this server does not hold
  offers Remove, which POSTs to `/index/remove` with the cookie and a
  strict origin, refuses a live artifact (409: its next event would write
  the row back), refuses a malformed id (400), and answers 404 for a row
  that is not there; a live row whose file is gone is greyed and says it
  is kept while its review is open here. The page answers GET and HEAD,
  405 to anything else, and has no script but the remove handler.
- **`open` lands on the index** when the server holds several artifacts
  and none is named (`"index": true`, `"artifact": null`). A bootstrap
  token now maps to a landing path, `/a/<id>` or `/`. Every served plan
  page carries an "All artifacts" link in its topbar, added by
  `served_document`; a static export has no index and gets no link.
- **`artefacto clean [--json]`.** Stops a running server over its port,
  holds the startup lock, and rewrites the log without the events of every
  artifact whose review was sent, leaving the rest at their numbers. The
  log's replay now requires strictly increasing sequence numbers rather
  than contiguous ones, so a gap is history. A final `log.cleaned` record,
  internal and never delivered, is numbered past the old high-water mark
  so a cursor acknowledged before the clean still points below every event
  appended after it; lease and cursor records name no artifact and stay.
  The secret turns over under the same port with a dead pid recorded, so
  every cookie and bearer minted so far is refused and the next `serve`
  rebinds where open pages look. The log is checked, read-only, before
  anything is changed: a log the server would refuse leaves the server up,
  the secret as it was, and the caller told why, since that is exactly the
  state `clean` cannot help with. Nothing sent means the log's records are
  unchanged (a torn tail is truncated on the way, as a restart would); no
  state directory means nothing to do and none is created. The index and
  the posters are untouched. A `push` or `serve` that arrives while `clean`
  holds the startup lock waits for the lock, not for a server `clean` will
  never start.
- **cargo-dist.** `dist init` with the GitHub host and the shell installer,
  the Windows target it proposed removed (spec 16: macOS and Linux only,
  and the daemon forks and flocks). `dist plan` lists the four archives, the
  installer script, and the source tarball for `v0.1.0`.

### Where this diverges from the spec, with the reason

- **The server writes the registry, not only `render` and `push`.** Spec
  4.4 and the risk table say the index "is written by render and push".
  Written at push time only, the open thread count and the last verdict
  would be stale until the next push, and a review sent with no push after
  it would never show its verdict, which is the row's answer to "what have
  I got open". So the server records on every commit that changes a row's
  facts, and `list` is as current as the last such commit.
- **`clean` keeps open reviews and takes sent ones out**, as spec 6.7 says,
  rather than truncating the whole log, which spec 4.2's shorter sentence
  could be read as. Two consequences: the log may have gaps in its numbers
  (spec 4.2: "it never renumbers"), and the log's highest number must
  survive the events that carried it, which is what the `log.cleaned`
  record is for. Without it, an agent that acknowledged 50 before the clean
  would find the next event numbered 41 and never see it.
- **`clean --json`**, which spec 5 does not list; every other command has
  it. Recorded in the spec's CLI surface.
- **"Yesterday" is elapsed time**, one to two days, not the calendar's
  yesterday. Spec 4.4 asks for the age to "render correctly either side of a
  day boundary", and the test pins 23 hours as "23 hours ago", 24 as
  "yesterday", 48 as "2 days ago".
- **A poster carries its own `<style>`**, so the `.svg` file the JSON names
  is readable on its own. Inlined into the index page those rules are
  document-wide, which is why the classes are prefixed and the page's own
  overrides are more specific.
- **The index page follows the system colour scheme** and has no toggle;
  the toggle is injected by `plan.js`, which the index does not load. A
  browser test does not switch schemes.
- **The index write takes no fsync.** A full fsync on macOS costs tens of
  milliseconds, the write happens under the commit gate, and the file is a
  convenience: a crash that loses the newest row loses nothing the next
  state change does not write again. The log's own fsync is untouched.
- **A greyed row that this server holds has no Remove.** Spec 4.4 says a
  missing file "greys the row and offers a per-row remove"; a live row's
  next event would write it straight back, so the row says it is kept while
  its review is open here, and the route refuses it with 409. Remove
  appears once the review is no longer on this server (`clean`, or the
  server stopped).
- **`open` with rows in the index but nothing in the log** exits 2 ("push
  a plan first"), as after a full `clean`: the index page is served by the
  server, and a daemon started only to show it would idle for half an hour.
  `list` shows the rows; a push brings the page back.

### What only running could establish

1. **Two fsyncs under the commit gate moved a tick.** With the index
   written and synced inside `append_all`, two push tests started failing
   under load: the presence tick runs from the accept loop on the next
   request's arrival once 200 ms have passed, and the second push's own
   arrival now came after that mark, so its `agent.attached` announcement
   landed on the page ahead of the push frame. Eight runs at the previous
   commit passed; two of six with the fsync failed. The fsync is gone, and
   the two tests read past announcement frames (`next_logged_frame`),
   which are asynchronous by design.
2. **macOS resolves `/var` to `/private/var`**, and the row records the
   resolved path (`render` canonicalises the source; `current_dir` is
   already resolved), so a test comparing against the temp directory's
   unresolved path fails. The tests compare resolved paths.
3. **A lone passive event is never a frame**, so the rule that
   `log.cleaned` is internal survived a test that read the backlog: nothing
   was delivered either way. The test now sends a chat after the clean and
   asserts the active frame carries the chat alone.
4. **A newer registry's rows may not parse**, and parsing the whole file
   first made such a file read as corrupt and writable. The format number
   is read before the rows are.
5. **cargo-dist proposes Windows** by default; spec 16 says no, and the
   daemon would not compile there.

### What is not covered by a test

- **The lock's necessity is probabilistic.** Eight threads recording at
  once lose nothing with the lock; the mutation without it was caught once
  and could pass on a quiet machine.
- **The dark theme on the index page** is CSS only; no browser test
  switches the scheme.
- **The remove button's failure path** ("Could not remove" after a refused
  POST) is not driven in a browser; the refusals themselves are.
- **The registry across processes** is covered by threads in the suite and
  by the reviewer's hand run, not by a cross-process test.
- **The release workflow** has never run: `dist plan` is what was checked.
- **`list`'s text layout** is asserted for its facts, not its columns.

### The hand-drive

Against the built binary in a scratch repository: a static render and two
pushes, `list` in both modes (the static row "not pushed" with its page,
the pushed rows "rev 1"), `open --json` landing on `/` with `"index":
true`, the bootstrap trading itself for the cookie, the index page listing
three rows with the two live ones marked, a review sent through the page,
`clean` reporting it removed and the other kept, `list` after it with the
cleaned row's verdict still "approve", a restart serving only the kept
review, `open --artifact` on the cleaned one refused with exit 2, and the
index page showing the cleaned row as "Not on this server; push it again"
with its Remove. Two of the drive's own steps failed first and were the
drive's fault: an empty `--session` passed to the second push (exit 6, as
it should), and a hand-counted `Content-Length` one byte long, which the
server waited on.

### Review round sixteen: plan 4, reviewed fresh

A fresh reviewer on a different model read the nine commits against the
spec in a detached worktree, ran every plan 4 suite and the browser test,
and drove the binary through push-then-render, a hand-broken registry, a
hand-broken log, a held startup lock, eight concurrent render processes
against six page commands, an in-flight `events --follow` and `await`
during `clean`, and the release config. Six confirmed defects, one high,
one medium, four low; all closed:

1. **A `render` of a live artifact rebuilt its row from nothing** (high).
   `render_entry` wrote revision 0, no threads, no verdict, and
   `revised_at` now, so after push, thread, render, `list` said "not
   pushed, 0 open" while `status` said revision 1 with one open thread; a
   sent `approve` became none; the poster said "not pushed"; and the row
   jumped to the top of the list. Only the reverse order had a test. The
   row is now built under the lock from the row already there
   (`record_with`), a render changes only what it knows, and the test
   pushes, opens a thread, backdates the row, renders, and reads every
   review fact back.
2. **One unreadable row made the whole registry read as corrupt** (medium),
   and the next write kept only its own row: every other row silently
   lost, against spec 4.4's "never auto-prune". Rows parse one at a time;
   an unreadable one is carried through verbatim and reported; a file that
   is not an index is reported by `list` and the page instead of "no
   artifacts yet", and its bytes are kept as `index.json.corrupt` when the
   next write repairs it.
3. **`clean` on a log the server refuses stopped the server, then
   failed**, rotating nothing and printing nothing on stdout for `--json`.
   The log is checked read-only first; a bad log changes nothing.
4. **A `push` arriving while `clean` held the startup lock waited ten
   seconds for a server that never came**, then blamed a `serve` that did
   not exist. `serve` now waits for the lock or a peer, whichever comes,
   and goes on as soon as the lock is free (tested with the lock held from
   the test for a second and a half).
5. **Every inlined poster carried `id="ap-title"`**, so the index page had
   duplicate ids and every card was labelled with the first title. The id
   carries the plan id.
6. **A live row with a missing file offered no Remove** with no word about
   it, against spec 4.4's sentence. The row now says why it stays, and the
   divergence is recorded above.

Prose findings adopted: "byte for byte unchanged" overclaimed (a torn tail
is truncated on the way); "reported in the server's log" was not true for
a newer-format registry (the server now says so once in `server.log`, with
a test that reads it); the spec's risk table still said the registry is
written by render and push only; the `open` row of the skill and the
divergence list now say what happens with rows but nothing live; `list`
and the page over a corrupt or newer file no longer say "no artifacts
yet" beside a note that contradicts it. Test-strength findings adopted:
the page's "rev 1" assertion matched the poster's own text and now names
the facts line; `POST /` answers 405; `clean` is driven against a corrupt
log, a held lock, and a removed multi-event commit. Noted, not acted on:
cross-process concurrency of the registry is covered by the reviewer's
hand run (eight processes) and by threads in the suite, not by a
cross-process test; a dotted format number (`/1.1`) reads as "not an
index", which the `/N` contract makes moot; an `await` retrying across a
`clean` and restart keeps the old bearer and exits 2, which is acceptable
for a deliberate rotation. The fix slice was mutated seventeen ways, all
caught once the render test backdated its row (in the same second, "now"
and "the revision's time" coincide).

### Review round seventeen: the fix slice, reviewed fresh, and why the loop stops

A second fresh reviewer read the round-sixteen fixes narrowly, drove every
one of the six against the real binary (push then render in four orders,
wrong-typed and non-object rows, a non-array `artifacts`, an empty file, a
newer file with a bad row, `clean` against a bad line, a repeated seq and
a foreign format with the server up, a push under a lock held for three
seconds and for twelve, two and four concurrent `serve`s over eight rounds,
POST and PUT on `/`, and the server log across two commits), and ran
eleven mutations of its own, all caught. Two confirmed defects, both low,
both in the new code, no regressions; both closed:

1. **`list` said "no artifacts yet" on stdout beside the stderr note that
   a row could not be read**, because its guard checked read-only and
   corrupt but not unreadable rows, while the page's guard had been
   extended. Both now say it only when there is nothing to say about the
   file.
2. **A second repair overwrote `index.json.corrupt`**, against the
   comment beside it. Each repair keeps its own copy.

Observation adopted: an unreadable row sharing an id with a recorded one
left two rows under that id, one unlisted and unremovable. A recorded row
replaces it, and `remove` under that id forgets it. Prose findings
adopted: the `list` bullet omitted `corrupt` and `unreadable_rows`; "GET
only" was loose (HEAD is answered); the lib test that listed a one-bad-row
file among its corrupt cases now has that case as its own test. Noted,
not acted on: a render of a pushed artifact whose local file has changed
records the local hash and title beside the server's review facts until
the next push, which is what a row that is one row can do; the server's
newer-index note is once per daemon life, not per episode; `clean` has a
window between stopping the server and taking the lock in which a `serve`
can start (pre-existing, unchanged by the slice); a corrupt envelope's
top-level extra fields survive only in the kept-aside copy.

**The loop stops here for plan 4.** Round sixteen found one high and one
medium defect in the registry's two directions of merge and two low ones
in `clean`'s ordering; round seventeen found only two low findings in the
code the fixes added and verified the six fixes under attack. The fixes
were mutated seventeen and then four ways, all caught. What remains is a
window in `clean` that predates the slice, a note said once per daemon,
and prose. The next real finding will come from use.

## Plan 6: the loadout dispatcher

Built in the loadout repository (`~/_git/rosita`, binary `load`), not here:
[loadout PR #57](https://github.com/elleryfamilia/loadout/pull/57), branch
`feat/artefacto-dispatcher`, thirteen commits on loadout 0.28.0, gate green
at 667 tests. Spec section 10, with these outcomes and divergences:

- `load plan check | render | push | schema` and bare `load plan` run
  `artefacto plan …` with loadout's paths; artefacto's exit codes pass
  through unchanged; `render` records the Recents row from artefacto's JSON
  and opens the browser itself; the gitignore entries, `load plan clean`,
  and `load clean` stay loadout's and accept this repository's page marker,
  whose bytes both repositories pin (`tests/fixtures/marker/first-line.txt`
  here, `tests/fixtures/artefacto-marker-first-line.txt` there).
- **Loadout had no consent-gated installer**; the spec assumed one. It was
  written from nothing: `load plan` offers this repository's cargo-dist
  installer on a terminal and prints the one-liner off one; `load doctor`
  reports the version, with a note when the major or minor is not
  `TESTED_VERSION` (0.1.0); `load update` reruns the installer, into the
  receipt's directory, when the installer put artefacto there. Not
  axoupdater: that is a self-updater, and asked about a second binary it
  compares the receipt against the running `load` and answers "current"
  offline whenever the two live apart (found by the fresh review).
- The plan skill loadout installs is `artefacto-plan`, read from
  `artefacto skill --print`, with a two-line pointer until artefacto is
  installed; `loadout-plan-preview` is retired when pristine. The spec said
  "the same id"; the manifest's name is the id, so what an agent sees is the
  skill this repository documents.
- Studio's Recents badge asks `artefacto plan check --json --lenient` once
  for every plan row, cached five seconds. A pushed review is not a Recents
  row: it lives on this repository's server and in `artefacto list` (spec
  10 said pushes are recorded; spec 12 defers artefacto artifacts in
  studio, and a Recents row must name a file studio can serve).
- `load plan status` never existed as a verb; bare `load plan` prints
  status from `artefacto plan status --json`'s answer, exit 0 either way.
  The spec's table row is corrected.
- Loadout's own plan module, fixtures, browser smoke, and shipped skill are
  deleted; `loadout.plan/1` documents are still read here.

Checked the same way as the rest: six slices mutated (18, then 8 and 7 on
the fix rounds; two survivors are guards behind other guards) and two fresh
reviews on a different model in detached worktrees. Round one found a high
(the pointer skill could never become the real one: a missing manifest file
read as a user edit), a medium-high (the updater misuse above), a medium
(retiring a skill could not see edits), and four lows. Round two, on the
fixes, found two regressions from the new on-disk hash (a `.DS_Store`
flipped a pristine install to "edited"; manifest order versus sorted order
in the two hashes) and one latent gap (the installer rerun ignored the
receipt's directory). All fixed and pinned; the loop stops there. A hand-
drive against this repository's binary found the one defect the stand-in
had hidden, artefacto's non-zero exit for a stale render read as an invalid
plan, and a harness trap: a mutation pass leaves the last mutation's build
in `target/debug`, so a hand-drive must rebuild first.

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
5. `open` gained `--no-open` and `--json`, and its no-id form is spelled out:
   one artifact needs no id; several open the index, which is plan 4.

## What is not built

- **`--passive live` is not implemented.** Delivery is digest-only, which is
  spec 16's default. Live mode's rate limit (one passive frame per 30 seconds,
  a 5 minute age cap, coalescing repeated edits to the same ref) is plan 2b's
  Task 5 and is untouched.
- **Question `options`** (spec 4.3): answers are free text, as v1 says.
- **The loadout dispatcher is a loadout PR** (#57), not merged yet.
- **A release has not been cut.** The cargo-dist configuration is committed
  and `dist plan` lists what a `v0.1.0` tag would build; no tag has been
  pushed and no installer has been run.
- **Real screenshots as posters** (spec 4.4's later opt-in) are not built;
  the poster is drawn.

## What is not covered by a test

Stated plainly, because a passing suite is not the same as a covered one.

- **The browser suite runs only where a Chromium is installed.** Everywhere
  else it prints a skip line and passes, so a CI job without a browser (or
  without `ARTEFACTO_REQUIRE_BROWSER=1`) is green without having proved the
  page.
- **The fine broadcast-ordering race has no test that reproduces it.** The
  stress test in `server_page.rs` catches a reordering skew longer than an
  fsync and nothing shorter; the rule (broadcast under the gate) is held by
  reasoning.
- **Two page rules are shadowed by another rule** and survive mutation: the
  page's use of hello's presence (the `/state` snapshot carries it too) and
  the server's `broadcast_except` (the page filters its own client ids). Both
  server halves are tested on their own.
- **Not exercised in a browser**: the "gone" notice after eight failed
  reconnects against a server that is really absent (the bounded-probe test
  reaches "gone" with HTTP alive), the resync generation counter (two
  resyncs in flight), a `/state` 404 marking the page lost, the null
  own-cursor on a failed resync (unreachable, above), and the recovery
  panel's Discard for a reply draft whose thread was deleted by another
  tab. A reply with `seq: 0` is exercised only through the reload-mid-send
  test, which reaches it by way of a repeated client id.
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

Plan 3 was checked the same way. Server slice: no nonce stamped, the meta CSP
left in (caught only once the socket client existed), the placeholder served
instead of the plan, hello without presence, the push frame without its body,
`Frame::of` attaching a body (caught only by an `events` assertion, since
`await` rebuilds its result from fields), the fragment keeping the script or
being the whole document, the served body unmarked, the state route answering
200 for an unknown artifact. Page: hello presence ignored (survived; see
above), `opened_revision` sent as the current revision, no view restore after
a swap, threads never unanchoring, drafts not restored on mount, the page's
own command never applied, the server broadcasting to the poster, catch-up
applying everything, announced events deduped by seq, no reconnect, the
stopping notice never clearing, no ping throttle. Review fixes: the page
registered after the 101, the query not stripped, any id or extra segment
served as a page, the placeholder id unescaped, the fragment without markers,
strict parse on the read path, a render failure as JSON, and broadcasts after
the gate with a 40 ms skew. Page fixes: the own reply applied straight into a
state about to be replaced, own client ids not filtered, the client id minted
per send, the composer closed by its captured node, threads on every element,
drafts not restored after a render, the chat open state never applied, no
composer on an unanchored thread, a lost cookie read as gone, and catch-up
applying buffered frames blindly. Round seven's fixes: other artifacts'
frames applied, the composer host found by descent, pending marks not shown,
a lost reply never looked up, the snapshot banner without counts, a chat
draft restored hidden, the ping clock reset by a swap, Send review not
guarded, notice code spans as text, no fetch deadline, and (survived, then
tested in round eight) a snapshot-covered reply applied again. Round eight's
fixes: a chat draft reopening the panel every render, set-valued replies
applied in arrival order, a pending mark dropped while syncing, probe cycles
unbounded, the probe never recovering, a snapshot-covered reply applied
again, and (survived, unreachable) own replies skipped on a failed resync.
Round nine's fixes: the per-key rule not applied to frames, (survived,
redundant) the buffer drained in arrival order, (survived, a no-op mutation)
the probe not marked as catching up, the error line inside the label, the
retry budget reset on every open, a 404 not rendered, no composer after a
chat send, the toggle error never cleared, and changed-to-declined not
counted. Round ten's fixes: superseded read as gone, the error line's click
not stopped, (survived, redundant) the summary handler not ignoring error
lines, the buffer drained in arrival order, a post-send chat composer
storing an empty draft, and a lost page left catching up. Round eleven's
fixes: an open panel left without a composer after a swap, (survived,
redundant) a reconnect scheduled on a live socket, superseded read as
failed, a lost page keeping its pending marks, and (survived, redundant) a
probe result applied on a live socket. Round twelve's fixes: the chat
composer's id not stable across a swap, Retry not rendering the pill,
Cancel not closing the chat, and a lost page dropping its accepted writes.

Plan 5 was checked the same way. `open`: an unknown artifact minted for,
the first of several artifacts picked without a name, the server not
started, the page route returned instead of a fresh mint, and nothing
pushed minted for anyway. `status`: `last_actor` from the first message,
unanchored always zero, open counting every thread, the follow line
ignoring the holder, a name never quoted, `present` always true, `seen`
never true, the feedback path as the source path, blocking counting
resolved threads, chat counting thread messages, cursors empty, the text
mode without the follow line, `submitted` never reported, and the revision
always 1. The skill: `--install` skipping the reference, manifest paths
without the skill directory, the wrong format string, a flag the binary
lacks and a subcommand the binary lacks written into SKILL.md, a plan field
that does not exist written into the reference's example, and
a "last actor" field from the first message (survived the status test alone,
caught by the loop test; the field was then removed in round thirteen).
Every one fails the test that names it. Round thirteen's fixes: the session
line printing the result's seq instead of the cursor, the await result's
cursor as the frame seq, `open` starting a server with no log, a log check
that says yes to any state directory or to an empty file, a leading-dash
agent name accepted by the CLI and (round fourteen) by the lease, a
thread's messages without their text, and page chat as a count again.
Round fourteen's: the artifact check says yes to a log of lease records.
Round fifteen's: the artifact check as a string search (a torn multibyte
tail), a repeated resolution appended again by `push` and by `resolve`,
and the lease check moved after the commit gate opens (the name test's
write assertion).

Plan 4 was checked the same way. The registry and `list`: a row appended
instead of put first, `remove` keeping the poster, the same version read as
newer, a foreign format accepted as ours, "yesterday" starting an hour
late, `render` never recording, the source always existing, no lock on
`record`, oldest first, the poster's bars not clamped, the done share never
drawn, `list` hiding read-only, `record` writing a newer file, and the
poster written for a refused row. The server's upkeep: `revised_at` never
set, the verdict never folded, each of the five row-changing event types
dropped from the rule in turn, a strict parse for the row, a push
forgetting the rendered path, a rewrite dropping unknown fields, the server
never writing the index, rows computed before the fold, `status` hiding
the verdict, the poster drawn without review state, and the index written
to the wrong directory; survived, redundant: writing the same row twice in
one commit. The index page: served without the cookie, to a foreign
origin, without nonce stamping, or not at all (an unknown route); remove
with a navigation origin, allowing a live artifact, skipping the id check,
or answering 200 for an absent row; a remove button on live rows, every
row linking to a page, the missing class never set, no exact time on the
age, the count text wrong, `open` with several still refusing, the index
link landing elsewhere, the served page without its link, and the remove
button doing nothing on the page (caught in the browser). `clean`: keeping
the sent review and dropping the open one, dropping lease and cursor
records, the marker numbered at the old mark, no marker, a rewrite when
nothing was sent, gaps refused again, the next seq from the count, the
secret not rotated, the server not stopped first, the old pid kept, the
port dropped, a state directory created for nothing, kept ids including
the sent ones, and `log.cleaned` delivered to agents, which survived until
the test sent an active event after the clean (item 3 above). Round
sixteen's fixes: the render ignoring the previous row, stamping a pushed
row's time with now (survived until the test backdated the row), one bad
row read as corrupt, unreadable rows dropped on save, a corrupt file not
kept aside, `list` hiding corrupt, saying "no artifacts" over a corrupt
file, or printing no notes, the page showing no notes or "no artifacts"
over a corrupt file, the index answering POST, a live greyed row saying
nothing, poster ids colliding, the server silent over a newer index or
saying it on every commit, `clean` stopping the server before checking the
log, and `serve` waiting only for a peer. Round seventeen's: `list`
saying "no artifacts" beside a note, a second repair overwriting the kept
copy, a same-id unreadable row kept beside the new one, and `remove`
leaving an unreadable row under the id.

The loop was then driven by hand against a real daemon, twice. First: push
with no server running, bootstrap a page, comment, ask, `await`, `reply`,
`resolve`, a stale push refused with exit 7, submit, and the feedback document
on disk with server-assigned ids. After the fixes: a second `await` without
`--ack` handed back the same frame and `--ack` moved the cursor; `events
--follow` printed its session line at once, that token worked for `reply`, the
lease was live with the follow's pid and gone within half a second of `kill
-9`; and an `await` whose daemon was killed mid-wait returned `timeout` with
exit 0, with the next call exiting 4.
