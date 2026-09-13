# artefacto plan page: design pass brief

Paste the "Prompt" section into the design tool and attach the four files
listed under "What to attach". Everything after the prompt is the reference
the prompt points at; attach this whole file too.

## Prompt

You are doing a design pass on the review page of artefacto, a tool where a
human reviews a plan written by an AI coding agent, in the browser, while
the agent waits on the other side and answers questions in place. The page
exists and works; the attached HTML file is the real page, the screenshots
show it in live mode with the agent attached, and plan.css holds the
current tokens. Read the attached brief first: it lists the page's parts,
the interaction model, the eight problems found in the first real review,
the constraints, and what we want out.

The goal is a cohesive system, not a restyle. Work in this order and show
your reasoning at each step:

1. Colour roles. Separate status (blocking, high risk, done, cut) from
   action (the things a reviewer clicks) from the agent's identity, with
   neutral for everything else. Light and dark from one token set.
2. The agent's mark. One distinctive glyph the reviewer recognises
   everywhere the agent appears: the ask control, the agent's replies, the
   presence pill, the "working" state.
3. The ask-and-answer pattern. Entry point on any element (the mark),
   a working state from the moment the question is sent until the answer
   lands, the answer in place, and a persistent input once a conversation
   has started on an element. Also the no-agent state.
4. The element control row (comment, ask, mark reviewed) at three
   densities: a task heading row, a prose column (the summary, a risk), and
   an acceptance-criterion row where controls only appear on hover.
5. The thread card: comment versus question; open, changed, declined,
   unanchored; blocking; "you" and "agent" with small avatars; no internal
   ids shown to the reviewer.
6. The bottom bar and the end of a review: approve and request changes as
   two distinct actions, "not done yet" communicated as autosave, the
   counts, the hint, the sent notice.
7. Notices: the agent's nudge, agent stopped, no agent attached, a new
   revision arrived.

Deliver: the token set as CSS custom properties (light and dark), a
component sheet showing every state above in both themes, and the HTML and
CSS for each component so they can be ported into plan.css and plan.js.
Keep the type pairing unless you argue for a change: Newsreader (serif)
for prose, JetBrains Mono for metadata and controls.

## What to attach

- `plan-6-replan.html`: the real page, self-contained. Open it in a
  browser; the theme toggle is top right. This is the static export, so it
  has no agent and no ask controls; it shows the content, the type, the
  layout, and the comment flow.
- `served-page-with-question-thread.png`: the live page with the agent
  attached, an open question thread with the agent's answer, the control
  row on every element, the bottom bar with the hint.
- `served-page-minimal.png`: the same on a one-task plan, for the bar and
  the banner without noise.
- `plan.css`: the current stylesheet with all tokens (`:root` and the dark
  block near the top).

## What the page is

artefacto turns a plan (a JSON document: title, summary, key points, out of
scope, phases, tasks, risks, open questions) into one self-contained HTML
page and serves it for review. The reviewer reads, comments, asks the agent
questions, answers the plan's open questions, ticks sections as reviewed,
and sends the review. An AI coding agent (Claude Code, Codex) is attached
on the other side: it answers questions in place within seconds, pushes
revised plans that swap into the page without a reload (keeping scroll,
focus, and drafts), resolves each comment as changed or declined with a
note, and nudges the reviewer with a banner when they go quiet. The same
asset also works with no server as a static export, where feedback is
copied to the clipboard instead of sent.

## The parts of the page, top to bottom

- Top bar: brand, presence pill (agent live / agent waiting / no agent),
  "All artifacts", the artifact id and revision, theme toggle.
- Orientation banner: what this page is and the three steps (skim,
  comment, send).
- Title block: author, date, title, goal.
- Stats: tasks, phases, risks, questions.
- Summary (prose) beside the phase ledger (estimate, risk per phase) and
  the blocking-questions note.
- Key points, out of scope.
- Open questions (each with Answer, Ask, and a blocking chip).
- Risks (severity chip, title, mitigation).
- Phases: numbered, collapsible; each task with status, risk, estimate,
  files, acceptance criteria, validation command, dependencies; phase and
  task dependency graphs.
- Per-element controls on every task, phase, risk, question, and the
  summary: Comment, Ask the agent, Mark reviewed.
- Threads under an element: comment threads and question threads, with
  messages from "you" and "agent", status chip, and actions (Reply / Ask
  the agent / Edit / Delete).
- Composers: the text box for a comment (with "Blocks approval"), an
  answer, a reply, a question.
- Bottom bar (fixed): a one-line hint, "live review · rev N", counts of
  threads and reviewed sections, Ask the agent (page-level), Approve
  checkbox, Send review.
- Page-level chat panel above the bar.
- Recovery panel at the top for threads and drafts whose element vanished
  in a revision.
- Notices: the agent's nudge, the "sent" confirmation, agent stopped.

## Interaction model today

- A comment waits until the reviewer sends the review.
- A question ("Ask the agent") reaches the agent now; the answer appears in
  the same thread.
- An answer to an open question and a "reviewed" tick are data the agent
  reads with the sent review.
- "Send review" sends everything, with a verdict: approve if the Approve
  box is ticked, otherwise comment; request changes when a blocking
  comment exists.
- The agent answers in threads, pushes revisions, marks each comment
  changed or declined with a note, nudges, and stops.
- States that matter: agent live / waiting / none; review unsent / sent /
  approved; revision N; thread open / changed / declined / unanchored;
  blocking; a draft in progress; a revision arriving while typing.

## What the first real review found (the problems to solve)

1. Asking the agent was not obvious or universal. It is now a labelled
   button beside every Comment button, but it should be a recognisable
   call to action, probably the agent's mark, with the label on hover.
2. Nothing shows that the agent is working between the question and the
   answer.
3. Asking is two clicks (Ask, then Send). Once a conversation has started
   on a section it should behave like a chat with a persistent input.
4. The agent has no identity on the page: "you" and "agent" are plain
   labels. A distinctive mark and small avatars are wanted.
5. Thread ids like "c-3" are shown; they exist for the agent's commands
   and mean nothing to the reviewer.
6. Colour roles collide: the accent marks attention (blocking, high risk)
   and also actions (ask); green is used for Send review. It reads as
   confused.
7. There is no answer to "I do not want to send yet". Everything is
   autosaved on the server the moment it happens, and the agent is told
   when the reviewer leaves, but the page never says so. Approve is a
   checkbox next to a Send button rather than two distinct actions.
8. The reviewer expected a comment to reach the agent as it was written.
   Today comments are batched until send; questions are live. Whether
   comments should also flow live (with "send" meaning "I am done, act on
   all of it") is an open product question; design for both if you can.

## Constraints

- One self-contained HTML file. Fonts are embedded (`@font-face` in
  plan.css); no runtime requests to third parties.
- Content Security Policy with a nonce: no inline `style` attributes on
  elements, no third-party scripts. Everything is classes in plan.css.
- Light and dark from one token set (`:root` and a `prefers-color-scheme`
  block, plus a manual toggle).
- The same asset serves live mode and static export; static has no agent,
  so no ask controls and a clipboard flow instead of Send.
- Keyboard reachable controls with visible focus; `prefers-reduced-motion`
  respected for any working animation; a print stylesheet exists.
- Type: Newsreader (serif) for prose, JetBrains Mono for metadata and
  controls. Keep unless there is a reason not to.
- The page is long; controls on acceptance rows appear only on hover to
  keep rows to one line.
