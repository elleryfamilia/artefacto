# Server build status

Current as of 2026-09-11, branch `feat/server-spine`.

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

407 tests, `cargo fmt --all --check` and `cargo clippy --all-targets -D warnings`
clean. Forty-nine of the tests run the served page in a headless Chromium;
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

The forty-nine tests cover the loop end to end and the races spec 14 names:
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
redundant with the per-key rule for set-valued writes and no test asserts
message order across a drain; and the "probe not marked as catching up"
mutation was a no-op, because the probe is now the resync that marks it —
the test's wait for that state is the rule.

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
- **`artefacto open`** is still not implemented, so a page that says "run
  `artefacto open` for a fresh link" is pointing at a command that does not
  exist yet. A second `push` is the only way to mint one.
- **Question `options`** (spec 4.3): answers are free text, as v1 says.

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
counted.

The loop was then driven by hand against a real daemon, twice. First: push
with no server running, bootstrap a page, comment, ask, `await`, `reply`,
`resolve`, a stale push refused with exit 7, submit, and the feedback document
on disk with server-assigned ids. After the fixes: a second `await` without
`--ack` handed back the same frame and `--ack` moved the cursor; `events
--follow` printed its session line at once, that token worked for `reply`, the
lease was live with the follow's pid and gone within half a second of `kill
-9`; and an `await` whose daemon was killed mid-wait returned `timeout` with
exit 0, with the next call exiting 4.
