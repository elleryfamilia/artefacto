# artefacto — design spec

Date: 2026-09-06 (revision 3, 2026-09-08)
Status: draft for review
Origin: extracted from loadout's plan visualizer (`load plan`, rosita repo)

Revision history: r1 as brainstormed. r2 after a critic pass and a cross-model
pass. r3 folds a second round of both: a critic that read every claim against
the rosita source, and a codex pass told what round one had already fixed. The
two agreed on cursors, the lease, the passive rate bound, and the browser
tests, and disagreed on nothing.

## 1. Objective

Turn loadout's static plan visualizer into a standalone tool called **artefacto**
that renders **interactive artifacts** between a human and a coding agent.

The agent emits structured data. artefacto renders it as a page in the browser.
The human comments, answers questions, and approves. Every interaction reaches
the agent as data. The agent stays quiet unless the human talks to it, the
review is submitted, or the human goes idle.

artefacto is agent-agnostic. Every agent gets the same information through the
same CLI. Agents that can watch a long-running command get it as it happens.
Agents that can only run a command to completion get it by polling. Nothing in
the loop depends on one model or one harness.

The first artifact kind is the development plan that exists today. The design
leaves room for more kinds on the same spine, but builds none of them.

## 2. Context

What exists today in rosita (the loadout repo). Line references are current as
of 2026-09-08.

- `src/plan/{model,render,svg,icons}.rs` plus `src/plan/assets/{plan.css,plan.js}`
  render a `loadout.plan/1` JSON document into one self-contained HTML file.
- `src/commands/plan.rs` is the CLI: `load plan`, `check`, `render`, `status`,
  `clean`, `schema`.
- The page supports element-anchored comments, "mark reviewed", and one clipboard
  action that copies a `loadout.plan-feedback/1` document for paste-back. That
  document has a verdict of `comment` or `request_changes` only, positional
  comment ids, and no answers, replies, or reviewed marks.
- The page stores draft comments and reviewed marks in `localStorage` under keys
  that embed the plan hash (`plan.js:775-777`, `plan.js:814-816`), and discards
  any stored value whose fingerprint no longer matches (`plan.js:784`,
  `plan.js:823`).
- `plan.js` is one 1270-line IIFE. Only the pure core is exported, as
  `window.loadoutPlan` (`plan.js:53`); `init()` is private and runs once on
  `DOMContentLoaded` (`plan.js:1258-1270`). The script is embedded inline by
  `render.rs:924`.
- `skills/loadout-plan-preview/` teaches the loop to agents.
- The studio server (`src/studio/server.rs`) has a reviewed loopback security
  model: bind 127.0.0.1, Host-header allowlist, one-time bootstrap token,
  HttpOnly cookie, Origin check, no CORS.
- Studio's Recents tab lists rendered plans and re-parses plan.json per row to
  show a fresh or stale badge.

Couplings the extraction has to cut or keep, each with a home in section 10:

| rosita symbol | used for |
|---|---|
| `markdown::render_markdown` | markdown fields in the page |
| `hash::context_hash`, `hash::short` | plan fingerprint |
| `render::header::GENERATED_MARKER` | the **prefix** `<!-- loadout:generated` (`render/header.rs:9`). artefacto does not adopt this string; it emits its own (section 11.1) and rosita learns to accept both in plan 6 |
| `render::header::extract_context_hash` | `load plan status` compares the hash in the HTML to the plan's hash; it must learn artefacto's prefix |
| `writer::AtomicWriter`, `ensure_line` | atomic writes and gitignore entries |
| `config::generated_dir`, `workflow::artifacts_dir` | loadout's paths for plan.html and plan.json |
| `Prepared`, `Runtime` | loadout's repo and invocation context |
| `skills::by_id("loadout-plan-preview")` | `load plan schema` prints the skill's reference |
| `recents::RecentsStore` | Recents registry |
| `studio::server::open_browser` | opening the page |

Reverse dependencies inside rosita, things that call into the plan module or
its files. Each needs a home in section 10 or the extraction breaks them.

| caller | what it does |
|---|---|
| `commands/clean.rs:66` | `load clean` removes plan.html and plan-feedback.json |
| `commands/plan.rs:279-289` (`status`) | reads `context=` out of plan.html and compares it to the plan hash |
| `studio/server.rs:1348-1358`, `:1402-1416` (`plan_badge`) | parses plan.json per Recents row, leniently, and compares its hash |
| `commands/run.rs:567` | one description string for the plan-preview skill |
| `skills.rs` | `include_str!` of the skill files that move |
| `studio/views.rs`, `studio/server.rs` (Skills tab) | installs and removes the skill by id |
| `tests/skill_examples.rs:133-155` | validates every JSON example in the skill reference against the real deserializer |
| `README.md`, `docs/concepts.md`, `docs/extending.md` | link the skill files directly |

## 3. Concept: interactive artifacts

An artifact kind is a JSON schema plus a renderer plus a section in the skill.
Everything else is shared:

- stable element ids that survive revisions
- comment threads anchored to elements
- structured answers to questions the agent asked
- a verdict (approve or request changes)
- live revisions with "changed" badges
- presence and idle detection
- chat between reviewer and agent
- one event protocol, delivered by one CLI in two modes

Every kind uses the same seven interactions: comment, answer, pick, tick, set
status, verdict, chat. That is the test for whether a new kind belongs here.

Planned kinds, in the order they map onto a development workflow. Only **plan**
is in scope for v1. Nothing in the v1 code, schema, or protocol is built for
the others. The only generic parts are the `artifact` and `ref` fields every
event already carries.

| kind | stage | what the human does | verdict |
|---|---|---|---|
| options | brainstorm | picks one of two or three approaches, annotates | pick |
| spec | brainstorm | approves a design section by section | approve per section |
| plan | plan | comments on phases and tasks, answers open questions | approve / request changes |
| board | implement | watches tasks flip status live; pauses, skips, reorders | control |
| recap | verify | reviews an annotated diff and file map | approve / request changes |
| checklist | ship | performs steps and ticks them; the agent proceeds on ticks | complete |
| triage | any | marks each finding fix, defer, or ignore | disposition |
| form | any | fills in values the agent needs | submit |

## 4. Architecture

Three pieces.

```
┌──────────────┐   HTTP, bearer token (push/reply/await/events)   ┌──────────────────┐   ws + cookie   ┌──────────┐
│ coding agent │ ───────────────────────────────────────────────▶ │ artefacto server │ ◀─────────────▶ │ the page │
│ (any model)  │ ◀── NDJSON on stdout (events --follow / await) ─ │ 127.0.0.1:PORT   │                 │ browser  │
└──────────────┘                                                  └──────────────────┘                 └──────────┘
```

The agent never opens a socket. Every agent-side operation is a CLI command
that talks to the server over loopback HTTP with a bearer token read from a
file only the user can read. The page is the only WebSocket client.

### 4.1 The binary

One Rust binary, `artefacto`. Extracted from rosita's plan module. It contains:

- the `artefacto.plan/1` model and validator (the schema is loadout's, unchanged
  apart from its name; see section 11.1)
- the deterministic renderer and SVG graph (unchanged)
- a loopback HTTP + WebSocket server
- the event log and delivery cursors
- the CLI
- the embedded skill text

Static export stays. `artefacto plan render` still writes one self-contained
HTML file that works with no server and keeps the clipboard paste-back flow. That
is the zero-server fallback and stays tested.

### 4.2 The server

One server per repository, anchored to the git root of the invoking directory.

**Binding and identity.** Binds 127.0.0.1. The port is chosen at random on the
first `serve` for a repository and recorded. Every later start re-binds the same
port, so an open page can reconnect after a crash or restart. If the port is
taken, the server picks a new one, records it, and open pages show "server
restarted on a new port, run push again" once their retries run out. The session
secret is created on the first `serve` and kept across restarts, so the page's
cookie stays valid. `artefacto clean` rotates it.

**Daemonizing** means: fork, `setsid`, fork again, close inherited descriptors,
redirect stdout and stderr to `server.log` in the state directory.
`--foreground` skips all of that. If the bind fails, the message tells the user
to run `artefacto serve` in their own terminal, because some agent sandboxes
forbid listening sockets. Every CLI command checks that the pid in `server.json`
is alive before trusting the file. A dead pid means "no server".

**One event log per server**, not per artifact, with one monotonic `seq` across
every artifact. That makes a single `--since SEQ` meaningful no matter how many
artifacts a repository has. Each event names its artifact. `clean` truncates the
log; it never renumbers.

The server is the **only writer** of the log. CLI commands never touch it; they
call the server. All state is a fold over the log: revisions, threads, answers,
reviewed marks, delivery cursors, and the lease. `server.json` holds only what
must exist before the log can be read: pid, port, secret, and started-at.

For that to be true, a `revision.published` event carries the **whole validated
plan** and its hash, not just a change summary. A summary alone cannot rebuild
the current body after a restart, which would make the log an incomplete source
of truth. Plans are large, so a revision event stores the plan once and later
events reference it by hash.

**The agent lease.** One agent acts at a time, and the lease is what makes a
poll-mode agent hold together across many short commands.

- `events --follow` or `await` takes a lease and gets back a **session token**
  carrying a generation number. The token, not the process, is the agent's
  identity, so a lease survives an `await` returning every 90 seconds instead
  of dropping and re-taking on every cycle.
- **Every agent mutation carries the token**: `push`, `reply`, `resolve`, and
  `ack`. A token from a superseded generation is refused. Without this a stale
  agent that lost the lease could still write.
- The lease has a TTL of five minutes. Any agent call refreshes it. A
  `--follow` disconnect releases it immediately. An expired lease, or one whose
  recorded pid is dead, is released by the server.
- A second agent is refused with exit 6 unless it passes `--takeover`, which
  increments the generation atomically and invalidates the old token. The
  refusal names the current holder and its age, so the user is never left
  guessing.
- Presence is derived from the lease, so it does not flicker between poll
  cycles: the pill changes only when the lease changes hands or expires.

**Self-exit** when no page and no agent have been connected for 30 minutes.
`artefacto stop` exits it now.

### 4.3 The page

This is the largest piece of v1 work. Today's `plan.js` is a 1270-line IIFE that
mounts once and keeps its own state in `localStorage`. Server mode needs a page
that can be re-mounted and that treats the server as the only store. Section 12
budgets it as a rewrite of that asset, not as "page changes".

Three structural changes to the existing asset:

1. **A re-entrant mount.** `init()` becomes an exported `mount(root)` that can
   run again against a swapped-in body. Today it is private and runs once
   (`plan.js:1258-1270`), and a script inserted through `innerHTML` does not
   execute, so an in-place swap cannot re-initialize anything without this.
2. **One store, chosen at mount.** In server mode the server's event log is the
   only store for comments, replies, answers, and reviewed marks; the
   hash-keyed `localStorage` paths are not used at all. They would silently
   discard everything on the first push, because a new revision changes the
   plan hash and both loaders drop values whose fingerprint no longer matches
   (`plan.js:784`, `plan.js:823`). In static export they stay exactly as they
   are today.
3. **Both modes in one asset.** The serverless clipboard flow keeps working
   from the same file, selected at mount by whether the page was served.

Behaviour added in server mode:

- a presence pill: `agent live` (an `events --follow` holds the lease),
  `agent waiting` (an `await` holds it), or `no agent`
- threads: reply, edit, delete, and an **ask the agent** button that turns a
  reply into a chat event
- a page-level chat composer for questions about the plan as a whole; when
  the pill says `no agent`, the composer says the message will wait
- open questions rendered as inputs, so answers arrive as data. In v1 an answer
  is free text, because `artefacto.plan/1` questions have no options field
  (`model.rs:134-139`). An optional `options` list is an additive later change.
- **Send review** with an **Approve** toggle replaces the clipboard button; the
  clipboard button stays in static export and gains the same toggle
- a revision banner when the agent pushes: what changed, with the previous title
  on hover, and each thread marked addressed or declined with the agent's note

**Applying a pushed revision.** The server sends **one snapshot** holding the
rendered body, thread state, and resolutions together, and the page applies it
in a single pass. Sending those separately would let the reviewer see a body
from one revision beside threads from another. The page swaps the body, calls
`mount(root)` again, then restores, in order: disclosure state by element id
(phases already carry `id="phase-<id>"`, `render.rs:855`), focus and caret
position, and the scroll position **anchored to an element** rather than to a
pixel offset. Geometry changes between revisions, so a raw offset does not put
the reviewer back where they were. No full reload.

**Anything a reviewer is typing survives.** Every editable control, comment
composer, question input, and edit box gets a **composer id** minted when it
opens. Its draft is stored in `sessionStorage` under that id together with the
ref it targets and the revision it was opened against. On restore, the event it
eventually sends carries the revision it was **opened against**, not the one
showing when send is pressed, so text written against revision 3 does not arrive
labelled revision 4. Two composers on the same ref stay separate because the id,
not the ref, is the key.

**Page writes are idempotent.** Each outgoing interaction carries a
client-generated identifier, and the server ignores one it has already
committed. A disconnect around a submit otherwise forces a choice between losing
the review and duplicating every comment in it when the page retries.

A thread whose ref no longer exists after a push gets status `unanchored`. So
does a draft whose target ref is gone. Both are listed in a recovery panel at
the top, so nothing a reviewer wrote is ever silently dropped.

The page reconnects with backoff and shows a clear "server gone" state after a
bounded number of retries, never a silent one. `artefacto open` mints a fresh
one-time bootstrap URL at any time, so losing the cookie is never a lockout.

### 4.4 The artifact index

Every artifact artefacto has ever produced for this repository is listed on one
page, served at `/` by the server and printed by `artefacto list`. It answers
"what have I got open, and how stale is it" at a glance.

Each row carries:

- **A poster**: a small picture of the artifact, described below.
- **Title and kind**, and the artifact id.
- **How long ago**, as relative text ("2 hours ago", "yesterday"), computed from
  the last revision. The exact timestamp is in the title attribute and in the
  JSON.
- **Review state**: the current revision number, open thread count, unanchored
  thread count, and the last verdict if there was one.
- **Where it came from**: the plan file's path, and whether that file still
  exists.

**The poster is drawn, not screenshotted.** A real screenshot needs a browser at
generation time. Making Chrome a hard dependency of `push` would be a bad trade
for a thumbnail, and it would fail on exactly the headless boxes loadout already
supports. Instead artefacto draws a deterministic SVG card from the plan model
it already has: title, kind badge, a bar per phase sized by task count, risk
markers, and the review state. artefacto already generates a deterministic SVG
dependency graph, so this reuses machinery that exists. The poster is
recognisable, costs nothing, needs no browser, and can be asserted in a golden
test the same way the graph is.

A real rendered screenshot stays possible later, as an opt-in for machines that
have a browser, writing to the same slot the poster occupies. It is not v1.

**The index survives everything.** It is a small per-repository registry, not a
view over the event log, because the log gets truncated by `clean` and because
static exports are made with no server running. `render` and `push` both record
into it. It follows the conventions rosita already settled on for its Recents
registry: refuse to write a file written by a newer version, self-heal a corrupt
one, and **never auto-prune**. A missing artifact file greys the row and offers a
per-row remove; it is never deleted on the user's behalf, because an absent file
usually means an unmounted volume rather than a dead artifact.

### 4.5 The skill

The skill is a **package, not a file**: today's `loadout-plan-preview` is a
`SKILL.md` that points at a `reference.md`, and both are installed together. So
the binary exposes two forms. `artefacto skill --print` emits a JSON manifest of
relative paths and their contents, which is what loadout's lifecycle consumes.
`artefacto skill --install DIR` writes the package into a staging directory for
anything that would rather copy files. A single flat text stream could not
preserve the package, and the pointer from one file to the other would break.

**One skill per artifact kind.** The plan kind ships `artefacto-plan`; a later
spec kind ships `artefacto-spec`, and so on. Each is small and describes one
schema, which is what makes it useful to an agent deciding whether it applies.

**The skill is written to be invoked by the model, not typed by a person.** Its
description says when it applies, so an agent that has just written a plan
reaches for it on its own. A person typing its name is the secondary path, and
in that case the content to turn into an artifact already exists, so the skill
starts from what is there rather than asking the user to produce it.

The content has one generic section and one Claude Code section. loadout's skill
lifecycle installs it into every agent directory exactly as it does today.

## 5. CLI surface

```
artefacto serve  [--port N] [--idle 15m] [--away 5m] [--passive digest|live] [--no-open] [--foreground]
artefacto stop
artefacto status [--json]
artefacto list   [--json]            # every artifact for this repo, newest first
artefacto open   [--artifact ID]     # mint a fresh bootstrap URL and open the browser;
                                     # with no id, opens the artifact index

artefacto plan check  <file>... [--json] [--lenient]   # prints plan_hash, title, counts, per file
artefacto plan render <file> [--out PATH] [--no-open] [--json]
artefacto plan status <file> [--json]                  # is the rendered HTML fresh for this plan
artefacto plan push   <file> [--session TOKEN] [--agent NAME] [--takeover]
                             (--base-revision N | --force)
                             [--resolutions FILE] [--json]
artefacto plan schema

artefacto await  [--timeout 90s] [--ack SEQ] [--since SEQ] [--artifact ID] [--agent NAME] [--takeover]
artefacto events [--ack SEQ] [--since SEQ] [--follow] [--agent NAME] [--takeover]
artefacto ack    --seq N --session TOKEN
artefacto reply  --session TOKEN (--thread ID | --artifact ID) [--nudge] (<text> | --stdin)
artefacto resolve <thread-id> --session TOKEN (--changed | --declined) [--note TEXT]

artefacto skill (--print | --install DIR)
artefacto clean
```

Behaviour that matters:

- `check`, `render`, `status`, and `push` all take `--json`. Every JSON result
  carries `plan_hash`, `title`, and phase and task counts; push adds the
  artifact id, revision, and page URL. The loadout dispatcher records Recents
  from that output. `check` accepts **many files in one call** and has a
  `--lenient` mode, because studio's Recents tab needs both (section 10).
- `push` validates, renders, starts the server if none is running, opens the
  browser on the first push only. `--resolutions` takes a JSON file of
  `{thread, status, note}` entries so one push can address many threads
  without one CLI call each. **`--base-revision` is passed by the caller**,
  taken from the revision the agent last saw, and the server compares and
  appends atomically. Reading the current revision at push time instead would
  make the check vacuous, because it would always match. The first push for an
  artifact needs neither flag. A later push must pass one or the other; if the
  server is ahead, it is refused with exit 7 and the agent re-reads with
  `status --json`.

  **`--session` is optional**, because push is usually the first command an
  agent runs and there is no token to present yet. Without one, push takes the
  lease under `--agent` exactly as `await` does, and returns the token in its
  result. With one, it refreshes the lease it already holds. A push under
  another agent's name is refused with exit 6, naming the holder.

  The change summary section 6.3 requires is **derived** by comparing the
  previous revision's plan with the new one, not typed by the agent: there is
  no flag for it here, and a summary nobody has to write is one that is always
  present and always true.
- `await` long-polls the server and returns when something the agent should
  act on happens. It always exits 0 when the server answered and prints one
  JSON object carrying `status`, `seq`, and `events` (the same frame shape as
  `events`), because agents treat non-zero exits as failures rather than
  "poll again":

  | `status` | meaning |
  |---|---|
  | `submitted` | the feedback document (section 6.6) and its path |
  | `chat` | a chat event, with every undelivered passive event before it |
  | `idle`, `away` | the timer event, same prepending |
  | `back` | a page reconnected after `away`; section 6.2 lists it as active |
  | `timeout` | nothing actionable; the frame holds whatever passive events accumulated |
  | `stopped` | the server is shutting down; same partial frame |

  Non-zero only for real errors: 4 no server, 6 lease held by another agent or
  a superseded session token.

  The default timeout is **90 seconds**. An earlier draft said five minutes and
  claimed it sat under every harness's command limit; that was not verified,
  and shell timeouts of a couple of minutes are common, so a killed `await`
  would look like a failed tool call. 90 seconds is safely inside every limit
  we know of, and the skill re-arms it. Agents whose limit is known to be
  higher can raise it.

  `await` **reconnects on its own**: if the connection drops or the server
  restarts mid-wait, it retries against the same cursor until its absolute
  deadline, then returns `timeout`. Without that, the restart-mid-wait test in
  section 14 could not pass.

  When several active events are waiting, `await` returns at the **earliest**
  one, and the frame stops there. The agent handles events in the order they
  happened rather than seeing a later one first.
- **Delivery is resumable and at-least-once.** The server keeps an `acked_seq`
  per lease name in the log. `events` and `await` default `--since` to that
  cursor, so an agent that restarts with no memory of where it was resumes
  exactly where it left off. A result's `seq` is the highest event in it.
  **The agent acknowledges; the server never guesses.** A call that passes
  `--ack SEQ` — the `seq` of the previous result — acknowledges everything up
  to it, once the agent has acted; `artefacto ack --seq N` does the same on
  its own, and is what a monitor-mode agent uses. Nothing else moves the
  cursor: a call that acknowledges nothing is handed the same frame again. An
  earlier draft had the next call acknowledge the previous frame by itself,
  which is at-most-once — an agent that receives a frame and restarts before
  acting loses it — and section 16 forbids exactly that. An agent that dies
  between receiving a frame and acting on it therefore sees that frame again
  rather than losing it, so every handler must be safe to run twice: replying
  to the same chat event twice is the failure mode this trades for, and the
  skill tells the agent to check the thread before replying. Acknowledging is
  idempotent: an `--ack` at or behind the cursor is a no-op, never an error.
- `events` prints frames as NDJSON. Its first line is an `artefacto.session/1`
  record carrying the session token and the cursor, because NDJSON has no
  envelope to put them in and a monitor-mode agent needs the token to `reply`.
  Without `--follow` it prints the backlog and exits. With it, it stays
  attached, holds the lease, flushes every line, and exits when the server
  stops. It acknowledges nothing on its own and advances only its own read
  position; the agent runs `ack --seq N` after acting on a frame, so a follow
  that restarts replays what was printed but never acknowledged.
- `reply` needs either a thread or an artifact. When the server has exactly one
  artifact, `--artifact` may be omitted for page-level chat. `--nudge` posts
  the `nudge` banner of section 6.3 instead of a message; it reaches open pages
  and is never written to the log, because it records nothing about the review.
- `status --json` prints port, artifacts, revisions, open and unanchored
  threads, the last event sequence, each lease's `acked_seq`, the lease holder
  and its age, reviewer presence, and the exact `events --follow` command line
  for the skill to arm.
- `list` prints one row per artifact: id, kind, title, revision, relative age,
  open and unanchored thread counts, last verdict, source path, and whether
  that path still exists. `--json` adds absolute timestamps and the poster path.
  It works with no server running, because it reads the registry, not the log.
- **Where the session token comes from.** `await` and `events` return it in
  their result as `session`, alongside `seq`. An agent takes it from the first
  call and passes it to every mutation until a call hands back a new one.
  `status --json` never prints it, so a token cannot be picked up by something
  that only reads status.

## 6. Event protocol

### 6.1 Envelope

Format `artefacto.event/1`. Every event is one JSON object:

```json
{ "format": "artefacto.event/1", "seq": 42, "ts": "2026-09-06T16:02:11Z",
  "artifact": "plan:auth-refactor", "revision": 3, "actor": "reviewer",
  "type": "thread.replied",
  "data": { "thread": "c-3", "ref": "task:t-session-store", "text": "..." } }
```

`seq` is **server-wide**, monotonic, and continues from the last logged value
after a restart. It is the resume cursor. `actor` is `reviewer`, `agent`, or
`server`; agent events also carry the lease name in `data.agent`.

What the agent receives is a **frame**: one NDJSON line holding an ordered array
of events, `{"format":"artefacto.frame/1","seq":<seq of last event>,
"events":[...]}`. A frame has one or more events. The last event in a frame is
the one that caused it to be sent. A frame's own `seq` is the acknowledgement
point.

### 6.2 Reviewer to agent

Passive:

- `thread.opened`, `thread.replied`, `thread.edited`, `thread.deleted`
- `question.answered` with `{question, text}`
- `element.reviewed` (mark reviewed on a task or phase, with `on: true|false`)

Active:

- `chat.sent` — from the composer or an "ask the agent" reply; carries the
  thread id when it has one
- `review.submitted` — carries the full feedback document and `base_revision`
- `reviewer.idle` — page open, no activity for `--idle`; fires once per quiet
  period and re-arms after activity. Activity is measured from a throttled
  page ping (at most one per 30 seconds) on scroll, keys, pointer, and
  visibility, not from the last comment, so a reader who reads for twenty
  minutes is not idle.
- `reviewer.away` — every page socket closed for `--away` with the review
  unsubmitted; fires once
- `reviewer.back` — a page reconnected after `reviewer.away`
- `server.stopping`

### 6.3 Agent to reviewer

- `revision.published` with a change summary
- `thread.replied` with `actor: "agent"`
- `thread.resolved` with `status: changed | declined` and a note
- `agent.attached`, `agent.detached` (presence, with mode `live` or `waiting`)
- `nudge` (renders as a banner)

### 6.4 Passive delivery

Passive events reach the agent in every case. What differs is whether they
wake it.

- `--passive digest` (default): a passive event does not by itself cause a
  frame. When an active event fires, the frame carries every event after the
  lease's `acked_seq` up to and including the active one. `events --since` uses
  the same rule. There is **no separate passive buffer**: the frame's contents
  are always computed from the cursor, so a passive event cannot be delivered
  once by a poll and again by the next wake-up.
- `--passive live`: passive events also flush on their own. The bound is a
  **rate limit, not a count**: at most one passive frame per 30 seconds, and a
  frame is sent when the quiet gap or the 5 minute age cap is reached. An
  earlier draft also flushed at 100 buffered events, which bounds nothing: a
  storm of a thousand edits a minute would produce ten frames a minute, not
  the two the same paragraph claimed. Buffer size is capped separately by
  coalescing repeated edits to the same ref, which keeps memory bounded without
  touching the frame rate.

Digest is the default because the agent does nothing with a passive frame by
design (section 7), so waking it costs a model turn and buys nothing the next
active frame does not already carry. Live mode exists for anyone who wants the
agent's terminal to show the review as it happens.

Frames sent to the page are never coalesced.

### 6.5 Transports

| mode | who | mechanism |
|---|---|---|
| `artefacto events --follow` | agents that can watch a long-running command (Claude Code's Monitor tool with a command source) | one NDJSON frame per line on stdout |
| `artefacto await` | agents that run a command to completion | long-poll, one JSON result per call, re-armed by the skill |
| `artefacto events --since SEQ` | any agent, after a restart | backlog, then exit |

Chat is live in both modes: `await` returns on `chat.sent`, so an agent that
can only poll still answers a question within one poll cycle. The page's
presence pill tells the reviewer which mode the agent is in.

An agent-role WebSocket is deferred. It would save one process on the Claude
Code side and cost a second auth path with the token in the transcript.

### 6.6 The feedback document

`review.submitted` carries `artefacto.feedback/1`. Its shape is a superset of
loadout's old `loadout.plan-feedback/1`, so a reader that knows the old fields
finds all of them under the new name:

- `verdict` gains `approve` alongside `comment` and `request_changes`
- `base_revision`: the revision the review was made against
- `comments[]` keep their v1 fields; ids are server-assigned and stable
  (`c-<n>` per artifact, never renumbered); each gains `status`
  (`open | changed | declined | unanchored`) and `replies[]`
- `answers[]`: `{question, text}`
- `reviewed[]`: refs marked reviewed

Static export writes the same document. The rename is safe for an older loadout:
`warn_stale_feedback` parses the file as untyped JSON and reads only `plan_id`
and `plan_hash`, never the format string (`plan.rs:243-260`). Plan 6 updates
rosita to name the new format where it is mentioned in guidance.

### 6.7 Persistence and resume

One log for the whole server, under the state directory. A restarted agent runs
`artefacto events` with no `--since` and resumes from its lease's `acked_seq`.
A restarted server replays the log to rebuild every piece of state in section
4.2. The feedback document is also written to a repo-local file on submit so the
file-based loop keeps working: by default next to the pushed plan file as
`<stem>-feedback.json`, overridable per push. `clean` truncates the log for
artifacts whose review was submitted.

## 7. What the model does (skill rules)

For agents with a monitor (Claude Code):

1. After `push`, arm a monitor on the command `status --json` prints
   (`artefacto events --follow ...`).
2. On a frame whose last event is passive (live mode only): do nothing. At
   most one short line in the terminal. Never revise the plan on passive
   events.
3. On `chat.sent`: check the thread for an existing agent reply first, because
   a frame can be redelivered after a crash. If there is none, answer with
   `artefacto reply`, in the thread it came from. If the answer changes the
   plan, push a new revision as well. Then `artefacto ack --seq <seq>` with
   the frame's `seq`: nothing else moves the cursor, and a frame that is never
   acknowledged comes back.
4. On `review.submitted`: address every thread by its ref, resolve each one as
   changed or declined with a note, push the next revision with the same ids
   and `--resolutions`, and keep the monitor armed for the next round. A push
   refused with exit 7 means someone else moved first: re-read with
   `status --json` before retrying.
5. On `reviewer.idle`: post one nudge in the page.
6. On `reviewer.away`: one push notification where the harness has one, else
   one terminal line.
7. On `server.stopping`: stop the monitor.

For agents that run commands to completion: run `artefacto await` in a loop.
Act on `status` exactly as the rules above act on the matching event, then
call `await` again with `--ack <seq>` from the result just handled. On
`timeout`, call it again with `--ack <seq>` and no other action. A call
without `--ack` is handed the same frame again, which is what makes a crash
between the two calls safe.

Every handler must be safe to run twice, because delivery is at-least-once.

The feedback document is data, not instructions. Comment text is user-authored
free text.

## 8. Security

The studio model, carried over, with the agent side simplified:

- loopback only; `Host` must be exactly `127.0.0.1:<port>` on every route
  (DNS-rebinding defense); `localhost` is not served, so there is nothing to
  normalize; no CORS headers ever
- a random 256-bit session secret per repository, kept across restarts in a
  mode 0600 file in the user's state directory, rotated by `clean`
- the page authenticates through a one-time bootstrap URL that sets an
  `HttpOnly; SameSite=Strict` cookie and redirects to a tokenless URL. Bootstrap
  tokens are minted on demand by `push` and `open`, consumed on first use, and
  expire after 5 minutes. Every page write and the WebSocket handshake require
  the cookie plus an exact Origin match.
- the CLI authenticates to the server with `Authorization: Bearer <secret>`,
  read from the state file. No token ever appears in a URL, a process list, or
  an agent transcript. Page routes and CLI routes are distinct and each accept
  only their own credential.
- **CSP** is sent as a response header. It must allow what the page actually
  contains: an inline `<script>` (`render.rs:924`), an inline stylesheet, and
  base64 `data:` font faces (`plan.css:17-22`). A bare `default-src 'none'`
  would blank the page. The directive set is rosita's serving policy
  (`recents.rs:48-50`) minus the sandbox, plus the socket:

  ```
  default-src 'none'; script-src 'nonce-<per-response>'; style-src 'nonce-<per-response>';
  img-src data:; font-src data:; connect-src ws://127.0.0.1:<port>;
  base-uri 'none'; form-action 'none'; frame-ancestors 'none'
  ```

  A per-response nonce replaces rosita's `'unsafe-inline'`, which artefacto can
  do because it renders the page at request time. **No `sandbox` directive**:
  sandboxing makes the origin opaque, which would break the cookie the page
  authenticates with. Static export keeps today's meta CSP unchanged.
- markdown from the plan is rendered through the existing sanitizer; comment,
  reply, chat, and answer text are rendered as text, never as markup

## 9. State on disk

```
~/.local/state/artefacto/<repo-hash>/
  server.json                       # pid, port, secret, started_at   (0600)
  server.log                        # daemon stdout and stderr
  events.ndjson                     # one log per server; live state folds from it
  index.json                        # the artifact index; survives clean and needs no server
  posters/<artifact-id>.svg         # one drawn poster per artifact
<repo>/<plan-stem>-feedback.json    # written on submit; path overridable per push
```

`<repo-hash>` is a hash of the git root path.

## 10. loadout integration

- `load plan <args>` becomes a dispatcher that executes `artefacto plan <args>`
  with loadout's paths (`.loadout/workflow/artifacts/plan.json` and the
  matching feedback path under the same directory). The loadout-specific
  gitignore entries stay in the dispatcher.
- If `artefacto` is not on PATH, `load plan` offers to install it through the
  consent-gated installer loadout already has. `load update` updates it.
  `load doctor` reports its version or absence.
- Recents: the dispatcher records renders and pushes from artefacto's `--json`
  output. artefacto never touches loadout's state files.

Every reverse dependency in section 2 gets a home:

| dependency | what happens |
|---|---|
| `load clean` | keeps removing marker-gated plan.html and the feedback file; needs only the marker prefix, not the plan module |
| `load plan status` | stays, as `artefacto plan status`; the dispatcher forwards it |
| studio Recents badge | **one** batched `artefacto plan check --json --lenient` call for all rows, not one subprocess per row; result cached with a short TTL; no badge at all when artefacto is absent |
| `run.rs` description string | stays; it is one string |
| skill files | removed from rosita; the lifecycle consumes the **manifest** from `artefacto skill --print` (a package of paths and contents, not one text stream) and installs a two-line pointer skill when the binary is missing |
| studio Skills tab | keeps installing and removing by the same id; the source of the text changes, not the id |
| `tests/skill_examples.rs` | moves to artefacto with the skill; rosita keeps no copy to validate |
| README and docs links | repointed to the artefacto repo in the dispatcher PR |

**The index and loadout's Recents tab do different jobs, so neither replaces the
other.** Recents is machine-wide and cross-repo: "what did I render lately,
anywhere". The artefacto index is per-repo and deeper: every artifact for this
repository, with its poster, age, revision, and review state. The dispatcher
keeps recording into Recents exactly as before. Later, studio can show artefacto
posters in Recents by reading `artefacto list --json`, but that is not v1 and
nothing in v1 depends on it.

**Why the badge needs batching.** `plan_badge` runs per Recents row inside a
synchronous HTTP handler, and the registry holds up to 30 entries
(`recents.rs:38`). One subprocess per row would put up to 30 process spawns in
one page render, where today it is an in-process parse. It also parses
leniently (`server.rs:1408`), which is why `check` needs `--lenient`.

**Compatibility contract, and which side carries it.** artefacto emits
`<!-- artefacto:generated context=<hash> -->` and knows nothing about loadout.
Rosita is the integrator, so rosita adapts: plan 6 widens every place it gates
on its own marker to accept artefacto's too. Those places are `load clean`
(`commands/plan.rs:135`), studio's serve and clean gate
(`studio/server.rs:1395-1397`), and `extract_context_hash`
(`render/header.rs:82`), which `load plan status` uses.

Two independent test suites are not enough on their own, because both can be
edited to agree with a change that breaks the other side. Each repo commits a
**frozen fixture** of the line: artefacto's pins the line it produces, rosita's
pins the line it must keep accepting, and they hold the same bytes. The
dispatcher also records the artefacto version it was tested against, so a
mismatched binary is reported rather than silently producing wrong badges.

The loadout workflow's plan stage template names `artefacto` directly, so the
flow survives the skill not surfacing.

This is a one-line dispatch table, not a plugin system. If a second external
tool appears, the `loadout-<name>` naming rule turns it into one.

A thin Claude Code marketplace plugin (manifest, the skill, a SessionStart hook
that checks the binary and reports a pending review) is published from this repo
for people who do not use loadout. It is v1.1, not v1.

## 11. Names, and extraction from rosita

### 11.1 artefacto's name is the only one in its output

artefacto is a separate project. Most people who use it will not have loadout
installed and will never hear of it. So **nothing artefacto produces carries
loadout's name**: not the generated first line, not a format string, not a
default path, not a message. The word "loadout" appears in this repository only
where an integration is being described, as in section 10.

What artefacto writes:

| thing | value |
|---|---|
| generated first line | `<!-- artefacto:generated context=<hash> -->` |
| plan document format | `artefacto.plan/1` |
| feedback document format | `artefacto.feedback/1` |
| event and frame formats | `artefacto.event/1`, `artefacto.frame/1` |

What artefacto reads: the plan parser also accepts `loadout.plan/1` as a
**deprecated alias**, because plan documents in that format already exist and
breaking them would be gratuitous. Reading one prints a single deprecation line
naming the new value. artefacto never writes the old string, and the alias is
not documented in the skill reference: new plans use `artefacto.plan/1`.

**The compatibility burden moves to loadout, which is the right way round.**
loadout is the integrator here; artefacto does not know it exists. Plan 6
teaches rosita to accept `<!-- artefacto:generated` alongside its own marker
wherever it gates on one today, and to read `artefacto.feedback/1`. Rosita is
the same owner's project, so the change is cheap there and free for everyone
else.

This replaces the earlier frozen-fixture contract, which pinned loadout's line
in artefacto's tests. The fixture stays, but it now freezes **artefacto's** line,
and rosita gets its own fixture for the line it must keep accepting.

### 11.2 What moves

Moves to artefacto: `src/plan/*` and its assets, the plan logic of
`src/commands/plan.rs`, `skills/loadout-plan-preview/`, `tests/fixtures/plan/`,
`tests/skill_examples.rs` (the plan half), and `tools/build-plan-fonts.py`.

**The headless-Chromium browser smoke moves with the page, not with the
renderer.** It drives the page's `#selftest` harness through identifiers the
extraction renames, and it is built on the `file://` plus sandboxed-iframe
harness section 14 already records as unusable for a served page. Since the page
is rewritten for a re-entrant mount and a server-side store, porting the old
smoke first would mean writing it twice.

Copied, not shared: `markdown::render_markdown` and the hash helpers. Both are
small; publishing a shared crate is not worth it for two items. The marker
constant is **not** copied: artefacto defines its own (section 11.1). Rosita's
plan module is deleted in the dispatcher PR, so drift cannot start.

Stays in rosita: `AtomicWriter`, `ensure_line`, the path helpers, `Prepared`,
`Runtime`, the recents store, and `open_browser` (artefacto gets its own
copy of the browser-opening logic).

Format strings are artefacto's own from day one (section 11.1), with
`loadout.plan/1` accepted on read as a deprecated alias so existing plan
documents keep working.

## 12. Scope

v1 delivers:

1. the `artefacto` binary with the CLI in section 5, plan kind only
2. the server: log, cursors, lease, revisions, WebSocket, security
3. **the page, rewritten** for a re-entrant mount and a server-side store,
   keeping the serverless clipboard path working in the same asset (section
   4.3). This is the largest single item, not a set of small edits.
4. the artifact index: registry, drawn posters, the served page, and `list`
5. the skill with both sections
6. cargo-dist releases for macOS and Linux
7. the rosita dispatcher PR: `load plan` → `artefacto plan`, install offer,
   update, doctor, recents, clean, status, batched badge, skill pointer, docs
   links

Deferred, in likely order: real browser screenshots as an opt-in alternative to
drawn posters; the Claude Code marketplace plugin (v1.1); studio showing
artefacto posters in Recents; the agent-role WebSocket; question `options`;
the **spec** and **checklist** kinds;
the **board** kind; an MCP mode exposing the same verbs as tools; Windows
builds; mockup review; anything remote or team-facing; studio listing live
artifacts.

## 13. Risks

| risk | mitigation |
|---|---|
| the page rewrite is the whole schedule | it is item 3 in scope, sized as a rewrite; the static export path is covered by the existing golden fixtures, so regressions there are caught by tests that already exist |
| an agent sandbox breaks the server in one of three different ways | they are separate failures and get separate spikes before the server is built: (1) the bind is refused, (2) the bind succeeds but the daemon is killed with the agent's process group when the tool call ends, (3) the daemon survives but a sandboxed agent cannot connect to a loopback port the user owns. Bind failure prints the exact `serve` command to run by hand. If (3) turns out to be real on any target harness, the fallback is a file-based transport: the agent reads the log and writes commands into a directory the server watches. |
| the Monitor tool auto-stops a stream that produces too many frames | digest mode sends nothing passive; live mode is rate-limited to one frame per 30 seconds and tested under a storm |
| an agent crashes mid-frame and loses events | delivery is at-least-once against a persisted per-lease cursor; every handler is written to be safe to run twice |
| a push lands while the reviewer is mid-comment | composer drafts survive the swap; a composer sends the revision it was opened against; orphaned threads become `unanchored` and are listed, never dropped |
| a stale agent session pushes over a newer revision | `base_revision` check, exit 7, `--force` to override |
| a crashed agent holds the lease forever | lease TTL, pid liveness, `--takeover` with the holder named in the refusal |
| a stale agent that lost the lease keeps writing | every mutation carries the session token; a superseded generation is refused |
| the log cannot rebuild the page after a restart | revision events carry the full validated plan, not just a summary |
| a disconnect around submit loses or duplicates a review | page writes carry a client-generated id and the server ignores duplicates |
| daemon left behind | self-exit after 30 idle minutes, `status` shows it, `stop` kills it, pid liveness checked before trusting `server.json` |
| idle nudges annoy a careful reviewer | activity measured from page pings, fire once per quiet period, defaults of 15 minutes idle and 5 minutes away, both flags |
| server dies mid-review | port and secret persist, the page reconnects, the log replays; `open` mints a new URL if the cookie is gone |
| studio's Recents page gets slow | one batched `check` call per render plus a short-TTL cache, never one subprocess per row |
| renderer drift between artefacto and rosita | rosita's plan module is deleted in the dispatcher PR |
| the browser test for the served page cannot be written | budgeted explicitly in section 14 as new harness work, with a fallback that needs no new harness |
| a thumbnail feature drags in a browser dependency | posters are drawn deterministically from the plan model, reusing the existing SVG machinery; no browser is involved at any point |
| the index lists artifacts whose files are gone | the row greys and offers a per-row remove; nothing is auto-pruned, because an absent file is usually an unmounted volume |
| the index and the event log disagree | the registry is the index's only source and is written by render and push; the log drives live state only |

## 14. Testing

- The existing model, validator, renderer, and golden-fixture tests move with
  the code and must stay green before any server work starts.
- Server tests run over a real TCP connection, as studio's do: Host rejection,
  bootstrap, cookie, Origin rejection, bearer rejection on page routes and
  cookie rejection on CLI routes, lease refusal, takeover, TTL expiry, dead-pid
  release, port re-bind after restart, and a mutation carrying a superseded
  session token.
- Cursor and delivery tests: `--since` defaults to the lease cursor; a frame
  redelivered after a simulated crash contains exactly the unacked events; a
  passive event delivered by `events --since` is not delivered again by the
  next active frame; `seq` continues across a restart; with two active events
  pending, `await` returns at the earlier one. Crash points are tested both
  before and after the event commits, because those lose different things.
- A log-sufficiency test: kill the server, start it again with no other state,
  and the rendered body and every thread come back identical.
- Index tests: `render` and `push` both record; `list` works with no server
  running; a truncating `clean` leaves the index intact; a newer-version file is
  refused rather than overwritten; a corrupt file self-heals; a missing source
  file greys the row and is never auto-removed; relative ages render correctly
  either side of a day boundary.
- A golden test for the drawn poster, the same shape as the existing dependency
  graph golden, so poster drift is caught like renderer drift.
- Digest and live-mode tests use an injected clock: prepend order, quiet gap,
  count cap, age cap, the two-frames-a-minute bound.
- `await` tests: returns on submit, chat, idle, away, timeout, and stop with
  the right `status` and `seq`; the long poll reconnects and survives a server
  restart mid-wait; it returns before any plausible harness command timeout.
- An end-to-end CLI test: `serve` → `push` → a fake page client comments,
  answers, and submits → `await` returns the document → `reply` reaches the
  fake page → `push --resolutions` marks threads → a push with a stale
  `base_revision` is refused.
- A frozen marker fixture asserting the emitted line in full, prefix and
  `context=` value. artefacto pins the line it writes; rosita pins the same
  bytes as a line it must keep accepting (plan 6).
- A format-alias test: a document declaring `loadout.plan/1` still parses and
  prints one deprecation line; every document artefacto writes declares
  `artefacto.plan/1`.
- A `--passive live` rate test asserting at most one frame per 30 seconds under
  a synthetic storm, since the earlier count-based bound was arithmetically
  impossible.
- **The browser test for the served page needs a new harness, and this is real
  work.** Rosita's two browser tests both run over `file://` with `--dump-dom`,
  and its comments record why: real-socket serving plus a virtual time budget
  makes Chrome dump an empty page in CI (`browser_smoke.rs:100-107`). The
  "served over TCP" test that exists is a raw-socket header assertion with no
  browser in it (`tests/studio.rs:441`). So there is no precedent to copy.
  Two options, decided in the implementation plan:
  1. a CDP-driven harness (the Chromium-over-DevTools scripts used for
     screenshots are the closest starting point), or
  2. keep `--dump-dom` and have the page's own `#selftest` perform the cookie
     bootstrap and one WebSocket round trip, writing the existing result
     marker. No new harness, less coverage.

  Whichever is chosen, the browser suite must cover the races a fake client
  cannot reach, because they are where the in-place swap actually breaks: a
  revision arriving while the reviewer is typing, a draft whose ref was
  removed, focus and caret recovery, two tabs on one artifact, and duplicate
  suppression after a reconnect. In-place revision delivery is not v1-complete
  until those pass.
- Every geometry assertion in the page smoke is relative, never a pixel count.

## 15. Rollback

Rosita keeps its in-binary plan code until the dispatcher PR merges. If artefacto
has a blocking problem after that, users pin the previous loadout release, which
still contains the renderer. artefacto's static export means a broken server
never blocks a review; the file-based loop is always available.

## 16. Decisions taken by default

Stated so they can be overridden rather than discovered:

- passive delivery is `digest`, not `live`. This is the one place the spec
  departs from the owner's literal request ("every interaction sent to the
  model"). Every interaction is still delivered; it just does not wake the
  agent until something actionable happens. Live mode is one flag away.
- idle and away nudges stay in v1 at the owner's request, both flags, both
  can be set to `off`
- one event log per server, one server-wide `seq`
- delivery is at-least-once against a persisted per-lease cursor; handlers must
  be safe to run twice. The alternative, at-most-once, silently drops a review
  when an agent crashes at the wrong moment.
- one agent lease at a time, five minute TTL, identified by a session token
  with a generation; every agent mutation carries it; `--takeover` bumps the
  generation
- `await` exits 0 with a `status` field for every non-error outcome; default
  timeout 90 seconds, chosen to sit under common shell limits rather than from
  a verified per-harness number
- `--base-revision` is supplied by the caller on every push after the first,
  never read from the server at push time
- revision events carry the whole validated plan, so the log alone can rebuild
  the page
- port and secret persist per repository across restarts; `clean` rotates
- no agent-role WebSocket in v1; the CLI is the only agent transport
- daemon by default, `--foreground` opt-in; self-exit after 30 idle minutes
- every string artefacto writes is in its own namespace: the generated line,
  `artefacto.plan/1`, `artefacto.feedback/1`. `loadout.plan/1` is accepted on
  read as a deprecated alias and never written. Rosita, as the integrator,
  carries the compatibility work
- one skill per artifact kind, written for the model to invoke
- server mode does not use `localStorage` for review state; static export keeps
  using it exactly as today
- index posters are **drawn from the plan model**, not screenshotted, so no
  browser is ever required; real screenshots stay a later opt-in
- the index is a per-repo registry file, never a view over the event log, so it
  survives `clean` and covers artifacts made with no server running
- marketplace plugin is v1.1
- macOS and Linux only in v1

## 17. First implementation step

`cargo init`, move the plan module, assets, fixtures, and tests from rosita,
and get `artefacto plan check`, `render`, and `status` green against the moved
test suite, with the `--json` outputs, the multi-file and `--lenient` modes,
and the `context=` value contract test added. This is mechanical and de-risks
everything after it.
