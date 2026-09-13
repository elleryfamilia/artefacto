/* artefacto plan viewer — progressive enhancement over server-rendered HTML.
   Pure core first (no DOM), then the DOM layer, then the served-page
   session, then the #selftest harness.

   Two modes share this one file, chosen at mount by whether the page was
   served: a body carrying `data-artefacto-artifact` came from the server
   and treats its event log as the only store; a body without it is a
   static export and keeps drafts in localStorage with the clipboard flow. */
(function () {
  "use strict";

  /* ---- pure core ---------------------------------------------------- */

  /* Events the server announces to open pages without writing them down:
     presence, nudges, and the stop. They carry `seq = last_seq` -- the seq
     of whatever was logged last -- because they have no seq of their own.
     So a page must never drop a frame by seq alone; see `applyFrame`. */
  const ANNOUNCED = {
    "agent.attached": true, "agent.detached": true, "nudge": true, "server.stopping": true,
  };

  const core = {
    parseIsland(text) { return JSON.parse(text); },
    /* `blocking` replaces the old 4-way `type` taxonomy: a comment either
       blocks approval or it doesn't -- the free-form `text` carries whatever
       nuance a category label used to gesture at. Defaults to false so
       existing non-blocking callers don't need to pass it. */
    makeComment(ref, quote, text, blocking) {
      return { ref: ref, quote: quote || null, text: text, blocking: !!blocking };
    },
    /* The static export's document. `approve` is the toggle spec 4.3 gives
       the clipboard button: an explicit approval, else the old rule -- any
       blocking comment makes it request_changes, otherwise comment. */
    buildFeedback(plan, fingerprint, comments, approve) {
      const doc = {
        format: "artefacto.feedback/1",
        plan_id: plan.meta.id,
        plan_hash: fingerprint,
        verdict: approve ? "approve"
          : (comments.some(c => c.blocking) ? "request_changes" : "comment"),
        comments: comments.map((c, i) => ({
          id: "c-" + (i + 1),
          ref: c.ref, quote: c.quote, text: c.text, blocking: !!c.blocking,
        })),
      };
      const lines = ["## Plan feedback — " + plan.meta.id, ""];
      if (approve) lines.push("**Approved.**", "");
      for (const c of doc.comments) {
        lines.push("### " + c.ref + (c.blocking ? " — BLOCKS APPROVAL" : ""));
        /* Blockquote every line of free-form comment text so a "```" line
           in it reads as "> ```" -- that can't open a top-level fence, and
           any fence it does open stays contained inside the blockquote. */
        for (const textLine of c.text.split("\n")) lines.push("> " + textLine);
        if (c.quote) {
          /* Collapse whitespace (incl. newlines) to single spaces so the
             quote is safe to embed inline -- a mid-line ``` can't open a
             fence. */
          lines.push('_(re: "' + c.quote.replace(/\s+/g, " ") + '")_');
        }
        lines.push("");
      }
      const json = JSON.stringify(doc, null, 2);
      const markdown = lines.join("\n");
      /* Human-readable mirror first, canonical JSON after: the person
         pasting reads the top; the agent needs the fenced block (stable
         refs, plan_hash, blocking flags) and is told not to lose it. */
      const combined = markdown
        + "\n---\n\n"
        + "Machine-readable block — paste everything, leave this intact:\n\n"
        + "```json\n" + json + "\n```\n";
      return { json: json, markdown: markdown, combined: combined };
    },

    /* Every element a thread may anchor to, computed from the RAW plan a
       revision event carries -- the same set the server's fold uses to
       decide which threads lost their element, so the page and the server
       agree about what is unanchored without the server having to say. */
    planRefs(plan) {
      const refs = new Set();
      if (!plan || typeof plan !== "object") return refs;
      if (plan.meta && plan.meta.id) refs.add("meta:" + plan.meta.id);
      for (const q of plan.open_questions || []) if (q && q.id) refs.add("question:" + q.id);
      for (const r of plan.risks || []) if (r && r.id) refs.add("risk:" + r.id);
      for (const ph of plan.phases || []) {
        if (!ph) continue;
        if (ph.id) refs.add("phase:" + ph.id);
        for (const t of ph.tasks || []) if (t && t.id) refs.add("task:" + t.id);
      }
      return refs;
    },
    /* An open thread whose element is gone becomes unanchored; an
       unanchored one whose element came back is open again. Resolved
       threads are left alone: the agent already answered them. */
    reanchor(threads, refs) {
      for (const t of threads) {
        const anchored = refs.has(t.target);
        if (!anchored && t.status === "open") t.status = "unanchored";
        else if (anchored && t.status === "unanchored") t.status = "open";
      }
    },

    emptyState(artifact) {
      return {
        artifact: artifact, revision: 0, planHash: "", plan: null, threads: [],
        answers: {}, reviewed: [], chat: [], submitted: false,
        verdict: null, presence: null, lastSeq: 0,
      };
    },
    /* What `GET /a/<artifact>/state` returns, as page state. */
    fromSnapshot(s) {
      return {
        artifact: s.artifact, revision: s.revision || 0, planHash: s.plan_hash || "",
        plan: s.plan || null,
        threads: (s.threads || []).map(function (t) {
          return {
            id: t.id, target: t.target, quote: t.quote || "", blocking: !!t.blocking,
            asked: !!t.asked, status: t.status || "open",
            messages: (t.messages || []).map(function (m) {
              return { actor: m.actor, text: m.text, ts: m.ts, note: !!m.note };
            }),
          };
        }),
        answers: Object.assign({}, s.answers || {}),
        reviewed: (s.reviewed || []).slice(),
        chat: (s.chat || []).slice(),
        submitted: !!s.submitted,
        verdict: s.verdict || null,
        presence: s.presence || null,
        lastSeq: s.last_seq || 0,
      };
    },

    /* One event into the state: the server's fold, in this language. Returns
       whether anything changed. Every case is safe to run twice, because a
       page can be handed an event it already has (its own command's result,
       then the same event in a snapshot). */
    applyEvent(state, e) {
      const d = e.data || {};
      const find = function (id) { return state.threads.find(function (t) { return t.id === id; }); };
      const message = function () { return { actor: e.actor || "reviewer", text: d.text || "", ts: e.ts || "" }; };
      switch (e.type) {
        case "revision.published": {
          state.revision = e.revision > 0 ? e.revision : state.revision + 1;
          if (d.plan) state.plan = d.plan;
          if (d.plan_hash) state.planHash = d.plan_hash;
          /* A new revision reopens the review. */
          state.submitted = false;
          state.verdict = null;
          core.reanchor(state.threads, core.planRefs(state.plan));
          return true;
        }
        case "thread.opened": {
          if (!d.thread || find(d.thread)) return false;
          state.threads.push({
            id: d.thread, target: d.ref || "", quote: d.quote || "", blocking: !!d.blocking,
            asked: false, status: "open", messages: [message()],
          });
          return true;
        }
        case "thread.replied": {
          const t = find(d.thread);
          if (!t) return false;
          t.messages.push(message());
          return true;
        }
        case "thread.edited": {
          const t = find(d.thread);
          if (!t || !t.messages.length) return false;
          t.messages[0].text = d.text || "";
          return true;
        }
        case "thread.deleted": {
          const before = state.threads.length;
          state.threads = state.threads.filter(function (t) { return t.id !== d.thread; });
          return state.threads.length !== before;
        }
        case "thread.resolved": {
          const t = find(d.thread);
          if (!t || (d.status !== "changed" && d.status !== "declined")) return false;
          t.status = d.status;
          if (d.note) t.messages.push({ actor: "agent", text: d.note, ts: e.ts || "", note: true });
          return true;
        }
        case "question.answered": {
          if (!d.question) return false;
          state.answers[d.question] = d.text || "";
          return true;
        }
        case "element.reviewed": {
          const on = d.on !== false;
          const i = state.reviewed.indexOf(d.ref);
          if (on && i < 0) state.reviewed.push(d.ref);
          else if (!on && i >= 0) state.reviewed.splice(i, 1);
          else return false;
          return true;
        }
        case "chat.sent": {
          if (d.thread) {
            const t = find(d.thread);
            if (t) {
              t.messages.push(message());
            } else if (d.ref) {
              /* A question asked on an element that had no thread opens
                 one, and the question is its opening message. */
              state.threads.push({
                id: d.thread, target: d.ref, quote: d.quote || "", blocking: false,
                asked: true, status: "open", messages: [message()],
              });
            } else {
              return false;
            }
          } else {
            state.chat.push(message());
          }
          return true;
        }
        case "review.submitted":
          state.submitted = true;
          state.verdict = d.verdict || null;
          return true;
        case "agent.attached":
          state.presence = { agent: d.agent || "", mode: d.mode || "waiting" };
          return true;
        case "agent.detached":
          state.presence = null;
          return true;
        default:
          return false;
      }
    },

    /* A frame into the state, in order. `cursor`, when given, is the
       `last_seq` of the snapshot the state was just rebuilt from: a LOGGED
       event at or below it is already inside that snapshot and is skipped.
       An announced event never is. Its seq is borrowed from the log's last
       record, so by seq alone it would look old, and it lives in no
       snapshot -- dropping it is how a page stops hearing that the agent
       left. Returns the events that were applied. */
    applyFrame(state, frame, cursor) {
      const applied = [];
      for (const e of frame.events || []) {
        if (cursor != null && !ANNOUNCED[e.type] && e.seq <= cursor) continue;
        core.applyEvent(state, e);
        applied.push(e);
      }
      if (frame.seq > state.lastSeq) state.lastSeq = frame.seq;
      return applied;
    },
    isAnnounced(type) { return !!ANNOUNCED[type]; },

    /* The event a command becomes once the server accepts it. A page that
       posted a command is skipped by the broadcast -- it would otherwise
       count its own change twice -- so it builds the event from the reply,
       with the id the server assigned, and applies it through the same
       fold as everything else. */
    localEvent(cmd, reply, ts) {
      const base = {
        format: "artefacto.event/1", seq: reply.seq || 0, ts: ts, artifact: "",
        revision: cmd.opened_revision || cmd.base_revision || 0, actor: "reviewer",
      };
      switch (cmd.cmd) {
        case "thread.open":
          return Object.assign(base, { type: "thread.opened", data: {
            thread: reply.assigned, ref: cmd.ref, text: cmd.text,
            blocking: !!cmd.blocking, quote: cmd.quote || "",
          } });
        case "thread.reply":
          return Object.assign(base, { type: "thread.replied", data: { thread: cmd.thread, text: cmd.text } });
        case "thread.edit":
          return Object.assign(base, { type: "thread.edited", data: { thread: cmd.thread, text: cmd.text } });
        case "thread.delete":
          return Object.assign(base, { type: "thread.deleted", data: { thread: cmd.thread } });
        case "question.answer":
          return Object.assign(base, { type: "question.answered", data: { question: cmd.question, text: cmd.text } });
        case "element.reviewed":
          return Object.assign(base, { type: "element.reviewed", data: { ref: cmd.ref, on: !!cmd.on } });
        case "chat.send":
          return Object.assign(base, { type: "chat.sent", data: {
            text: cmd.text, thread: cmd.thread || reply.assigned || null, ref: cmd.ref || null, quote: cmd.quote || "",
          } });
        case "review.submit":
          return Object.assign(base, { type: "review.submitted", data: { verdict: cmd.verdict } });
        default:
          return null;
      }
    },

    threadsOn(state, ref) {
      return state.threads.filter(function (t) { return t.target === ref && t.status !== "unanchored"; });
    },
    unanchored(state) {
      return state.threads.filter(function (t) { return t.status === "unanchored"; });
    },
  };
  window.artefactoPlan = core;

  function selftest() {
    const results = [];
    const pending = [];
    function check(name, fn) {
      try { fn(); results.push("PASS " + name); }
      catch (e) { results.push("FAIL " + name + ": " + e.message); }
    }
    function checkAsync(name, promise) {
      pending.push(Promise.resolve(promise).then(
        function (ok) { results.push((ok ? "PASS " : "FAIL ") + name); },
        function (e) {
          results.push("FAIL " + name + ": " + (e && e.message ? e.message : String(e)));
        }
      ));
    }
    /* Build the real page first. The harness used to run INSTEAD of mount(),
       which kept it honest about the pure core but blind to everything the
       page actually mounts -- the theme toggle, the comment editors, the
       reviewed toggles. Running mount() here means a throw during mount is a
       reported failure rather than a silent one, and lets the checks below
       measure the live DOM. Everything after this point may assume it ran. */
    check("page initialises", function () { mount(document.body); });
    check("plan refs match the server's fold", function () {
      const refs = core.planRefs({
        meta: { id: "p" },
        open_questions: [{ id: "q1" }], risks: [{ id: "r1" }],
        phases: [{ id: "ph", tasks: [{ id: "t1" }, { id: "t2" }] }],
      });
      ["meta:p", "question:q1", "risk:r1", "phase:ph", "task:t1", "task:t2"].forEach(function (r) {
        if (!refs.has(r)) throw new Error("missing " + r);
      });
      if (refs.size !== 6) throw new Error("extra refs: " + refs.size);
    });
    check("a thread loses and regains its anchor", function () {
      const threads = [
        { id: "c-1", target: "task:t1", status: "open" },
        { id: "c-2", target: "task:gone", status: "open" },
        { id: "c-3", target: "task:t1", status: "changed" },
        { id: "c-4", target: "task:t1", status: "unanchored" },
      ];
      core.reanchor(threads, new Set(["task:t1"]));
      if (threads[0].status !== "open") throw new Error("anchored stays open");
      if (threads[1].status !== "unanchored") throw new Error("a gone element unanchors");
      if (threads[2].status !== "changed") throw new Error("resolved threads are left alone");
      if (threads[3].status !== "open") throw new Error("an element that came back re-anchors");
    });
    check("catching up skips logged events but never announced ones", function () {
      const state = core.emptyState("plan:x");
      state.threads.push({ id: "c-1", target: "task:t1", quote: "", blocking: false, status: "open", messages: [] });
      const frame = { format: "artefacto.frame/1", seq: 9, events: [
        { type: "thread.replied", seq: 5, actor: "reviewer", data: { thread: "c-1", text: "old" } },
        { type: "agent.attached", seq: 9, actor: "agent", data: { agent: "claude", mode: "live" } },
        { type: "thread.replied", seq: 10, actor: "agent", data: { thread: "c-1", text: "new" } },
      ] };
      const applied = core.applyFrame(state, frame, 9);
      if (applied.length !== 2) throw new Error("applied " + applied.length);
      if (state.threads[0].messages.length !== 1 || state.threads[0].messages[0].text !== "new") {
        throw new Error("the logged event at or below the cursor was not skipped, or the newer one was");
      }
      if (!state.presence || state.presence.mode !== "live") throw new Error("the announced event was dropped");
      if (core.applyFrame(state, frame, null).length !== 3) throw new Error("with no cursor everything applies");
    });
    check("a command's reply becomes the same event the server folds", function () {
      const state = core.emptyState("plan:x");
      const ev = core.localEvent(
        { cmd: "thread.open", ref: "task:t1", text: "why", blocking: true, opened_revision: 2 },
        { ok: true, assigned: "c-7", seq: 12 }, "2026-01-01T00:00:00Z");
      core.applyEvent(state, ev);
      if (state.threads[0].id !== "c-7") throw new Error("the id is the server's");
      if (state.threads[0].messages[0].actor !== "reviewer") throw new Error("actor");
      if (!core.applyEvent(state, { type: "thread.resolved", data: { thread: "c-7", status: "declined", note: "no" } })) throw new Error("resolve");
      if (state.threads[0].status !== "declined" || state.threads[0].messages[1].actor !== "agent") throw new Error("note becomes an agent message");
      if (core.applyEvent(state, ev)) throw new Error("opening the same thread twice is a no-op");
    });
    check("island parses", function () {
      const plan = core.parseIsland(document.getElementById("plan-data").textContent);
      if (!plan.meta || !plan.meta.id) throw new Error("no meta.id");
    });
    check("feedback round-trips", function () {
      const plan = core.parseIsland(document.getElementById("plan-data").textContent);
      const fp = document.body.getAttribute("data-plan-fingerprint");
      const fb = core.buildFeedback(plan, fp,
        [core.makeComment("task:t-session-store", "q", "needs work", true)]);
      const parsed = JSON.parse(fb.json);
      if (parsed.verdict !== "request_changes") throw new Error("verdict");
      if (parsed.comments[0].blocking !== true) throw new Error("blocking");
      if (parsed.plan_hash !== fp) throw new Error("hash");
      if (fb.combined.indexOf("## Plan feedback") !== 0) throw new Error("combined starts with mirror");
      if (fb.combined.indexOf("```json") === -1) throw new Error("combined carries the JSON block");
    });
    check("refs exist in dom", function () {
      if (!document.querySelector('[data-plan-ref="task:t-session-store"]'))
        throw new Error("missing data-plan-ref");
    });
    check("storage guarded", function () {
      /* Under an opaque origin localStorage ACCESS throws; the guards must
         swallow that and hand back an empty array, not break the page. */
      const drafts = loadDrafts("selftest-plan", "selftest-fp");
      if (!Array.isArray(drafts)) throw new Error("loadDrafts must return an array");
    });
    check("comment editor fills its box", function () {
      /* Regression: .comment-box was a block container, so the textarea sat
         at its intrinsic cols="20" (~205px) however wide the box was; and
         inside an acceptance <li> (a two-column grid) the box was placed in
         the 1.75rem counter column, ~28px wide. Both looked like styling
         nobody had finished. Assert every editor is at least most of its
         own box. */
      const boxes = document.querySelectorAll(".comment-box");
      if (!boxes.length) throw new Error("no comment editors mounted");
      /* Both invariants below are RELATIVE -- an editor is judged against the
         room its own container offers, never against a pixel count. An
         absolute floor here would really be an assertion about the viewport,
         and it duly failed in CI, where this page is framed in a narrow
         sandboxed iframe rather than a desktop window. */

      /* Most editors live inside a collapsed <details>, where nothing has
         layout at all. Open every phase for the measurement -- the acceptance
         grid is exactly where the worse of the two bugs was -- then put the
         page back the way it was found. */
      const wasOpen = [].map.call(document.querySelectorAll("details.phase"), function (d) {
        const o = d.open; d.open = true; return o;
      });
      let failure = null;
      boxes.forEach(function (box) {
        const wasHidden = box.hasAttribute("hidden");
        if (wasHidden) box.removeAttribute("hidden");
        const parent = box.parentElement;
        const pcs = window.getComputedStyle(parent);
        const avail = parent.clientWidth
          - (parseFloat(pcs.paddingLeft) || 0)
          - (parseFloat(pcs.paddingRight) || 0);
        const cap = parseFloat(window.getComputedStyle(box).maxWidth);
        const bw = box.getBoundingClientRect().width;
        const tw = box.querySelector("textarea").getBoundingClientRect().width;
        if (wasHidden) box.setAttribute("hidden", "");
        /* Nothing here is laid out (a zero-size frame): no claim to make. */
        if (avail < 1 || failure) return;
        /* (1) The box takes the width its container offers, up to its own
           max-width. Catches an editor placed into a narrow grid column --
           inside an acceptance criterion the box became an ordinary item in
           that row's two-column grid and rendered 28px wide. */
        const expected = isNaN(cap) ? avail : Math.min(avail, cap);
        if (bw < expected - 2) {
          failure = "editor " + Math.round(bw) + "px in a container offering "
            + Math.round(expected) + "px";
          return;
        }
        /* (2) The textarea fills the box. Catches the box being a block
           container, which left the textarea at its intrinsic cols="20". */
        if (tw < bw - 2) {
          failure = "textarea " + Math.round(tw) + "px inside a " + Math.round(bw) + "px box";
        }
      });
      document.querySelectorAll("details.phase").forEach(function (d, i) { d.open = wasOpen[i]; });
      if (failure) throw new Error(failure);
    });
    check("theme toggle offers system, light, dark and vibe", function () {
      const modes = [].map.call(
        document.querySelectorAll("[data-theme-set]"),
        function (b) { return b.getAttribute("data-theme-set"); }
      );
      if (modes.join("|") !== "|light|dark|vibe") throw new Error("modes were " + modes.join("|"));
      applyTheme("vibe", false);
      if (document.documentElement.getAttribute("data-theme") !== "vibe") {
        throw new Error("vibe did not apply");
      }
      /* System must be reachable AGAIN after an override, or "follow the OS"
         is a state a reader can only ever leave. */
      applyTheme("dark", false);
      if (document.documentElement.getAttribute("data-theme") !== "dark") {
        throw new Error("dark did not apply");
      }
      applyTheme("", false);
      if (document.documentElement.getAttribute("data-theme")) {
        throw new Error("system did not clear the override");
      }
      if (document.querySelector('[data-theme-set=""]').getAttribute("aria-pressed") !== "true") {
        throw new Error("system not marked pressed");
      }
    });
    check("phases open and shut with motion", function () {
      const phase = document.querySelector("details.phase");
      if (!phase) throw new Error("no phases on the page");
      const body = phase.querySelector(".phase-body");
      if (!body) throw new Error("phase has no body to animate");
      /* No Web Animations API, or a reader who asked for reduced motion:
         instant IS the correct behaviour, and there would be nothing for
         `heightAnimations` to report. Nothing to assert. */
      if (!canDisclose() || !body.getAnimations) return;

      setPhaseOpen(phase, false, false);

      /* Nothing below the phase may move on the FRAME of the click: the whole
         change has to be inside the animation. The summary's bottom padding
         differs between the shut and open rows and was not transitioned, so
         it shunted the rest of the page 14px the instant a phase was clicked
         -- in the opposite direction to the one the body was about to move
         it. A delta, never a position, so the frame's size cannot matter. */
      const below = phase.nextElementSibling;
      const beforeTop = below ? below.getBoundingClientRect().top : 0;

      setPhaseOpen(phase, true, true);
      if (below) {
        const shift = Math.abs(below.getBoundingClientRect().top - beforeTop);
        if (shift > 1) {
          throw new Error("clicking a phase shifted the page " + Math.round(shift)
            + "px before anything had animated");
        }
      }
      if (!phase.open) throw new Error("opening did not open the phase");
      if (phase.classList.contains("is-closing")) throw new Error("opening left is-closing set");
      const opening = heightAnimations(body);
      if (!opening.length) throw new Error("opening was not animated");
      /* It must start from collapsed, or the box would appear at full size
         and merely fade -- which is the jump, with a fade over it. */
      const first = opening[0].effect.getKeyframes()[0];
      if (parseFloat(first.height) !== 0
        || parseFloat(first.paddingTop) !== 0
        || parseFloat(first.paddingBottom) !== 0) {
        throw new Error("opening starts at height=" + first.height
          + " paddingTop=" + first.paddingTop + " paddingBottom=" + first.paddingBottom
          + ", not from nothing");
      }
      opening.forEach(function (a) { a.finish(); });

      /* The three things that make a close look right, and each of which
         has to be got wrong deliberately to break: the body is still on the
         page so there is something to shrink, the open styling is ALREADY
         off so the chevron turns with the click rather than after it, and
         the height really is being animated. */
      setPhaseOpen(phase, false, true);
      if (!phase.open) throw new Error("closing dropped the body before it could shrink");
      if (!phase.classList.contains("is-closing")) throw new Error("closing left the open styling on");
      const closing = heightAnimations(body);
      if (!closing.length) throw new Error("closing was not animated");
      const last = closing[0].effect.getKeyframes().slice(-1)[0];
      if (parseFloat(last.height) !== 0
        || parseFloat(last.paddingTop) !== 0
        || parseFloat(last.paddingBottom) !== 0) {
        throw new Error("closing ends at height=" + last.height
          + " paddingTop=" + last.paddingTop + " paddingBottom=" + last.paddingBottom
          + ", not at nothing");
      }

      /* Reversing that close has to resume from the frame ON SCREEN, opacity
         included. The body is fully visible at this point, so an open that
         assumed a start of 0 would blink it out and fade it back in. */
      setPhaseOpen(phase, true, true);
      const resumed = heightAnimations(body)[0].effect.getKeyframes()[0];
      if (parseFloat(resumed.opacity) < 0.5) {
        throw new Error("reversing a close restarts the fade at opacity " + resumed.opacity);
      }
      heightAnimations(body).forEach(function (a) { a.finish(); });

      /* And a phase already sitting where it is asked to go is left alone --
         "expand all" reaches every phase, including ones already open. */
      setPhaseOpen(phase, true, true);
      if (heightAnimations(body).length) {
        throw new Error("re-opening an already-open phase animated it again");
      }

      /* And the instant path settles everything the animated one leaves in
         flight -- otherwise a print or a second click could strand a phase
         half-open, or leave its body permanently clipped. */
      setPhaseOpen(phase, false, false);
      if (phase.open) throw new Error("the instant path left the phase open");
      if (phase.classList.contains("is-closing")) throw new Error("the instant path left is-closing set");
      if (body.style.overflow) throw new Error("the instant path left the body clipped");
    });
    check("a link in a phase row still navigates", function () {
      const phase = document.querySelector("details.phase");
      const teaser = phase && phase.querySelector(".phase-teaser");
      if (!teaser) throw new Error("no phase description to put a link in");
      /* The phase description sits INSIDE the <summary>, and the renderer
         keeps safe markdown links in it. Taking over the summary's click
         cancelled those links: one cancelled flag covers every activation
         behaviour on the event's path. Injected here rather than baked into
         the fixture because what is under test is this file's click
         handling, not the renderer. */
      const link = document.createElement("a");
      link.href = "#selftest-link-probe";
      link.textContent = "link";
      teaser.appendChild(link);
      const wasOpen = phase.open;
      /* Read the cancelled flag from the far end of the bubble path -- past
         the summary's handler -- then cancel for real, so the probe does not
         actually navigate the page it is testing. */
      let cancelled = null;
      function spy(e) { cancelled = e.defaultPrevented; e.preventDefault(); }
      document.addEventListener("click", spy);
      link.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
      document.removeEventListener("click", spy);
      link.remove();
      if (cancelled === null) throw new Error("the probe click never reached the document");
      if (cancelled) throw new Error("clicking a link in a phase row cancelled its navigation");
      if (phase.open !== wasOpen) throw new Error("clicking a link in a phase row toggled the phase");
    });
    check("commenting on a shut phase reveals the editor", function () {
      const phase = document.querySelector("details.phase");
      const btn = phase && phase.querySelector("summary .comment-btn");
      const box = phase && phase.querySelector(".phase-body > .comment-box");
      if (!btn || !box) throw new Error("phase has no comment editor mounted");
      /* Collapsing a phase leaves an open editor with `hidden === false`
         while the <details> keeps it off screen, so the two disagree. A
         plain toggle from that state hides something already invisible and
         the click does nothing a reader can see. */
      setPhaseOpen(phase, false, false);
      box.hidden = false;
      btn.click();
      if (phaseIsShut(phase)) throw new Error("commenting on a shut phase left it shut");
      if (box.hidden) throw new Error("commenting on a shut phase hid the editor");
      setPhaseOpen(phase, false, false);
      box.hidden = true;
    });
    if ((location.protocol !== "file:" || window.parent !== window)
        && !document.body.hasAttribute("data-artefacto-artifact")) {
      /* Non-plain-file contexts only — a real http(s) serving, or the CI
         harness that frames this page in a sandboxed iframe to give it the
         same opaque origin the studio's sandbox-CSP header does. Prove the
         CSP wall is live and the copy flow terminates in a handled state
         (clipboard success OR the manual fallback rendered) — never a
         silent dead-end. */
      checkAsync("fetch blocked by CSP", new Promise(function (resolve) {
        try {
          fetch("/selftest-probe").then(function () { resolve(false); }, function () { resolve(true); });
        } catch (e) { resolve(true); }
      }));
      checkAsync("copy terminates handled", new Promise(function (resolve) {
        let succeeded = false;
        copyToClipboard("selftest-probe", function () { succeeded = true; resolve(true); });
        setTimeout(function () {
          if (!succeeded) resolve(!!document.getElementById("manual-copy"));
        }, 800);
      }));
    }
    function finish() {
      const failed = results.some(r => r.indexOf("FAIL") === 0);
      const marker = document.createElement("pre");
      marker.id = "selftest-result";
      marker.textContent =
        (failed ? "ARTEFACTO_SELFTEST_FAIL" : "ARTEFACTO_SELFTEST_PASS") + "\n" + results.join("\n");
      document.body.appendChild(marker);
      /* When framed (the CI harness), relay the verdict to the parent:
         a sandboxed iframe's DOM is invisible to --dump-dom (it only
         serializes the top document), but postMessage crosses the opaque-
         origin boundary by design. No-op in every top-level context. */
      if (window.parent !== window) {
        try { window.parent.postMessage("ARTEFACTO_SELFTEST_RELAY\n" + marker.textContent, "*"); }
        catch (e) { /* relay is test-harness sugar, never load-bearing */ }
      }
    }
    Promise.all(pending).then(finish, finish);
  }

  /* ---- DOM layer -------------------------------------------------- */

  const BANNER_TEXT = "comments live in this page — copy feedback before closing";

  const SVG_NS = "http://www.w3.org/2000/svg";

  /* A small stroke-based icon built via createElementNS -- never innerHTML,
     so the markup can't smuggle anything through it -- `paths` is a list of
     `d` attribute strings, one <path> per entry. */
  function svgIcon(className, paths) {
    const svg = document.createElementNS(SVG_NS, "svg");
    svg.setAttribute("viewBox", "0 0 24 24");
    svg.setAttribute("width", "16");
    svg.setAttribute("height", "16");
    svg.setAttribute("fill", "none");
    svg.setAttribute("stroke", "currentColor");
    svg.setAttribute("stroke-width", "2");
    svg.setAttribute("stroke-linecap", "round");
    svg.setAttribute("stroke-linejoin", "round");
    svg.setAttribute("aria-hidden", "true");
    svg.setAttribute("focusable", "false");
    svg.setAttribute("class", className);
    paths.forEach(function (d) {
      const path = document.createElementNS(SVG_NS, "path");
      path.setAttribute("d", d);
      svg.appendChild(path);
    });
    return svg;
  }

  /* Speech-bubble icon for the comment button: bubble outline plus two
     short lines standing in for text. */
  function commentIcon() {
    return svgIcon("comment-btn-icon", [
      "M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z",
      "M7 8h10M7 12h6",
    ]);
  }

  /* The agent's mark: a ring around a point, something looking back. One
     SVG, four states by class: rest, is-working (a bead orbits the ring),
     is-waiting, is-off (the point hollows out). It is the agent wherever
     the agent appears: the presence pill, the avatar, and soon the ask
     control and the working row. */
  function agentMark(state) {
    const svg = document.createElementNS(SVG_NS, "svg");
    svg.setAttribute("viewBox", "0 0 24 24");
    svg.setAttribute("aria-hidden", "true");
    svg.setAttribute("focusable", "false");
    setMarkState(svg, state);
    [["ag-ring", 12, 12, 8.5], ["ag-core", 12, 12, 3.2], ["ag-orbit", 12, 3.5, 2]].forEach(function (c) {
      const circle = document.createElementNS(SVG_NS, "circle");
      circle.setAttribute("class", c[0]);
      circle.setAttribute("cx", String(c[1]));
      circle.setAttribute("cy", String(c[2]));
      circle.setAttribute("r", String(c[3]));
      svg.appendChild(circle);
    });
    return svg;
  }

  function setMarkState(svg, state) {
    svg.setAttribute("class", "ag-mark" + (state ? " is-" + state : ""));
  }

  /* Warning-triangle icon for the "Blocks approval" checkbox: triangle
     outline plus an exclamation mark (stem + dot as one path). */
  function warningIcon() {
    return svgIcon("blocking-icon", [
      "M10.29 3.86 1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z",
      "M12 9v4M12 17h.01",
    ]);
  }

  /* ---- theme --------------------------------------------------------

     Three states, and System is the default: with no explicit choice the
     page carries no data-theme attribute at all, so plan.css's
     prefers-color-scheme block decides and the page simply IS whatever the
     OS is set to -- including when the OS flips while the page is open
     (see the matchMedia listener below; the colours change on their own,
     only the toggle's highlight needs telling).

     Light and Dark record an override on <html data-theme>, which plan.css
     ranks above its prefers-color-scheme block in both directions. System
     is not the absence of a choice in the UI -- it is a choice a reader can
     come BACK to, which is the whole reason it is a button rather than just
     the initial state.

     The choice is per-plan-viewer, not per-plan, so it lives under one
     fixed key and carries across every rendered plan a reader opens.

     Storage is best-effort throughout: a file:// document in some browsers
     has an opaque origin where localStorage throws on access, and a theme
     toggle is not worth breaking the page over. */
  const THEME_KEY = "artefacto-plan:theme";

  /* The explicit override, or "" for System. Note the return is the stored
     MODE, not the colour being shown -- with System selected those differ,
     and it is the mode the toggle highlights. */
  function storedTheme() {
    try {
      const v = window.localStorage.getItem(THEME_KEY);
      return v === "light" || v === "dark" || v === "vibe" ? v : "";
    } catch (e) { return ""; }
  }

  function darkMedia() {
    return window.matchMedia ? window.matchMedia("(prefers-color-scheme: dark)") : null;
  }

  function applyTheme(mode, animate) {
    const root = document.documentElement;
    /* Colour transitions are gated on a class so first paint lands on the
       final colours instantly -- without the gate the page would visibly
       fade in from whatever the previous theme was. */
    if (animate) {
      root.classList.add("theme-anim");
      window.setTimeout(function () { root.classList.remove("theme-anim"); }, 260);
    }
    if (mode) {
      root.setAttribute("data-theme", mode);
      try { window.localStorage.setItem(THEME_KEY, mode); } catch (e) { /* see above */ }
    } else {
      root.removeAttribute("data-theme");
      try { window.localStorage.removeItem(THEME_KEY); } catch (e) { /* see above */ }
    }
    syncToggle();
  }

  /* Highlight the button for the selected MODE (System included), and tell
     the System button which way it currently resolves -- that is the one
     thing a reader cannot otherwise read off the control. */
  function syncToggle() {
    const mode = document.documentElement.getAttribute("data-theme") || "";
    document.querySelectorAll("[data-theme-set]").forEach(function (b) {
      b.setAttribute("aria-pressed", String(b.getAttribute("data-theme-set") === mode));
    });
    const sys = document.querySelector('[data-theme-set=""]');
    if (sys) sys.setAttribute("title", "Follow the system setting (currently " + current() + ")");
  }

  /* What the page is actually showing right now -- the explicit override if
     there is one, else whatever the OS is asking for. */
  function current() {
    const explicit = document.documentElement.getAttribute("data-theme");
    if (explicit) return explicit;
    const mq = darkMedia();
    return mq && mq.matches ? "dark" : "light";
  }

  let themeWired = false;
  function mountThemeToggle(root) {
    const host = root.querySelector(".pv-topbar-right");
    if (!host || host.querySelector(".pv-theme")) return;
    const group = document.createElement("div");
    group.className = "pv-theme";
    group.setAttribute("role", "group");
    group.setAttribute("aria-label", "Colour theme");
    [["", "System"], ["light", "Light"], ["dark", "Dark"], ["vibe", "Vibe"]].forEach(function (pair) {
      const btn = document.createElement("button");
      btn.type = "button";
      btn.setAttribute("data-theme-set", pair[0]);
      btn.textContent = pair[1];
      btn.addEventListener("click", function () { applyTheme(pair[0], true); });
      group.appendChild(btn);
    });
    host.appendChild(group);
    applyTheme(storedTheme(), false);

    /* Follow the OS live while System is selected. The CSS repaints itself
       -- this listener exists only so the System button's tooltip and the
       highlight stay truthful, and so the change reads as deliberate rather
       than as the page flickering. */
    const mq = darkMedia();
    if (mq && mq.addEventListener && !themeWired) {
      themeWired = true;
      mq.addEventListener("change", function () {
        if (document.documentElement.getAttribute("data-theme")) return;
        const root = document.documentElement;
        root.classList.add("theme-anim");
        window.setTimeout(function () { root.classList.remove("theme-anim"); }, 260);
        syncToggle();
      });
    }
  }

  /* ---- reading chrome -----------------------------------------------

     Two position cues, both driven by IntersectionObserver rather than a
     scroll handler so neither costs anything per frame:

       1. the topbar lifts off the page once it is no longer at the top,
       2. the phase ledger marks the row for the phase currently on screen.

     Both are decoration over information the page already carries, so a
     browser without IntersectionObserver simply gets neither. */
  function mountScrollCues(root) {
    const bar = root.querySelector(".pv-topbar");
    if (!bar || !("IntersectionObserver" in window)) return;

    /* A zero-height probe above the topbar: once it scrolls out of view the
       bar is stuck. Cheaper and steadier than reading scrollY. */
    const probe = document.createElement("div");
    probe.setAttribute("aria-hidden", "true");
    probe.style.cssText = "position:absolute;top:0;height:1px;width:1px;";
    bar.parentNode.insertBefore(probe, bar);
    const stuck = new IntersectionObserver(function (entries) {
      bar.classList.toggle("is-stuck", !entries[0].isIntersecting);
    });
    stuck.observe(probe);
    mounted.observers.push(stuck);

    const phases = root.querySelectorAll("details.phase");
    if (!phases.length) return;
    /* Fire when a phase crosses the upper third of the viewport: the row
       highlights for the phase a reader is reading, not the one that
       happens to be tallest on screen. */
    const spy = new IntersectionObserver(
      function (entries) {
        entries.forEach(function (entry) {
          if (!entry.isIntersecting) return;
          const id = entry.target.getAttribute("data-plan-ref");
          if (!id) return;
          const phaseId = id.slice("phase:".length);
          document.querySelectorAll("[data-phase-row]").forEach(function (row) {
            row.classList.toggle("is-current", row.getAttribute("data-phase-row") === phaseId);
          });
        });
      },
      { rootMargin: "-10% 0px -70% 0px" }
    );
    phases.forEach(function (p) { spy.observe(p); });
    mounted.observers.push(spy);
  }

  /* ---- disclosure ----------------------------------------------------

     A <details> element has no motion of its own: its body appears and
     disappears between one frame and the next, so opening a phase shoves
     everything below it down the page in a single jump, and shutting one
     yanks it back. `setPhaseOpen` gives that change a duration by animating
     the body's height, and every path a READER can trigger goes through it
     -- the summary itself, the expand-all and collapse-all buttons, and a
     comment button that has to open the phase it lives on.

     Programmatic paths deliberately do not. Printing and the self-test pass
     `false` for `animate` and stay instant: they want the end state, not a
     transition to it.

     Nothing here is load-bearing. A browser with no Web Animations API, or
     a reader who has asked their system for reduced motion, falls straight
     through to setting `.open` -- which is exactly the behaviour this page
     had before. */

  /* The animation currently running on a phase, if any, so a second click
     part-way through can take the phase over rather than fight it. */
  const disclosing = new WeakMap();

  /* Duration and easing for a disclosure, read out of plan.css. A value that
     is missing (no stylesheet) or unparseable falls back to a sane pair
     rather than to a zero-length -- and therefore invisible -- animation. */
  function discloseTiming() {
    const root = window.getComputedStyle(document.documentElement);
    const raw = root.getPropertyValue("--t-disclose").trim();
    const ms = raw.slice(-2) === "ms" ? parseFloat(raw) : parseFloat(raw) * 1000;
    return {
      duration: ms > 0 ? ms : 240,
      easing: root.getPropertyValue("--ease").trim() || "ease",
    };
  }

  /* Whether to animate at all. plan.css's prefers-reduced-motion block can
     only reach CSS animations and transitions, so motion driven from here
     has to consult the same query itself or it would ignore the one setting
     the whole budget is meant to answer to. */
  function canDisclose() {
    if (!window.Element || typeof Element.prototype.animate !== "function") return false;
    if (!window.matchMedia) return true;
    return !window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  }

  /* Whether a phase reads as SHUT to the person looking at it: either really
     closed, or open-but-mid-close. A click during a close has to re-open. */
  function phaseIsShut(details) {
    return !details.open || details.classList.contains("is-closing");
  }

  /* Everything the disclosure animates, as it is RIGHT NOW -- read together
     so a reversal mid-flight resumes from one consistent frame rather than
     from three properties sampled at different times.

     The padding is in here because it has to travel with the height: with
     `box-sizing: border-box` a height of 0 still leaves the padding on
     screen, so a phase would collapse to a bar of blank space and then drop
     it -- precisely the jump this is all here to remove.

     The opacity is in here because it must NOT be assumed. Hard-coding a
     start of 0 for every open blinked out any body that was already
     visible: "expand all" over a phase a reader had opened by hand faded it
     from nothing, and reversing a close snapped the content dimmer before
     bringing it back. */
  function bodyState(body) {
    const cs = window.getComputedStyle(body);
    return {
      height: body.getBoundingClientRect().height + "px",
      paddingTop: cs.paddingTop || "0px",
      paddingBottom: cs.paddingBottom || "0px",
      opacity: cs.opacity || "1",
    };
  }

  /* What a shut phase's body looks like: nothing at all, on every axis. */
  const COLLAPSED = { height: "0px", paddingTop: "0px", paddingBottom: "0px", opacity: "0" };

  /* The disclosure animations on an element, and only those. `getAnimations`
     also returns anything CSS has left lying around with a fill, so a bare
     length check would pass whether or not this file animated anything --
     the keyframes are what identify ours. */
  function heightAnimations(el) {
    if (!el.getAnimations) return [];
    return el.getAnimations().filter(function (a) {
      const frames = a.effect && a.effect.getKeyframes ? a.effect.getKeyframes() : [];
      return frames.some(function (f) { return f.height !== undefined; });
    });
  }

  /* Open or shut one phase.

     Opening is the straightforward direction: set `open` (so the chevron,
     the numeral and every other [open] rule in plan.css start their own
     transitions on the same frame), then grow the body from nothing into
     its natural height.

     Closing cannot do that in reverse, because clearing `open` removes the
     body from the page immediately and leaves nothing to shrink. So `open`
     stays set until the animation finishes, and the `is-closing` class tells
     plan.css to treat the phase as visually shut in the meantime -- see the
     `:not(.is-closing)` rules on the summary. */
  function setPhaseOpen(details, want, animate) {
    const body = details.querySelector(".phase-body");
    const running = disclosing.get(details);

    if (!body || animate === false || !canDisclose()) {
      if (running) { disclosing.delete(details); running.cancel(); }
      if (body) body.style.overflow = "";
      details.classList.remove("is-closing");
      details.open = want;
      return;
    }

    /* Already where it is being asked to go, and nothing in flight: leave it
       alone. "Expand all" reaches every phase including the ones a reader
       opened by hand, and animating one of those from its own current state
       to its own current state is a no-op for the height but a full pass for
       everything else.

       An animation that has FINISHED but whose `onfinish` has not run yet
       (that fires on a later task) is not in flight -- it still holds this
       phase's slot in `disclosing`, and its own cleanup is still correct
       when it arrives. */
    const inFlight = !!running && running.playState !== "finished";
    if (!inFlight && details.open === want && !details.classList.contains("is-closing")) return;

    /* Clip first, measure second. `overflow: hidden` makes the body a block
       formatting context, so its children's margins stop collapsing through
       it -- measuring before setting it would return a height the box never
       actually has while animating, and the phase would jump at the end by
       the difference. */
    body.style.overflow = "hidden";
    /* Where the box is NOW. Mid-animation this reads the animated values, so
       a click part-way through a close carries on from the frame on screen
       instead of restarting. Note the state is taken from `open`, never from
       the measurement: a shut <details> keeps reporting the size its body
       had when it was last open. */
    const from = details.open ? bodyState(body) : COLLAPSED;
    if (running) { disclosing.delete(details); running.cancel(); }

    if (want) {
      details.classList.remove("is-closing");
      details.open = true;
    } else {
      details.classList.add("is-closing");
    }
    /* And where it is going. Read after the cancel above, so these are the
       element's own resting values, not another animation's. */
    const to = want ? bodyState(body) : COLLAPSED;

    const timing = discloseTiming();
    const anim = body.animate(
      [from, to],
      {
        duration: timing.duration,
        easing: timing.easing,
        /* A close holds its last frame. Without the fill the body would
           spring back to full height for the one frame between the
           animation ending and `open` being cleared below. */
        fill: want ? "none" : "forwards",
      }
    );
    disclosing.set(details, anim);
    anim.onfinish = function () {
      /* A later call already took this phase over; the cleanup is its job
         now. (Cancelling does not fire this handler, so reaching here with
         a stale animation means the phase was re-clicked mid-flight.) */
      if (disclosing.get(details) !== anim) return;
      disclosing.delete(details);
      if (!want) {
        details.open = false;
        details.classList.remove("is-closing");
      }
      body.style.overflow = "";
      anim.cancel();
    };
  }

  /* Where a commentable block wants its comment button and its draft box.
     Every block type on the page pairs a heading row (where the button
     belongs, on the title's line) with a body column (where the box
     belongs, under the text it is about). Returning nulls is fine: the
     caller falls back to the block itself. */
  function commentSlots(el) {
    if (el.classList.contains("task")) {
      return { btn: el.querySelector(".task-head"), box: el.querySelector(".task-body") };
    }
    if (el.classList.contains("pv-row")) {
      const body = el.querySelector(".pv-row-body");
      return { btn: el.querySelector(".pv-row-head") || body, box: body };
    }
    if (el.classList.contains("plan-summary")) {
      const exec = el.querySelector(".summary-exec");
      return { btn: exec, box: exec };
    }
    if (el.tagName === "DETAILS") {
      return {
        btn: el.querySelector("summary .phase-head-line"),
        box: el.querySelector(".phase-body"),
      };
    }
    return { btn: null, box: null };
  }

  /* First 80 chars of the element's heading text: the element itself when
     it is a heading, else the first h1–h6 descendant, else its own
     trimmed text as a last-resort fallback. */
  function elementQuote(el) {
    let source = el;
    if (!/^h[1-6]$/i.test(el.tagName)) {
      source = el.querySelector("h1, h2, h3, h4, h5, h6") || el;
    }
    const text = (source.textContent || "").trim().replace(/\s+/g, " ");
    return text.slice(0, 80);
  }

  function draftKey(planId, fingerprint) {
    return "artefacto-plan:" + planId + ":" + fingerprint;
  }

  function loadDrafts(planId, fingerprint) {
    try {
      const raw = window.localStorage.getItem(draftKey(planId, fingerprint));
      if (!raw) return [];
      const stored = JSON.parse(raw);
      if (!stored || stored.fingerprint !== fingerprint || !Array.isArray(stored.comments)) {
        return [];
      }
      /* Old draft shape carried a `type` field (blocker/question/suggestion/
         change_request) instead of a `blocking` boolean. Restoring one of
         those as-is would silently resurrect the retired taxonomy, so
         discard the whole draft rather than partially restore it broken --
         the fingerprint gate above already covers the "plan changed"
         case; this covers "the draft's own shape changed". */
      const hasOldShape = stored.comments.some(function (c) {
        return c && Object.prototype.hasOwnProperty.call(c, "type");
      });
      if (hasOldShape) return [];
      return stored.comments;
    } catch (e) {
      return [];
    }
  }

  function saveDrafts(planId, fingerprint, comments) {
    try {
      window.localStorage.setItem(
        draftKey(planId, fingerprint),
        JSON.stringify({ fingerprint: fingerprint, comments: comments })
      );
    } catch (e) {
      /* best-effort only — quota errors, disabled storage, file:// origin, etc. */
    }
  }

  function reviewedKey(planId, fingerprint) {
    return "artefacto-plan-reviewed:" + planId + ":" + fingerprint;
  }

  function loadReviewed(planId, fingerprint) {
    try {
      const raw = window.localStorage.getItem(reviewedKey(planId, fingerprint));
      if (!raw) return [];
      const stored = JSON.parse(raw);
      if (!stored || stored.fingerprint !== fingerprint || !Array.isArray(stored.refs)) {
        return [];
      }
      return stored.refs;
    } catch (e) {
      return [];
    }
  }

  function saveReviewed(planId, fingerprint, refs) {
    try {
      window.localStorage.setItem(
        reviewedKey(planId, fingerprint),
        JSON.stringify({ fingerprint: fingerprint, refs: refs })
      );
    } catch (e) {
      /* best-effort only — same caveats as saveDrafts above */
    }
  }

  function copyToClipboard(text, done) {
    function fallback() {
      const ta = document.createElement("textarea");
      ta.value = text;
      ta.setAttribute("readonly", "");
      ta.style.position = "fixed";
      ta.style.top = "-1000px";
      ta.style.left = "-1000px";
      document.body.appendChild(ta);
      ta.focus();
      ta.select();
      let copied = false;
      try { copied = document.execCommand("copy"); } catch (e) { /* ignore */ }
      document.body.removeChild(ta);
      if (copied) done(); else showManualCopy(text);
    }
    if (navigator.clipboard && navigator.clipboard.writeText) {
      navigator.clipboard.writeText(text).then(done, fallback);
    } else {
      fallback();
    }
  }

  /* Terminal fallback: when BOTH clipboard paths fail (e.g. an opaque-origin
     sandboxed document with no clipboard permission), surface the payload for
     a manual Cmd/Ctrl-C — the paste-back loop must never dead-end silently. */
  function showManualCopy(text) {
    let panel = document.getElementById("manual-copy");
    if (panel) {
      panel.querySelector("textarea").value = text;
    } else {
      panel = document.createElement("div");
      panel.id = "manual-copy";
      const hint = document.createElement("p");
      hint.textContent =
        "Automatic copy is blocked here — select the text below and copy it manually.";
      const ta = document.createElement("textarea");
      ta.setAttribute("readonly", "");
      ta.value = text;
      const close = document.createElement("button");
      close.type = "button";
      close.textContent = "Close";
      close.addEventListener("click", function () { panel.remove(); });
      panel.appendChild(hint);
      panel.appendChild(ta);
      panel.appendChild(close);
      document.body.appendChild(panel);
    }
    const ta = panel.querySelector("textarea");
    ta.focus();
    ta.select();
  }


  /* ---- static export ------------------------------------------------

     The serverless page: drafts and reviewed marks in localStorage under
     keys that embed the plan hash, and one clipboard action that copies
     the feedback document for paste-back. Unchanged in behaviour from the
     page before the server existed, apart from the Approve toggle. */
  function mountStatic(root, plan) {
    const fingerprint = root.getAttribute("data-plan-fingerprint") || "";

    let comments = loadDrafts(plan.meta.id, fingerprint);
    let restoredCount = comments.length;

    function persist() {
      saveDrafts(plan.meta.id, fingerprint, comments);
    }

    /* ---- feedback bar ---- */
    const bar = document.createElement("div");
    bar.className = "feedback-bar";

    const banner = document.createElement("span");
    banner.className = "feedback-bar-banner";
    banner.textContent = BANNER_TEXT;
    bar.appendChild(banner);

    if (restoredCount > 0) {
      const restoredNote = document.createElement("span");
      restoredNote.className = "feedback-bar-restored";
      restoredNote.textContent = "restored " + restoredCount + " draft comments";
      bar.appendChild(restoredNote);
    }

    /* Blocking comments get their own readout ahead of the total: the
       difference between "4 comments" and "4 comments, one of which blocks
       approval" is the whole verdict the feedback document will carry. */
    const blockingCount = document.createElement("span");
    blockingCount.className = "feedback-bar-blocking";
    bar.appendChild(blockingCount);

    const count = document.createElement("span");
    count.className = "feedback-bar-count";
    bar.appendChild(count);

    const reviewedCount = document.createElement("span");
    reviewedCount.className = "feedback-bar-reviewed";
    bar.appendChild(reviewedCount);

    /* Spec 4.3: the clipboard button gains the same Approve toggle the
       served page's Send review has. */
    const approveLabel = document.createElement("label");
    approveLabel.className = "feedback-bar-approve";
    const approveBox = document.createElement("input");
    approveBox.type = "checkbox";
    approveLabel.appendChild(approveBox);
    approveLabel.appendChild(document.createTextNode("Approve"));
    bar.appendChild(approveLabel);

    const copyBtn = document.createElement("button");
    copyBtn.type = "button";
    copyBtn.className = "pv-btn is-lg is-primary feedback-bar-copy";
    copyBtn.textContent = "Copy feedback";
    bar.appendChild(copyBtn);

    root.appendChild(bar);

    function renderCount() {
      count.textContent = comments.length + (comments.length === 1 ? " comment" : " comments");
      const blocking = comments.filter(function (c) { return c.blocking; }).length;
      blockingCount.textContent = blocking > 0 ? blocking + " blocking" : "";
      /* Nothing to copy until something has been added -- or approved. */
      copyBtn.disabled = comments.length === 0 && !approveBox.checked;
      copyBtn.title = copyBtn.disabled ? "Add a comment or answer first, or approve" : "";
    }
    renderCount();
    approveBox.addEventListener("change", renderCount);

    copyBtn.addEventListener("click", function () {
      const feedback = core.buildFeedback(plan, fingerprint, comments, approveBox.checked);
      copyToClipboard(feedback.combined, function () {
        const original = "Copy feedback";
        copyBtn.textContent = "Copied ✓";
        /* One short pulse: the label change alone is easy to miss on a
           button the reader is still looking at when it fires. */
        copyBtn.classList.add("is-copied");
        window.setTimeout(function () {
          copyBtn.textContent = original;
          copyBtn.classList.remove("is-copied");
        }, 2000);
      });
    });

    /* ---- per-element comment buttons ---- */
    const refEls = root.querySelectorAll("[data-plan-ref]");
    refEls.forEach(function (el) {
      const ref = el.getAttribute("data-plan-ref");
      /* Snapshot the quote before any UI (comment button/box) is appended
         into el -- elementQuote's no-heading fallback reads el.textContent,
         which would otherwise pick up the injected chrome text. */
      const quote = elementQuote(el);

      /* The CTA names the action the element actually invites: an open
         question wants an answer, everything else wants a comment. Both
         produce the same feedback-contract comment — only the label (and
         placeholder) differ. */
      const kind = ref.split(":")[0];
      const isQuestion = kind === "question";
      const label = isQuestion ? "Answer" : "Comment";
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = "comment-btn";
      btn.appendChild(commentIcon());
      /* The label rides in a span so CSS can collapse the button to
         icon-only where a full button doesn't fit (the per-criterion
         line anchors); title + aria-label keep the name either way. */
      const labelSpan = document.createElement("span");
      labelSpan.className = "comment-btn-label";
      labelSpan.textContent = label;
      btn.appendChild(labelSpan);
      btn.title = label;
      btn.setAttribute("aria-label", isQuestion ? "Answer this question" : "Add comment");

      const box = document.createElement("div");
      box.className = "comment-box";
      box.hidden = true;

      const textarea = document.createElement("textarea");
      textarea.placeholder = isQuestion ? "Answer…" : "Add a comment…";
      textarea.rows = 3;

      const blockingRow = document.createElement("label");
      blockingRow.className = "comment-box-blocking";

      const blockingBox = document.createElement("input");
      blockingBox.type = "checkbox";

      blockingRow.appendChild(blockingBox);
      blockingRow.appendChild(warningIcon());
      blockingRow.appendChild(document.createTextNode("Blocks approval"));

      const actions = document.createElement("div");
      actions.className = "comment-box-actions";

      const addBtn = document.createElement("button");
      addBtn.type = "button";
      addBtn.className = "pv-btn is-primary composer-send";
      addBtn.textContent = "Add";

      const cancelBtn = document.createElement("button");
      cancelBtn.type = "button";
      cancelBtn.className = "pv-btn is-quiet composer-cancel";
      cancelBtn.textContent = "Cancel";

      actions.appendChild(addBtn);
      actions.appendChild(cancelBtn);
      /* The same foot row as the served composer: the toggle on the left,
         the actions on the right. */
      const foot = document.createElement("div");
      foot.className = "comment-box-foot";
      box.appendChild(textarea);
      foot.appendChild(blockingRow);
      foot.appendChild(actions);
      box.appendChild(foot);

      btn.addEventListener("click", function (e) {
        /* The same guard the reviewed toggle carries below, for the same
           reason: on a PHASE this button is placed inside the <summary>
           (see commentSlots), and a click that reaches the summary triggers
           its toggle. Commenting on an open phase used to shut it, hiding
           the very box the click had just opened. */
        e.stopPropagation();
        if (el.tagName === "DETAILS" && phaseIsShut(el)) {
          /* The button is in the summary but the box is in the body, which
             is off screen while the phase is shut -- whatever `hidden` says.
             It can be left un-hidden, too: collapsing a phase that had an
             editor open does not touch the box. Toggling from there would
             hide something the reader cannot see and the click would have no
             visible effect at all, so a shut phase always REVEALS rather
             than toggles. Focus without scrolling: the body is mid-animation
             and clipped, and scrolling into it now would leave the clip box
             scrolled once it settles. */
          box.hidden = false;
          setPhaseOpen(el, true, true);
          textarea.focus({ preventScroll: true });
          return;
        }
        box.hidden = !box.hidden;
        if (!box.hidden) textarea.focus();
      });

      cancelBtn.addEventListener("click", function () {
        textarea.value = "";
        blockingBox.checked = false;
        box.hidden = true;
      });

      addBtn.addEventListener("click", function () {
        const text = textarea.value.trim();
        if (!text) return;
        comments.push(core.makeComment(ref, quote, text, blockingBox.checked));
        textarea.value = "";
        blockingBox.checked = false;
        box.hidden = true;
        renderCount();
        persist();
      });

      /* Each commentable block is a grid or a flex column with named
         slots, so the button and the box go into the slots rather than
         being appended to the block itself -- appending to a .task would
         make them extra grid items and blow the two-column layout apart.
         Unknown shapes fall back to the block itself, which is always
         valid markup even if the placement is plain. */
      const slots = commentSlots(el);
      (slots.btn || el).appendChild(btn);
      (slots.box || el).appendChild(box);
    });

    /* ---- expand/collapse all ---- */
    mountDisclosure(root);

    /* ---- reviewed-state checkboxes ---- */
    const reviewed = new Set(loadReviewed(plan.meta.id, fingerprint));
    function persistReviewed() {
      saveReviewed(plan.meta.id, fingerprint, Array.from(reviewed));
    }

    /* Only real task cards (data-plan-ref="task:…") count toward the K/N
       ratio in the feedback bar. Phases also get a reviewed checkbox (on
       their summary line) so a reviewer can mark a whole phase read at a
       glance, but a phase isn't a task, so folding it into the same
       denominator would mix units and complicate the arithmetic — N stays
       exactly "how many tasks", full stop. */
    const taskEls = root.querySelectorAll('.task[data-plan-ref^="task:"]');

    function renderReviewedCount() {
      let k = 0;
      taskEls.forEach(function (el) {
        if (reviewed.has(el.getAttribute("data-plan-ref"))) k++;
      });
      reviewedCount.textContent = k + "/" + taskEls.length + " reviewed";
    }

    function addReviewedBox(container, ref) {
      /* A bare checkbox read as decoration at first glance (first-dogfood
         feedback) -- the visible label says what checking it does, and
         flips to a past-tense confirmation once checked. */
      const label = document.createElement("label");
      label.className = "reviewed-toggle";
      const box = document.createElement("input");
      box.type = "checkbox";
      box.className = "reviewed-box";
      const text = document.createElement("span");
      text.className = "reviewed-toggle-text";
      function sync() {
        text.textContent = box.checked ? "Reviewed" : "Mark reviewed";
        container.classList.toggle("is-reviewed", box.checked);
      }
      box.checked = reviewed.has(ref);
      sync();
      label.appendChild(box);
      label.appendChild(text);
      label.addEventListener("click", function (e) {
        /* A control nested inside a <summary> still bubbles its click up
           to the <summary>'s default action (toggling the parent
           <details> open/closed) unless stopped here -- marking reviewed
           should not also collapse or expand the phase. On the label, so
           it covers clicks on the text as well as the box. */
        e.stopPropagation();
      });
      box.addEventListener("change", function () {
        if (box.checked) {
          reviewed.add(ref);
        } else {
          reviewed.delete(ref);
        }
        sync();
        persistReviewed();
        renderReviewedCount();
      });
      return label;
    }

    /* Both toggles land on their block's heading ROW (.phase-head-line,
       .task-head) rather than inside the heading element itself: the row is
       already a baseline-aligned flex line built to carry the title plus its
       badges, so a control added to it lines up with them for free. */
    root.querySelectorAll("details.phase").forEach(function (details) {
      const ref = details.getAttribute("data-plan-ref");
      const line = details.querySelector("summary .phase-head-line");
      if (!ref || !line) return;
      line.appendChild(addReviewedBox(details, ref));
    });

    taskEls.forEach(function (el) {
      const ref = el.getAttribute("data-plan-ref");
      if (!ref) return;
      const line = el.querySelector(".task-head") || el;
      line.appendChild(addReviewedBox(el, ref));
    });

    renderReviewedCount();
  }

  /* ---- the served page ------------------------------------------------

     One session per page load. It owns the socket, the state, and the
     drafts; `mount(root)` may run any number of times against it, once per
     body swap, and rebuilds the DOM from the state each time.

     Four rules, each with a reason:

       1. The server is the only store. Nothing here reads localStorage for
          review state.
       2. Every write is idempotent: a client id is minted when a composer
          opens and stored with its draft, so a send repeated after a swap,
          a reload, or a retry is the same command, and the server answers a
          repeat with what it did the first time.
       3. The page applies its own writes from the reply, because the
          broadcast skips the page that posted. A frame that carries one of
          the page's own client ids is skipped, because after a reconnect a
          write in flight may have named the old socket and been broadcast
          to the new one.
       4. While the page is catching up from a snapshot, nothing is applied
          directly: frames and the page's own replies are buffered, and the
          snapshot's last_seq decides what it already holds. */

  let session = null;

  /* Small DOM builder. Text is always textContent: reviewer and agent
     text is data, never markup (spec 8). */
  function el(tag, attrs) {
    const e = document.createElement(tag);
    const a = attrs || {};
    for (const k in a) {
      const v = a[k];
      if (v === null || v === undefined || v === false) continue;
      if (k === "class") e.className = v;
      else if (k === "text") e.textContent = v;
      else if (k === "dataset") for (const dk in v) e.dataset[dk] = v[dk];
      else if (k.indexOf("on") === 0 && typeof v === "function") e.addEventListener(k.slice(2), v);
      else if (v === true) e.setAttribute(k, "");
      else e.setAttribute(k, v);
    }
    for (let i = 2; i < arguments.length; i++) {
      const c = arguments[i];
      if (c === null || c === undefined) continue;
      if (Array.isArray(c)) c.forEach(function (x) { if (x) e.appendChild(x); });
      else e.appendChild(typeof c === "string" ? document.createTextNode(c) : c);
    }
    return e;
  }

  /* Notice text with `code` spans, built as elements: the text is data. */
  function richText(text) {
    const frag = document.createDocumentFragment();
    String(text).split("`").forEach(function (part, i) {
      if (!part) return;
      frag.appendChild(i % 2 ? el("code", { text: part }) : document.createTextNode(part));
    });
    return frag;
  }

  function newId(prefix) {
    let rand = "";
    try {
      const bytes = new Uint8Array(8);
      window.crypto.getRandomValues(bytes);
      rand = Array.from(bytes, function (b) { return ("0" + b.toString(16)).slice(-2); }).join("");
    } catch (e) {
      rand = Math.random().toString(16).slice(2) + Date.now().toString(16);
    }
    return prefix + "-" + rand;
  }

  function nowIso() { return new Date().toISOString(); }

  function refKind(ref) { return String(ref).split(":")[0]; }

  function findRef(root, ref) {
    return findByAttr(root, "[data-plan-ref]", "data-plan-ref", ref);
  }

  /* The first element under `root` matching `selector` whose `attr` is
     exactly `value`. Compared as text, never built into a selector: refs
     and ids are data. */
  function findByAttr(root, selector, attr, value) {
    const els = root.querySelectorAll(selector);
    for (let i = 0; i < els.length; i++) {
      if (els[i].getAttribute(attr) === value) return els[i];
    }
    return null;
  }

  /* Settings a browser test may shorten. Spec 6.2: at most one activity
     ping per 30 seconds. The reconnect schedule is eight tries over about
     half a minute, then the page says so rather than spinning forever. A
     fetch that has not answered in ten seconds is treated as failed. */
  core.settings = {
    pingEveryMs: 30000,
      /* How long a question may go unanswered before the working row says
         "still waiting" and the bead stops. */
      stillWaitingMs: 120000,
    backoffMs: [500, 1000, 2000, 4000, 8000, 8000, 8000, 8000],
    fetchTimeoutMs: 10000,
    /* How long a socket must stay open before the retry budget resets. */
    stableAfterMs: 3000,
  };

  /* fetch with a deadline, so a wedged server leaves a visible error
     instead of a button disabled forever. */
  function fetchBounded(url, opts) {
    const options = Object.assign({}, opts || {});
    let timer = null;
    if (typeof AbortController === "function") {
      const ctl = new AbortController();
      options.signal = ctl.signal;
      timer = setTimeout(function () { ctl.abort(); }, core.settings.fetchTimeoutMs);
    }
    return fetch(url, options).then(function (r) {
      if (timer) clearTimeout(timer);
      return r;
    }, function (e) {
      if (timer) clearTimeout(timer);
      throw e;
    });
  }

  function createSession(artifact) {
    const S = {
      artifact: artifact,
      state: core.emptyState(artifact),
      root: null,
      plan: null,
      socket: null,
      connected: false,
      pageId: null,
      syncing: false,
      syncGen: 0,
      buffer: [],
      attempts: 0,
      reconnects: 0,
      gone: false,
      lost: false,
      stopping: false,
      /* Writes the server has not answered yet; the bar says Saving. */
      inflight: 0,
      submitting: false,
      /* Questions the agent has not answered yet: thread id (or "page") ->
         { since }. Page-side, so a body swap keeps the working row. */
      pending: {},
      /* What is typed into a question thread's persistent input, by
         thread id, so a swap re-creates the input with its text. Mirrored
         to session storage so a reload keeps it too. */
      threadDrafts: {},

      lastPing: 0,
      previousTitle: null,
      own: {},
      /* last_seq of the newest snapshot applied. A reply at or below it
         describes an event the snapshot already held. */
      snapshotSeq: 0,
      /* Reviewed marks with a send in flight: ref -> { on, count }. The
         page shows the reviewer's choice until the last reply is in. */
      pendingMarks: {},
      /* The highest seq applied for each set-valued write (a reviewed mark
         per ref, an answer per question). Two replies for one key can
         arrive in either order; the higher seq is the server's truth. */
      localSeq: {},
      /* End-of-backoff probes that answered 200 while the socket kept
         failing. Bounded, or the page would cycle forever without ever
         saying it gave up. */
      probes: 0,
      timers: {},
      applied: 0,
      ui: { chatOpen: false },
    };
    const base = location.pathname.replace(/\/$/, "");
    S.cmdUrl = base + "/cmd";
    S.stateUrl = base + "/state";
    S.socketUrl = (location.protocol === "https:" ? "wss://" : "ws://") + location.host + "/ws";

    /* ---- drafts: sessionStorage, keyed by composer id ------------------

       Every editable control is a composer with an id minted when it
       opens. Its text is stored under that id together with what it
       targets, the revision it opened against, and the client id its
       command will carry. Two composers on one element stay apart because
       the id, not the ref, is the key. On restore the command it sends
       carries the revision it was OPENED against, so text written against
       revision 3 never arrives labelled revision 4, and the same client
       id, so a send repeated after a reload is not a second comment. */
    const DRAFTS_KEY = "artefacto:drafts:" + artifact;
    function loadDraftMap() {
      try {
        const raw = window.sessionStorage.getItem(DRAFTS_KEY);
        const map = raw ? JSON.parse(raw) : {};
        return map && typeof map === "object" ? map : {};
      } catch (e) { return {}; }
    }
    function saveDraftMap(map) {
      try { window.sessionStorage.setItem(DRAFTS_KEY, JSON.stringify(map)); } catch (e) { /* best effort */ }
    }
    function saveDraft(d) { const m = loadDraftMap(); m[d.id] = d; saveDraftMap(m); }
    function dropDraft(id) { const m = loadDraftMap(); delete m[id]; saveDraftMap(m); }
    /* Declared here, above its first read: a const declared further down
       would be in its dead zone when the session starts, and the guarded
       read would quietly answer "closed". */
    const PANEL_KEY = "artefacto.panel";
    S.ui.chatOpen = panelStored();
    /* The panel's composer: its text and what it is aimed at (an element,
       or a thread), kept across a swap and a reload. */
    const PANEL_DRAFT_KEY = "artefacto:panel:" + artifact;
    function loadPanelDraft() {
      try {
        const raw = window.sessionStorage.getItem(PANEL_DRAFT_KEY);
        const d = raw ? JSON.parse(raw) : null;
        return d && typeof d === "object" ? d : {};
      } catch (e) { return {}; }
    }
    function savePanelDraft() {
      try { window.sessionStorage.setItem(PANEL_DRAFT_KEY, JSON.stringify(S.ui.panel)); } catch (e) { /* best effort */ }
    }
    S.ui.panel = Object.assign({ text: "", ref: null, thread: null, quote: null }, loadPanelDraft());
    S.ui.panelEvents = [];

    /* ---- transport ------------------------------------------------- */

    function post(cmd) {
      const body = Object.assign({ page: S.pageId }, cmd);
      const isWrite = cmd.cmd !== "ping";
      function attempt(n) {
        return fetchBounded(S.cmdUrl, {
          method: "POST",
          credentials: "same-origin",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(body),
        }).then(function (r) {
          if (r.status === 401 || r.status === 403) { sessionLost(); throw new Error("not signed in"); }
          if (!r.ok) throw new Error("HTTP " + r.status);
          return r.json();
        }).catch(function (e) {
          if (n < 3 && !S.lost) {
            return new Promise(function (resolve) { setTimeout(resolve, 250 * Math.pow(2, n)); })
              .then(function () { return attempt(n + 1); });
          }
          /* The reply is lost, but the write may have landed: the server
             skips this page when it broadcasts its own command, so only a
             snapshot can show it. */
          if (isWrite && !S.lost && S.connected) resync();
          throw e;
        });
      }
      return attempt(0);
    }

    /* Send a command and fold its result in. A reply with `seq: 0` is the
       server saying it already did this (a repeated client id); what it did
       may or may not be in the state by now, so the page resyncs rather
       than guess. While catching up, the reply is buffered as a one-event
       frame with the seq the server assigned, and the snapshot's last_seq
       decides whether it is already inside the snapshot. */
    function send(cmd) {
      if (!cmd.client_id) cmd.client_id = newId("cid");
      S.own[cmd.client_id] = true;
      S.inflight++;
      renderBar();
      const settle = function () { S.inflight = Math.max(0, S.inflight - 1); renderBar(); };
      return post(cmd).then(function (reply) { settle(); return reply; }, function (e) { settle(); throw e; }).then(function (reply) {
        if (!reply || !reply.ok) throw new Error((reply && reply.error) || "refused");
        if (reply.seq === 0) {
          resync();
          return reply;
        }
        const ev = core.localEvent(cmd, reply, nowIso());
        if (ev) {
          ev.data.client_id = cmd.client_id;
          if (S.syncing) {
            S.buffer.push({ format: "artefacto.frame/1", seq: reply.seq, events: [ev], own: true });
          } else if (reply.seq <= S.snapshotSeq) {
            /* Committed before the newest snapshot was taken, and the
               reply arrived after the snapshot was applied: already in. */
            renderAll();
          } else {
            applyEvents([ev], null, true);
            if (reply.seq > S.state.lastSeq) S.state.lastSeq = reply.seq;
            renderAll();
          }
        }
        return reply;
      });
    }

    /* For a write that sets a value rather than appending one, the event
       with the higher seq is the later write, whichever arrives first.
       Records the seq and says whether this event is older than one
       already applied for its key. Every path that folds an event — a
       socket frame, a buffered own reply, a direct reply — goes through
       `applyEvents`, so the rule holds across all of them. */
    function staleForKey(ev) {
      let key = null;
      const d = ev.data || {};
      if (ev.type === "element.reviewed") key = "reviewed:" + d.ref;
      else if (ev.type === "question.answered") key = "answer:" + d.question;
      if (!key) return false;
      const seen = S.localSeq[key] || 0;
      if (ev.seq < seen) return true;
      S.localSeq[key] = ev.seq;
      return false;
    }

    /* The one place events are folded. `cursor` is the catch-up rule
       (`core.applyFrame`); `own` says these are the page's own replies,
       which no client-id filter or artifact filter applies to. */
    function applyEvents(events, cursor, own) {
      const kept = events.filter(function (e) {
        if (!own) {
          if (e.artifact && e.artifact !== S.artifact) return false;
          if (e.data && e.data.client_id && S.own[e.data.client_id]) return false;
        }
        return !staleForKey(e);
      });
      const seq = events.length ? events[events.length - 1].seq : 0;
      const applied = core.applyFrame(S.state, { seq: seq, events: kept }, cursor);
      S.applied += applied.length;
      trackPending(applied);
      return applied;
    }

    /* A question is pending from the moment the server accepted it until
       the agent writes into the same thread (or the page-level panel),
       whichever tab asked. */
    function trackPending(events) {
      events.forEach(function (e) {
        const d = e.data || {};
        if (e.type === "chat.sent" && e.actor === "reviewer") {
          pendingSet(d.thread || "page");
        } else if ((e.type === "chat.sent" || e.type === "thread.replied") && e.actor === "agent") {
          delete S.pending[d.thread || "page"];
        } else if (e.type === "thread.deleted") {
          delete S.pending[d.thread];
        }
      });
    }

    function pendingSet(key, since) {
      if (S.pending[key]) return;
      S.pending[key] = { since: since || Date.now() };
      const left = Math.max(0, core.settings.stillWaitingMs - (Date.now() - S.pending[key].since));
      window.setTimeout(function () { if (S.pending[key]) renderAll(); }, left + 50);
    }

    /* The event stream says when a question was asked; the state says
       whether it has been answered. Reconciled before every render, so a
       snapshot that carried the answer, a reload, a second tab, and a
       resolution all end (or start) the working state the same way. A
       question thread's last turn by the reviewer is a question waiting;
       an ask inside a comment thread is only known from the event, and is
       cleared the same way once the agent has written after it. */
    function reconcilePending() {
      const seen = {};
      S.state.threads.forEach(function (t) {
        const open = t.status === "open" || t.status === "unanchored";
        const last = t.messages[t.messages.length - 1];
        const waiting = !!last && last.actor === "reviewer" && open;
        if (t.asked && waiting) {
          pendingSet(t.id, Date.parse(last.ts) || Date.now());
        } else if (!waiting) {
          delete S.pending[t.id];
        }
        seen[t.id] = true;
      });
      Object.keys(S.pending).forEach(function (key) {
        if (key !== "page" && !seen[key]) delete S.pending[key];
      });
      const chat = S.state.chat[S.state.chat.length - 1];
      if (chat && chat.actor === "reviewer") pendingSet("page", Date.parse(chat.ts) || Date.now());
      else delete S.pending.page;
    }

    /* What the working row says: the mark's state and a line. */
    function pendingLabel(key) {
      const p = S.pending[key];
      if (!p) return null;
      if (!S.state.presence) return { state: "off", text: "waiting for an agent\u2026" };
      if (Date.now() - p.since > core.settings.stillWaitingMs) return { state: "waiting", text: "still waiting\u2026" };
      return { state: "working", text: "thinking\u2026" };
    }

    function ping() {
      const now = Date.now();
      if (now - S.lastPing < core.settings.pingEveryMs || S.lost) return;
      S.lastPing = now;
      post({ cmd: "ping" }).catch(function () { /* activity is best effort */ });
    }

    function connect() {
      if (S.socket || S.lost) return;
      let ws;
      try { ws = new WebSocket(S.socketUrl); } catch (e) { scheduleReconnect(); return; }
      S.socket = ws;
      ws.onopen = function () {
        S.connected = true;
        S.gone = false;
        /* The retry budget is given back only once the socket has stayed
           open a while: a socket that opens and closes at once would
           otherwise reconnect forever without ever saying so. */
        if (S.timers.stable) clearTimeout(S.timers.stable);
        S.timers.stable = setTimeout(function () {
          S.timers.stable = null;
          S.attempts = 0;
          S.probes = 0;
        }, core.settings.stableAfterMs);
        S.stopping = false;
        notice("gone", null);
        notice("stopping", null);
        renderPresence();
        resync();
      };
      ws.onmessage = function (m) {
        let frame;
        try { frame = JSON.parse(m.data); } catch (e) { return; }
        if (frame.format === "artefacto.hello/1") {
          S.pageId = frame.page;
          /* The lease holder at this moment: presence is announced on
             change only, so a page that connects after the agent attached
             would otherwise never be told. The snapshot carries it too. */
          S.state.presence = frame.presence || null;
          renderPresence();
          return;
        }
        if (S.syncing) { S.buffer.push(frame); return; }
        applyFrame(frame, null);
      };
      ws.onclose = function () {
        if (S.socket !== ws) return;
        if (S.timers.stable) { clearTimeout(S.timers.stable); S.timers.stable = null; }
        S.socket = null;
        S.connected = false;
        /* The next socket gets a new id. A write posted before its hello
           arrives must not name this one: the server would skip the wrong
           socket and the page would count the write twice. */
        S.pageId = null;
        renderPresence();
        if (!S.lost) scheduleReconnect();
      };
      ws.onerror = function () { /* close follows */ };
    }

    function scheduleReconnect() {
      if (S.timers.reconnect || S.lost) return;
      const schedule = core.settings.backoffMs;
      if (S.attempts >= schedule.length) {
        /* The handshake's status is invisible to script, so a cookie that
           stopped working looks exactly like a server that stopped
           answering. One ordinary resync tells them apart: 401 is signed
           out, 404 is gone, a snapshot means HTTP answers and the socket
           is what will not — in which case the state is taken and the
           socket tried again, a bounded number of times, then the page
           says so. */
        resync().then(function (outcome) {
          if (S.lost || S.socket) return;
          /* "Superseded" means another resync overtook this one — a
             write's repeated reply starts one — so HTTP answered, whatever
             the socket did. It counts as an answered probe: bounded like
             one, and the socket is tried again like one. */
          const applied = outcome === "applied" || outcome === "superseded";
          if (applied && S.probes < 3) {
            S.probes++;
            S.attempts = 0;
            connect();
            return;
          }
          S.gone = true;
          notice("gone", S.stopping
            ? "The server stopped. Run `artefacto serve`, then reload this page."
            : applied
              ? "The server answers, but its socket will not connect. Reload this page, or run `artefacto open` for a fresh link."
              : "The server is not answering. If it moved to a new port, run `artefacto open` for a fresh link.",
            { action: "Retry", onAction: function () {
              S.attempts = 0;
              S.probes = 0;
              S.gone = false;
              notice("gone", null);
              renderPresence();
              connect();
            } });
          renderPresence();
        });
        return;
      }
      const wait = schedule[S.attempts++];
      S.reconnects++;
      S.timers.reconnect = setTimeout(function () {
        S.timers.reconnect = null;
        connect();
      }, wait);
    }

    function sessionLost() {
      if (S.lost) return;
      S.lost = true;
      if (S.socket) { try { S.socket.close(); } catch (e) { /* ignore */ } }
      notice("lost", "This page is no longer signed in. Run `artefacto open` for a fresh link; your drafts are kept.");
      renderPresence();
      renderBar();
    }

    /* Rebuild the state from the server, then apply whatever arrived while
       that was in flight. The snapshot's `last_seq` is the cursor: logged
       events at or below it are already inside the snapshot. A resync
       started while another is in flight supersedes it: only the newest
       snapshot is applied, and the buffer waits for it. */
    function resync() {
      S.syncing = true;
      const gen = ++S.syncGen;
      if (S.timers.resync) { clearTimeout(S.timers.resync); S.timers.resync = null; }
      return fetchBounded(S.stateUrl, { credentials: "same-origin" })
        .then(function (r) {
          if (r.status === 401 || r.status === 403) { sessionLost(); throw new Error("not signed in"); }
          if (r.status === 404) {
            S.lost = true;
            notice("lost", "This artifact is no longer on the server.");
            renderPresence();
            renderBar();
            throw new Error("gone");
          }
          if (!r.ok) throw new Error("HTTP " + r.status);
          return r.json();
        })
        .then(function (snap) {
          if (gen !== S.syncGen) return "superseded";
          applySnapshot(snap);
          drain(snap.last_seq || 0, snap.last_seq || 0);
          return "applied";
        })
        .catch(function () {
          if (gen !== S.syncGen) return "superseded";
          if (S.lost) {
            /* A lost page still shows what the server accepted: its own
               buffered replies apply, then nothing is left "catching up"
               or pending, because nothing will ever settle it. */
            drain(S.state.lastSeq, null);
            S.pendingMarks = {};
            renderAll();
            return "failed";
          }
          /* Best effort: fold in what arrived, skipping what the state
             already has. The page's own replies are applied whatever
             their seq — no snapshot holds them — and it tries again. */
          drain(S.state.lastSeq, null);
          if (S.connected && !S.timers.resync) {
            S.timers.resync = setTimeout(function () { S.timers.resync = null; resync(); }, 1500);
          }
          return "failed";
        });
    }

    function drain(cursor, ownCursor) {
      const frames = S.buffer;
      S.buffer = [];
      S.syncing = false;
      /* In log order, whatever order the replies came back in. A stable
         sort keeps announced frames, which borrow a seq, where they were
         relative to their equals. */
      frames.forEach(function (f, i) { f.arrived = i; });
      frames.sort(function (a, b) { return (a.seq - b.seq) || (a.arrived - b.arrived); });
      frames.forEach(function (f) { applyFrame(f, f.own ? ownCursor : cursor); });
      /* Marks whose last reply came while catching up were kept pending
         until their buffered frame had a chance to apply. */
      for (const ref in S.pendingMarks) {
        if (S.pendingMarks[ref].count <= 0) delete S.pendingMarks[ref];
      }
      renderAll();
    }

    function applySnapshot(snap) {
      const fresh = core.fromSnapshot(snap);
      const previous = S.state;
      S.state = fresh;
      S.snapshotSeq = Math.max(S.snapshotSeq, fresh.lastSeq);
      const title = function (state) {
        return state.plan && state.plan.meta ? state.plan.meta.title : null;
      };
      if (fresh.revision > previous.revision && previous.revision > 0) {
        /* A revision the page never saw as a frame: the banner still
           belongs to the reviewer, so it is built from the snapshot, and
           the threads it addressed are read off the two states. */
        S.previousTitle = title(previous);
        const resolved = fresh.threads.filter(function (t) {
          if (t.status !== "changed" && t.status !== "declined") return false;
          const was = previous.threads.find(function (x) { return x.id === t.id; });
          return !was || was.status !== t.status;
        }).map(function (t) { return { type: "thread.resolved", data: { thread: t.id, status: t.status } }; });
        revisionNotice({ data: { summary: snap.summary } }, resolved);
      }
      const fingerprint = document.body.getAttribute("data-plan-fingerprint") || "";
      if (snap.html && snap.plan_hash && snap.plan_hash !== fingerprint) {
        swapBody(snap.html, snap.revision);
      } else {
        renderAll();
      }
    }

    /* A frame from the socket, or one of the page's own buffered replies.
       `cursor` is set only while catching up. A socket frame carrying one
       of this page's own client ids is skipped: the page applied that
       write from its reply. */
    function applyFrame(frame, cursor) {
      /* The socket carries every artifact's frames. An event that names
         another artifact is not this review's: its thread would count
         here and its push would swap this body. Events with no artifact
         (presence, the stop) are about the server, and apply. */
      const before = { title: S.state.plan && S.state.plan.meta ? S.state.plan.meta.title : null };
      const applied = applyEvents(frame.events || [], cursor, !!frame.own);
      if (frame.seq > S.state.lastSeq) S.state.lastSeq = frame.seq;
      let swapped = false;
      applied.forEach(function (e) {
        switch (e.type) {
          case "revision.published":
            S.previousTitle = before.title;
            S.ui.sentAt = null;
            notice("sent", null);
            if (frame.html && !swapped) {
              swapped = true;
              swapBody(frame.html, S.state.revision);
            }
            revisionNotice(e, applied);
            break;
          case "nudge":
            notice("nudge", (e.data && e.data.text) || "The agent asked for your attention.", { dismiss: true });
            panelEvent("nudge", (e.data && e.data.text) || "The agent asked for your attention.");
            break;
          case "server.stopping":
            S.stopping = true;
            notice("stopping", "The server is stopping. This page will try to reconnect.");
            break;
          default:
            break;
        }
      });
      if (!swapped) renderAll();
    }

    /* ---- the body swap ----------------------------------------------

       Spec 4.3: swap the body, mount again, then restore -- disclosure
       state by element id, focus and caret, and the scroll position
       anchored to an element rather than a pixel offset, because geometry
       changes between revisions. Drafts are not touched: they live in
       sessionStorage under their composer ids and are put back by mount. */
    function captureView() {
      const open = [];
      document.querySelectorAll("details.phase").forEach(function (d) {
        if (d.open && !d.classList.contains("is-closing")) open.push(d.getAttribute("data-plan-ref"));
      });
      let focus = null;
      const active = document.activeElement;
      if (active && (active.tagName === "TEXTAREA" || active.tagName === "INPUT")) {
        const composer = active.closest("[data-composer]");
        if (composer) {
          focus = { composer: composer.getAttribute("data-composer"), start: active.selectionStart, end: active.selectionEnd };
        } else if (active.closest(".pv-panel-composer")) {
          focus = { panel: true, start: active.selectionStart, end: active.selectionEnd };
        }
      }
      const anchors = [];
      const refs = document.querySelectorAll("[data-plan-ref]");
      for (let i = 0; i < refs.length && anchors.length < 3; i++) {
        const r = refs[i].getBoundingClientRect();
        if (r.bottom > 0 && r.height > 0) anchors.push({ ref: refs[i].getAttribute("data-plan-ref"), top: r.top });
      }
      return { open: open, focus: focus, anchors: anchors, scrollY: window.scrollY };
    }

    function restoreView(view) {
      document.querySelectorAll("details.phase").forEach(function (d) {
        const ref = d.getAttribute("data-plan-ref");
        setPhaseOpen(d, view.open.indexOf(ref) >= 0, false);
      });
      let placed = false;
      for (let i = 0; i < view.anchors.length && !placed; i++) {
        const target = findRef(document.body, view.anchors[i].ref);
        if (!target) continue;
        const now = target.getBoundingClientRect().top;
        /* Instant, whatever plan.css says about smooth scrolling: this is
           putting the reader back, not taking them somewhere. */
        window.scrollBy({ top: now - view.anchors[i].top, left: 0, behavior: "instant" });
        placed = true;
      }
      if (!placed) window.scrollTo({ top: view.scrollY, left: 0, behavior: "instant" });
      /* A thread's input is created by the render that follows the mount,
         which may wait on a snapshot; if the box is not here yet the focus
         is kept and applied by the next render. */
      if (view.focus && !applyFocus(view.focus)) S.ui.pendingFocus = view.focus;
    }

    function applyFocus(f) {
      const box = f.panel
        ? document.querySelector(".pv-panel-composer textarea")
        : document.querySelector('[data-composer="' + f.composer + '"] textarea');
      if (!box) return false;
      box.focus({ preventScroll: true });
      try { box.setSelectionRange(f.start, f.end); } catch (e) { /* ignore */ }
      return true;
    }

    function swapBody(html, revision) {
      const view = captureView();
      const doc = new DOMParser().parseFromString(html, "text/html");
      const next = doc.body;
      /* Only the data island travels; the page's own script is already
         running and must not be handed a second copy. */
      next.querySelectorAll("script").forEach(function (s) {
        if (s.getAttribute("type") !== "application/json") s.remove();
      });
      const body = document.body;
      Array.from(body.attributes).forEach(function (a) { body.removeAttribute(a.name); });
      Array.from(next.attributes).forEach(function (a) { body.setAttribute(a.name, a.value); });
      body.setAttribute("data-artefacto-artifact", S.artifact);
      body.setAttribute("data-artefacto-revision", String(revision || S.state.revision));
      const nodes = Array.from(next.childNodes).map(function (n) { return document.adoptNode(n); });
      body.replaceChildren.apply(body, nodes);
      mount(body);
      restoreView(view);
    }

    /* ---- notices ---------------------------------------------------- */

    function noticeHost() {
      let host = document.querySelector(".pv-notices");
      if (!host) {
        host = el("div", { class: "pv-notices", role: "status", "aria-live": "polite" });
        const sheet = document.querySelector(".pv-sheet");
        const bar = document.querySelector(".pv-topbar");
        if (bar && bar.parentNode) bar.parentNode.insertBefore(host, bar.nextSibling);
        else if (sheet) sheet.insertBefore(host, sheet.firstChild);
        else document.body.insertBefore(host, document.body.firstChild);
      }
      return host;
    }

    /* One notice per kind, replaced in place; `null` removes it. The text
       survives a body swap because `mount` calls `renderNotices`. */
    function notice(kind, text, opts) {
      const o = opts || {};
      if (text === null) delete S.ui["notice:" + kind];
      else S.ui["notice:" + kind] = { text: text, dismiss: !!o.dismiss, action: o.action || null, onAction: o.onAction || null, title: o.title || null };
      renderNotices();
    }

    /* One component for everything the page says on its own. Each kind has
       a kicker (who is speaking) and a state for the mark. */
    const NOTICE_KINDS = {
      sent: { kicker: "Review sent", mark: "" },
      revision: { kicker: "New revision", mark: "" },
      nudge: { kicker: "The agent", mark: "" },
      noagent: { kicker: "No agent", mark: "off" },
      stopping: { kicker: "Server stopping", mark: "off" },
      gone: { kicker: "Server gone", mark: "off" },
      lost: { kicker: "Signed out", mark: "off" },
    };

    function noticeNode(kind, n) {
      const spec = NOTICE_KINDS[kind];
      const node = el("div", { class: "pv-notice", dataset: { kind: kind }, title: n.title });
      node.appendChild(agentMark(spec.mark));
      /* The kicker sits beside the text, not inside it, so the text is
         only ever what was said. */
      const text = el("span", { class: "pv-notice-text" });
      text.appendChild(richText(n.text));
      node.appendChild(el("span", { class: "pv-notice-body" },
        el("span", { class: "pv-notice-kicker", text: spec.kicker }), text));
      const actions = el("span", { class: "pv-notice-actions" });
      if (n.action) actions.appendChild(el("button", { type: "button", class: "pv-btn is-quiet pv-notice-action", text: n.action, onclick: n.onAction }));
      if (n.dismiss) actions.appendChild(el("button", { type: "button", class: "pv-btn is-quiet pv-notice-dismiss", text: "Dismiss", "aria-label": "Dismiss", onclick: function () { notice(kind, null); } }));
      if (actions.childNodes.length) node.appendChild(actions);
      return node;
    }

    function renderNotices() {
      const host = noticeHost();
      host.replaceChildren();
      ["sent", "revision", "nudge", "noagent", "stopping", "gone", "lost"].forEach(function (kind) {
        let n = S.ui["notice:" + kind];
        /* Derived, not stored: a question is waiting and nobody holds the
           lease. It goes the moment an agent attaches. */
        if (kind === "noagent") {
          n = S.connected && !S.state.presence && Object.keys(S.pending).length
            ? { text: "No agent is attached. Your question waits for one." }
            : null;
        }
        if (!n) return;
        host.appendChild(noticeNode(kind, n));
      });
    }

    /* Spec 6.6: what was sent, in the reviewer's terms, where they will see
       it. Without it a Send review that worked looked like one that did
       nothing, and got clicked four times. */
    function sentNotice() {
      const threads = S.state.threads.filter(function (t) { return t.status !== "unanchored"; });
      const answers = Object.keys(S.state.answers).filter(function (q) { return S.state.answers[q]; }).length;
      const parts = [];
      parts.push(threads.length + (threads.length === 1 ? " comment" : " comments"));
      parts.push(answers + (answers === 1 ? " answer" : " answers"));
      const tasks = S.root ? S.root.querySelectorAll('.task[data-plan-ref^="task:"]').length : 0;
      let k = 0;
      S.state.reviewed.forEach(function (r) { if (r.indexOf("task:") === 0) k++; });
      parts.push(k + " of " + tasks + " tasks reviewed");
      notice("sent",
        (S.state.verdict === "approve" ? "Approval sent" : "Review sent") + " for revision " + S.state.revision + ": "
          + parts.join(", ") + ". The agent has it."
          + (S.state.presence ? "" : " No agent is attached; it will be delivered when one is."),
        { dismiss: true });
    }

    function revisionNotice(e, applied) {
      const d = e.data || {};
      const resolved = applied.filter(function (x) { return x.type === "thread.resolved"; });
      const changed = resolved.filter(function (x) { return x.data && x.data.status === "changed"; }).length;
      const declined = resolved.length - changed;
      let text = "Revision " + S.state.revision + " pushed: " + (d.summary || "updated") + ".";
      if (resolved.length) {
        const parts = [];
        if (changed) parts.push(changed + " addressed");
        if (declined) parts.push(declined + " declined");
        text += " " + parts.join(", ") + ".";
      }
      const orphaned = core.unanchored(S.state).length;
      if (orphaned) text += " " + orphaned + (orphaned === 1 ? " thread lost its element." : " threads lost their elements.");
      notice("revision", text, { dismiss: true, title: S.previousTitle ? "Previously: " + S.previousTitle : null });
      panelEvent("revision", "revision " + S.state.revision + " pushed");
    }

    /* ---- presence ----------------------------------------------------- */

    function presenceLabel() {
      if (S.lost) return { mode: "off", text: "signed out" };
      if (S.gone) return { mode: "off", text: "server gone" };
      if (!S.connected) return { mode: "off", text: "reconnecting" };
      const p = S.state.presence;
      if (!p) return { mode: "off", text: "no agent" };
      if (Object.keys(S.pending).length) return { mode: "working", text: "agent working", agent: p.agent };
      return { mode: p.mode, text: "agent " + (p.mode === "live" ? "live" : "waiting"), agent: p.agent };
    }

    /* The mark's state for a presence mode: live is the mark at rest. */
    function markStateFor(mode) {
      return mode === "live" ? "" : mode === "working" ? "working" : mode === "waiting" ? "waiting" : "off";
    }

    function renderPresence() {
      const pill = document.querySelector(".pv-presence");
      if (!pill) return;
      const l = presenceLabel();
      pill.querySelector(".pv-presence-text").textContent = l.text;
      pill.setAttribute("data-mode", l.mode);
      setMarkState(pill.querySelector(".ag-mark"), markStateFor(l.mode));
      pill.title = l.agent ? l.agent + " holds the lease" : "";
      const hint = document.querySelector(".pv-chat-hint");
      if (hint) hint.textContent = presenceLine("message");
      document.querySelectorAll(".composer-presence").forEach(function (n) {
        n.textContent = presenceLine("question");
      });
    }

    /* What a question composer says about who will hear it. The hint and
       the banner promise the agent hears a question now; this is where
       the promise is qualified when no agent holds the lease. */
    function presenceLine(what) {
      return S.state.presence
        ? "The agent hears this at once."
        : "No agent is attached. Your " + what + " will wait for one.";
    }

    function mountPresence(root) {
      const host = root.querySelector(".pv-topbar-right");
      if (!host || host.querySelector(".pv-presence")) return;
      host.insertBefore(el("span", { class: "pv-presence", dataset: { mode: "off" } },
        agentMark("off"), el("span", { class: "pv-presence-text", text: "no agent" })), host.firstChild);
    }

    /* ---- threads ----------------------------------------------------- */

    function actorLabel(actor) {
      return actor === "agent" ? "agent" : actor === "server" ? "server" : "you";
    }

    /* Circle for the machine, square for the person, same weight. */
    function avatar(actor) {
      if (actor === "agent") return el("span", { class: "pv-avatar", title: "agent" }, agentMark(""));
      return el("span", { class: "pv-avatar is-you", title: actorLabel(actor) });
    }

    function whenLabel(ts) {
      const d = ts ? new Date(ts) : null;
      if (!d || isNaN(d.getTime())) return "";
      return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
    }

    function messageNode(m, i) {
      return el("div", { class: "thread-msg", dataset: { actor: m.actor, index: String(i) }, title: m.ts },
        avatar(m.actor),
        el("div", { class: "thread-msg-body" },
          el("span", { class: "thread-actor", text: actorLabel(m.actor) }),
          el("p", { class: "thread-text", text: m.text })));
    }

    function renderThread(node, t) {
      node.setAttribute("data-status", t.status);
      node.setAttribute("data-kind", t.asked ? "question" : "comment");
      node.classList.toggle("is-asked", !!t.asked);
      node.classList.toggle("is-blocking", !!t.blocking);
      node.querySelector(".thread-label").textContent = t.asked ? "Question" : "Comment";
      node.querySelector(".thread-status").textContent = t.status;
      node.querySelector(".thread-status").className = "thread-status pv-chip pv-chip-" + t.status;
      node.querySelector(".thread-blocking").hidden = !t.blocking;
      const target = node.querySelector(".thread-target");
      target.hidden = t.status !== "unanchored";
      target.textContent = "was on " + t.target;
      const first = t.messages[0];
      const when = node.querySelector(".thread-when");
      when.textContent = first ? whenLabel(first.ts) : "";
      when.title = first ? first.ts : "";
      /* A resolved thread's closing note is marked by the server as a
         note, not a turn: it is shown as the resolution with the verdict
         chip, wherever it sits, and a reply after it stays a reply. */
      const resolved = t.status === "changed" || t.status === "declined";
      let noteAt = -1;
      t.messages.forEach(function (m, i) { if (m.note && resolved) noteAt = i; });
      const last = t.messages[noteAt];
      const msgs = node.querySelector(".thread-msgs");
      msgs.replaceChildren();
      t.messages.forEach(function (m, i) {
        if (i !== noteAt) msgs.appendChild(messageNode(m, i));
      });
      /* From the question until the answer: the mark at work, or what it
         is waiting for. Not a message, so counts of messages stay true. */
      const pending = pendingLabel(t.id);
      if (pending) {
        msgs.appendChild(el("div", { class: "thread-working", dataset: { actor: "agent" } },
          el("span", { class: "pv-avatar" }, agentMark(pending.state)),
          el("div", { class: "thread-msg-body" },
            el("span", { class: "thread-actor", text: "agent" }),
            el("p", { class: "thread-text", text: pending.text }))));
      }
      const resolution = node.querySelector(".thread-resolution");
      resolution.hidden = noteAt < 0;
      if (noteAt >= 0) {
        resolution.replaceChildren(
          avatar("agent"),
          el("div", { class: "thread-msg-body" },
            el("span", { class: "thread-actor" }, document.createTextNode("agent"),
              el("span", { class: "pv-chip pv-chip-" + t.status, text: t.status })),
            el("p", { class: "thread-text", text: last.text })));
      }
      const open = t.status === "open" || t.status === "unanchored";
      node.querySelector(".thread-edit").hidden = !open;
      node.querySelector(".thread-delete").hidden = !open;
      /* On a question thread every follow-up is for the agent, and a Reply
         there would wait for the sent review: the trap this thread kind
         exists to remove. The persistent input is the one way to write in
         it, so the Ask button goes too. */
      node.querySelector(".thread-reply").hidden = !!t.asked;
      node.querySelector(".thread-ask").hidden = !!t.asked;
      /* A question thread is not rendered here at all: it lives in the
         conversation panel. */
    }

    function threadNode(t) {
      const node = el("div", { class: "thread", dataset: { thread: t.id } });
      /* No id in the head: `c-3` is the agent's handle for the thread, kept
         on the node as data-thread and shown to nobody. */
      node.appendChild(el("div", { class: "thread-head" },
        el("span", { class: "thread-label", text: "Comment" }),
        el("span", { class: "thread-status pv-chip", text: t.status }),
        el("span", { class: "thread-blocking", text: "blocks approval" }),
        el("span", { class: "thread-target", hidden: true }),
        el("span", { class: "thread-when" })));
      node.appendChild(el("div", { class: "thread-msgs" }));
      node.appendChild(el("div", { class: "thread-resolution", hidden: true }));
      const actions = el("div", { class: "thread-actions" });
      actions.appendChild(el("button", { type: "button", class: "pv-btn is-quiet thread-reply", text: "Reply", onclick: function () {
        openComposer({ kind: "reply", thread: t.id, ref: t.target });
      } }));
      actions.appendChild(el("button", { type: "button", class: "pv-btn is-agent thread-ask", text: "Ask the agent", onclick: function () {
        aimPanel(t.target, t.quote, t.id);
      } }));
      actions.appendChild(el("button", { type: "button", class: "pv-btn is-quiet thread-edit", text: "Edit", onclick: function () {
        const current = S.state.threads.find(function (x) { return x.id === t.id; });
        openComposer({ kind: "edit", thread: t.id, ref: t.target, text: current && current.messages[0] ? current.messages[0].text : "" });
      } }));
      let armed = null;
      actions.appendChild(el("button", { type: "button", class: "pv-btn is-quiet thread-delete", text: "Delete", onclick: function (ev) {
        const btn = ev.currentTarget;
        if (armed) {
          clearTimeout(armed);
          armed = null;
          btn.textContent = "Delete";
          send({ cmd: "thread.delete", thread: t.id }).catch(function (e) { failed(btn, e); });
          return;
        }
        btn.textContent = "Confirm delete";
        armed = setTimeout(function () { armed = null; btn.textContent = "Delete"; }, 4000);
      } }));
      node.appendChild(actions);
      node.appendChild(el("div", { class: "thread-composers" }));
      return node;
    }

    /* Keyed: an existing thread element is updated in place so a composer
       open inside it survives the render. */
    function renderThreadsIn(host, threads) {
      const keep = {};
      threads.forEach(function (t) {
        let node = host.querySelector('.thread[data-thread="' + t.id + '"]');
        if (!node) { node = threadNode(t); host.appendChild(node); }
        renderThread(node, t);
        keep[t.id] = true;
      });
      Array.from(host.querySelectorAll(".thread")).forEach(function (node) {
        if (!keep[node.getAttribute("data-thread")]) node.remove();
      });
    }

    function renderThreads() {
      const root = S.root;
      if (!root) return;
      root.querySelectorAll(".pv-threads[data-threads-for]").forEach(function (host) {
        const ref = host.getAttribute("data-threads-for");
        if (ref) renderThreadsIn(host, core.threadsOn(S.state, ref).filter(function (t) { return !t.asked; }));
      });
    }

    /* ---- answers and reviewed marks ------------------------------------- */

    function renderAnswers() {
      const root = S.root;
      if (!root) return;
      root.querySelectorAll(".pv-answer").forEach(function (box) {
        const q = box.getAttribute("data-answer-for");
        const text = S.state.answers[q];
        box.hidden = !text;
        box.querySelector(".pv-answer-text").textContent = text || "";
      });
    }

    function renderReviewed() {
      const root = S.root;
      if (!root) return;
      root.querySelectorAll(".reviewed-toggle").forEach(function (label) {
        const ref = label.getAttribute("data-reviewed-for");
        const box = label.querySelector("input");
        /* A mark whose send is in flight shows what the reviewer chose,
           across a body swap too; the last reply settles it. */
        const pending = S.pendingMarks[ref];
        const on = pending ? pending.on : S.state.reviewed.indexOf(ref) >= 0;
        box.checked = on;
        label.querySelector(".reviewed-toggle-text").textContent = on ? "Reviewed" : "Mark reviewed";
        const container = label.closest("[data-plan-ref]");
        if (container) container.classList.toggle("is-reviewed", on);
      });
    }

    /* ---- the bar and the chat ---------------------------------------- */

    function clock(d) {
      return String(d.getHours()).padStart(2, "0") + ":" + String(d.getMinutes()).padStart(2, "0");
    }

    /* Whether closing the page now would leave something the agent has not
       been sent: threads, answers, or marks on an unsent review. */
    function unsentWork() {
      if (S.state.submitted) return false;
      return S.state.threads.some(function (t) { return t.status !== "unanchored"; })
        || Object.keys(S.state.answers).some(function (q) { return S.state.answers[q]; })
        || S.state.reviewed.length > 0;
    }

    function renderBar() {
      const bar = document.querySelector(".pv-panel-foot");
      if (!bar) return;
      const threads = S.state.threads.filter(function (t) { return t.status !== "unanchored"; });
      const open = threads.filter(function (t) { return t.status === "open"; });
      const blocking = open.filter(function (t) { return t.blocking; }).length;
      const taskEls = S.root ? S.root.querySelectorAll('.task[data-plan-ref^="task:"]') : [];
      let k = 0;
      taskEls.forEach(function (t) { if (S.state.reviewed.indexOf(t.getAttribute("data-plan-ref")) >= 0) k++; });
      bar.querySelector(".feedback-bar-count").textContent = threads.length + (threads.length === 1 ? " thread" : " threads");
      bar.querySelector(".feedback-bar-reviewed").textContent = k + "/" + taskEls.length + " reviewed";
      bar.querySelector(".feedback-bar-blocking").textContent = blocking ? blocking + " blocking" : "";
      /* The state line: everything written is on the server the moment it
         is accepted, and the line says so. Leaving is not losing. */
      const state = bar.querySelector(".feedback-bar-state");
      const leaving = S.ui.leftAt && Date.now() - S.ui.leftAt < 4000;
      state.classList.toggle("is-saving", S.inflight > 0);
      state.querySelector(".feedback-bar-state-text").textContent = S.inflight > 0
        ? "Saving\u2026"
        : leaving
          ? "Saved. The agent sees your notes when you send them."
          : "Saved \u00b7 rev " + S.state.revision;
      const when = S.ui.sentAt ? " \u00b7 " + clock(S.ui.sentAt) : "";
      bar.querySelector(".feedback-bar-sent").textContent = S.state.submitted ? "review sent \u00b7 rev " + S.state.revision + when : "";
      bar.classList.toggle("is-sent", !!S.state.submitted);
      /* Two verdicts, one group; the one that was sent is the filled control. */
      const request = bar.querySelector(".feedback-bar-send");
      const approve = bar.querySelector(".feedback-bar-approve");
      request.disabled = S.lost || S.submitting;
      approve.disabled = S.lost || S.submitting;
      request.classList.toggle("is-filled", !!S.state.submitted && S.state.verdict !== "approve");
      approve.classList.toggle("is-filled", !!S.state.submitted && S.state.verdict === "approve");
    }

    function submitReview(verdict) {
      if (S.submitting) return;
      S.submitting = true;
      renderBar();
      const button = function () {
        return document.querySelector(verdict === "approve" ? ".feedback-bar-approve" : ".feedback-bar-send");
      };
      send({ cmd: "review.submit", verdict: verdict, base_revision: S.state.revision })
        .then(function () {
          S.submitting = false;
          S.ui.sentAt = new Date();
          sentNotice();
          renderBar();
          const btn = button();
          if (btn) {
            cleared(btn);
            /* One short pulse on the button the reviewer is looking at. */
            btn.classList.add("is-sent");
            setTimeout(function () { btn.classList.remove("is-sent"); }, 2000);
          }
        })
        .catch(function (e) {
          S.submitting = false;
          renderBar();
          failed(button(), e);
        });
    }

    /* Leaving with unsent work: say once that nothing is lost. No dialog,
       nothing blocks; the line in the bar changes for a few seconds. */
    function wireLeaving() {
      if (S.ui.leaveWired) return;
      S.ui.leaveWired = true;
      const note = function () {
        if (!unsentWork()) return;
        S.ui.leftAt = Date.now();
        renderBar();
        window.setTimeout(renderBar, 4200);
      };
      document.addEventListener("visibilitychange", function () { if (document.visibilityState === "hidden") note(); });
      window.addEventListener("pagehide", note);
    }

    function renderChat() {
      const log = document.querySelector(".pv-chat-log");
      if (!log) return;
      log.replaceChildren();
      const entries = conversationEntries();
      const lastOf = {};
      entries.forEach(function (e, i) { if (!e.event) lastOf[e.key] = i; });
      entries.forEach(function (e, i) {
        if (e.event) {
          if (e.kind === "nudge") {
            const nudge = el("div", { class: "pv-panel-nudge" }, el("span", { class: "pv-notice-kicker", text: "The agent" }));
            nudge.appendChild(richText(e.text));
            log.appendChild(nudge);
          } else {
            log.appendChild(el("div", { class: "pv-panel-event is-revision", text: e.text }));
          }
          return;
        }
        const actorRow = el("span", { class: "thread-actor" }, document.createTextNode(actorLabel(e.actor)));
        if (e.ref) actorRow.appendChild(chipFor(e.ref));
        if (e.note) actorRow.appendChild(el("span", { class: "pv-ctx is-resolution", text: e.status }));
        actorRow.appendChild(el("span", { class: "thread-when", text: whenLabel(e.ts) }));
        const attrs = { class: "pv-panel-msg" + (e.key === "page" ? " pv-chat-msg" : ""), dataset: { actor: e.actor }, title: e.ts };
        if (e.key !== "page") attrs.dataset.thread = e.key;
        log.appendChild(el("div", attrs, avatar(e.actor),
          el("div", { class: "pv-panel-msg-body" }, actorRow, el("p", { class: "thread-text", text: e.text }))));
        /* From the question until the answer: the mark at work, right
           after the last message of that thread. */
        if (lastOf[e.key] === i) {
          const pending = pendingLabel(e.key);
          if (pending) {
            const wattrs = { class: "pv-panel-msg thread-working" + (e.key === "page" ? " pv-chat-working" : ""), dataset: { actor: "agent" } };
            if (e.key !== "page") wattrs.dataset.thread = e.key;
            log.appendChild(el("div", wattrs,
              el("span", { class: "pv-avatar" }, agentMark(pending.state)),
              el("div", { class: "pv-panel-msg-body" },
                el("span", { class: "thread-actor", text: "agent" }),
                el("p", { class: "thread-text", text: pending.text }))));
          }
        }
      });
      log.hidden = entries.length === 0;
      if (S.ui.panelFollow !== false) log.scrollTop = log.scrollHeight;
      renderPanelComposer();
      const dock = document.querySelector(".pv-dock");
      if (dock) dock.classList.toggle("is-hidden", !S.ui.chatOpen);
      const handle = document.querySelector(".pv-panel-handle");
      if (handle) handle.hidden = !!S.ui.chatOpen;
      document.querySelectorAll(".pv-panel-count").forEach(function (n) { n.textContent = String(conversationCount()); });
    }

    /* An open panel always has somewhere to write. A body swap rebuilds
       the panel and restores composers from drafts only; a composer nobody
       has typed into has no draft, so it is opened again here — after the
       drafts, so a stored one is not joined by an empty twin. */
    function ensureChatComposer() {
      /* The panel's composer is built with the panel; nothing to open. */
    }

    /* One line, until the reviewer says they have read it: the difference
       between a comment and a question is the one thing the page cannot
       show by layout alone. Stored only on dismissal, so a page nobody
       dismissed writes nothing. */
    /* The ask control carries its label until the reviewer has asked once
       on this browser; after that the mark alone is the control, with the
       label in its tooltip and in the bar. Stored on the first ask. */
    const ASKED_KEY = "artefacto.asked";
    function askedOnce() {
      try { return window.localStorage.getItem(ASKED_KEY) === "1"; } catch (e) { return false; }
    }
    function markAsked() {
      try { window.localStorage.setItem(ASKED_KEY, "1"); } catch (e) { /* an opaque origin; the label stays */ }
      document.querySelectorAll("[data-plan-ref] .ask-btn.is-labelled").forEach(function (b) { b.classList.remove("is-labelled"); });
    }

    /* The ask control on an element that already has a question is tinted,
       and a click there goes to that thread's input rather than opening a
       second question. */
    function askedThreadOn(ref) {
      return S.state.threads.find(function (t) {
        return t.asked && t.target === ref && (t.status === "open" || t.status === "unanchored");
      });
    }
    function renderAskMarks() {
      if (!S.root) return;
      S.root.querySelectorAll(".ask-btn[data-ask-for]").forEach(function (b) {
        const ref = b.getAttribute("data-ask-for");
        const threads = S.state.threads.filter(function (t) { return t.asked && t.target === ref; });
        const count = threads.reduce(function (n, t) { return n + t.messages.length; }, 0);
        b.classList.toggle("has-thread", !!askedThreadOn(ref));
        let badge = b.querySelector(".ask-count");
        if (count && !badge) { badge = el("span", { class: "ask-count" }); b.appendChild(badge); }
        if (badge) { if (count) badge.textContent = String(count); else badge.remove(); }
        /* The element with a discussion: a spine, and a one-line preview of
           the last message after the controls (not on criterion rows, which
           have no room). */
        const element = b.closest("[data-plan-ref]");
        if (element) element.classList.toggle("is-discussed", count > 0);
        const row = b.closest(".el-actions");
        if (!row || row.closest(".acceptance")) return;
        let preview = row.parentNode.querySelector(":scope > .ask-preview");
        const last = threads.length ? threads[threads.length - 1] : null;
        const msg = last && last.messages.length ? last.messages[last.messages.length - 1] : null;
        if (!msg) { if (preview) preview.remove(); return; }
        if (!preview) {
          preview = el("button", { type: "button", class: "ask-preview", title: "Open the conversation" });
          preview.addEventListener("click", function () {
            if (!S.ui.chatOpen) setPanelOpen(true);
            const target = document.querySelector('.pv-panel-msg[data-thread="' + last.id + '"]');
            if (target) target.scrollIntoView({ block: "nearest" });
          });
          row.insertAdjacentElement("afterend", preview);
        }
        preview.replaceChildren(avatar(msg.actor), el("span", { class: "ask-preview-text", text: msg.text }));
      });
    }

    const HINT_KEY = "artefacto.hint.ask";
    function hintDismissed() {
      try { return window.localStorage.getItem(HINT_KEY) === "1"; } catch (e) { return false; }
    }

    /* The conversation panel: one place for everything said to and by the
       agent, docked beside the sheet. Closed by default; a floating handle
       (the mark with the message count) and a top-bar link open it, the X
       in its head hides it, and the choice is kept per browser. Its foot
       holds the state line and the two verdicts, so the served page has no
       bottom bar. */
    function panelStored() {
      try { return window.localStorage.getItem(PANEL_KEY) === "open"; } catch (e) { return false; }
    }

    function setPanelOpen(open) {
      S.ui.chatOpen = open;
      /* The reviewer has seen the panel; a draft in it no longer opens it
         on their behalf. */
      S.ui.chatDraftShown = true;
      try { window.localStorage.setItem(PANEL_KEY, open ? "open" : "closed"); } catch (e) { /* best effort */ }
      renderChat();
      if (open) {
        const ta = document.querySelector(".pv-panel-composer textarea");
        if (ta) ta.focus({ preventScroll: true });
      }
    }

    /* Everything said to and by the agent, in time order: the page-level
       chat, every question thread's messages, and the panel's own events
       (a revision, a nudge). Comment threads stay on their elements. */
    function conversationEntries() {
      const out = [];
      S.state.chat.forEach(function (m, i) {
        out.push({ ts: m.ts, actor: m.actor, text: m.text, key: "page", index: i });
      });
      S.state.threads.forEach(function (t) {
        if (!t.asked) return;
        t.messages.forEach(function (m, i) {
          out.push({ ts: m.ts, actor: m.actor, text: m.text, key: t.id, ref: t.target, index: i,
            note: !!m.note, status: t.status, unanchored: t.status === "unanchored" });
        });
      });
      S.ui.panelEvents.forEach(function (ev) { out.push({ event: true, kind: ev.kind, text: ev.text, ts: ev.ts }); });
      /* Ordered by time at one-second grain: the server stamps seconds,
         the page's own events carry milliseconds, and an answer must not
         sort ahead of its question because its stamp is coarser. Ties keep
         insertion order, which is each thread's own order. */
      out.sort(function (a, b) {
        return Math.floor((Date.parse(a.ts) || 0) / 1000) - Math.floor((Date.parse(b.ts) || 0) / 1000);
      });
      return out;
    }

    /* What the handle and the top-bar link count: every message in the
       conversation. */
    function conversationCount() {
      return conversationEntries().filter(function (e) { return !e.event; }).length;
    }

    function panelEvent(kind, text) {
      S.ui.panelEvents.push({ kind: kind, text: text, ts: nowIso() });
      renderChat();
    }

    /* The context chip: which element a message is about. A link into the
       page: it scrolls the element into view and flashes it. */
    function chipFor(ref) {
      const target = ref ? findRef(S.root, ref) : null;
      const title = target ? elementQuote(target) : ref || "";
      const chip = el("button", { type: "button", class: "pv-ctx", title: ref || "" },
        el("span", { class: "pv-ctx-kind", text: ref ? refKind(ref) : "plan" }),
        el("span", { class: "pv-ctx-title", text: title.slice(0, 48) }));
      chip.addEventListener("click", function () { jumpTo(ref); });
      return chip;
    }

    function jumpTo(ref) {
      const target = findRef(S.root, ref);
      if (!target) return;
      const details = target.closest("details.phase");
      if (details && details !== target && phaseIsShut(details)) setPhaseOpen(details, true, false);
      if (target.tagName === "DETAILS" && phaseIsShut(target)) setPhaseOpen(target, true, false);
      target.scrollIntoView({ block: "center" });
      target.classList.remove("is-jumped");
      void target.offsetWidth;
      target.classList.add("is-jumped");
    }

    /* Aim the panel's composer at an element (or at a thread on it), open
       the panel, and put the cursor in the input. */
    function aimPanel(ref, quote, thread) {
      const existing = ref && !thread ? askedThreadOn(ref) : null;
      S.ui.panel.ref = ref || null;
      S.ui.panel.quote = quote || null;
      S.ui.panel.thread = thread || (existing ? existing.id : null);
      savePanelDraft();
      if (!S.ui.chatOpen) setPanelOpen(true);
      renderPanelComposer();
      const ta = document.querySelector(".pv-panel-composer textarea");
      if (ta) ta.focus({ preventScroll: true });
    }

    function panelComposer() {
      const composer = el("div", { class: "pv-panel-composer" });
      const targetLine = el("div", { class: "pv-panel-target", hidden: true },
        el("span", { class: "pv-panel-target-label", text: "asking about" }),
        el("span", { class: "pv-panel-target-chip" }),
        el("button", { type: "button", class: "pv-btn is-quiet pv-panel-target-clear", text: "the whole plan instead",
          onclick: function () {
            S.ui.panel.ref = null; S.ui.panel.thread = null; S.ui.panel.quote = null;
            savePanelDraft();
            renderPanelComposer();
          } }));
      const box = el("div", { class: "thread-composer" });
      const ta = el("textarea", { rows: "1", placeholder: "Ask the agent\u2026", "aria-label": "Ask the agent" });
      const hint = el("span", { class: "thread-composer-hint" });
      const sendBtn = el("button", { type: "button", class: "pv-btn is-agent thread-composer-send composer-send", text: "Send" });
      box.appendChild(agentMark(""));
      box.appendChild(ta);
      box.appendChild(hint);
      box.appendChild(sendBtn);
      ta.value = S.ui.panel.text || "";
      const grow = function () { ta.rows = Math.min(6, ta.value.split("\n").length); };
      grow();
      ta.addEventListener("input", function () { S.ui.panel.text = ta.value; savePanelDraft(); grow(); });
      ta.addEventListener("keydown", function (ev) {
        if (ev.key === "Enter" && !ev.shiftKey) { ev.preventDefault(); sendPanel(); }
      });
      sendBtn.addEventListener("click", sendPanel);
      composer.appendChild(targetLine);
      composer.appendChild(box);
      return composer;
    }

    function renderPanelComposer() {
      const composer = document.querySelector(".pv-panel-composer");
      if (!composer) return;
      const p = S.ui.panel;
      /* A target thread that is gone (deleted, or a snapshot without it)
         is dropped; the text stays, aimed at the plan as a whole. */
      if (p.thread && !S.syncing && S.state.lastSeq > 0 && !S.state.threads.some(function (t) { return t.id === p.thread; })) {
        p.thread = null;
        p.ref = null;
        p.quote = null;
        savePanelDraft();
      }
      const line = composer.querySelector(".pv-panel-target");
      const aimed = !!(p.ref || p.thread);
      line.hidden = !aimed;
      if (aimed) {
        const thread = p.thread ? S.state.threads.find(function (t) { return t.id === p.thread; }) : null;
        const ref = p.ref || (thread ? thread.target : null);
        const chipHost = line.querySelector(".pv-panel-target-chip");
        chipHost.replaceChildren(chipFor(ref));
      }
      composer.querySelector(".thread-composer-hint").textContent = S.state.presence ? "Enter to send" : "waits for an agent";
      setMarkState(composer.querySelector(".thread-composer .ag-mark"), S.state.presence ? "" : "off");
      composer.querySelector(".thread-composer-send").disabled = !!S.ui.panelSending;
    }

    /* One send at a time, keyed by the panel, not by its node: a body swap
       while the send is in flight rebuilds the panel, and the reply must
       clear and re-enable the live input. */
    function sendPanel() {
      const ta = document.querySelector(".pv-panel-composer textarea");
      const text = ta ? ta.value.trim() : "";
      if (!text || S.ui.panelSending) return;
      const p = S.ui.panel;
      const cmd = { cmd: "chat.send", text: text, opened_revision: S.state.revision };
      const existing = p.ref && !p.thread ? askedThreadOn(p.ref) : null;
      if (p.thread) {
        cmd.thread = p.thread;
      } else if (existing) {
        cmd.thread = existing.id;
      } else if (p.ref) {
        const target = findRef(S.root, p.ref);
        cmd.ref = p.ref;
        cmd.quote = p.quote || (target ? elementQuote(target) : "");
      }
      S.ui.panelSending = true;
      renderPanelComposer();
      send(cmd)
        .then(function () {
          S.ui.panelSending = false;
          S.ui.panel = { text: "", ref: null, thread: null, quote: null };
          savePanelDraft();
          markAsked();
          renderPanelComposer();
          const box = document.querySelector(".pv-panel-composer textarea");
          if (box) { box.value = ""; box.rows = 1; box.focus({ preventScroll: true }); }
          const btn = document.querySelector(".pv-panel-composer .thread-composer-send");
          if (btn) cleared(btn);
        })
        .catch(function (e) {
          S.ui.panelSending = false;
          renderPanelComposer();
          const btn = document.querySelector(".pv-panel-composer .thread-composer-send");
          if (btn) failed(btn, e);
        });
    }

    function mountPanel(root) {
      if (root.querySelector(".pv-dock")) return;
      const sheet = root.querySelector(".pv-sheet");
      if (!sheet) return;
      const shell = el("div", { class: "pv-shell" });
      sheet.parentNode.insertBefore(shell, sheet);
      shell.appendChild(sheet);

      const panel = el("div", { class: "pv-panel pv-chat" });
      panel.appendChild(el("div", { class: "pv-panel-head" },
        el("span", { class: "pv-panel-title", text: "Conversation" }),
        el("span", { class: "pv-chat-hint" }),
        el("button", { type: "button", class: "pv-btn is-quiet pv-panel-hide", "aria-label": "Hide conversation",
          title: "Hide conversation", text: "\u00d7", onclick: function () { setPanelOpen(false); } })));
      if (!hintDismissed()) {
        const hint = el("div", { class: "feedback-bar-hint" },
          agentMark(""),
          el("span", { class: "feedback-bar-hint-text", text: "Comments wait for your review; a question here reaches the agent now." }),
          el("button", { type: "button", class: "pv-btn is-quiet feedback-bar-hint-dismiss", text: "Got it", onclick: function () {
            try { window.localStorage.setItem(HINT_KEY, "1"); } catch (e) { /* an opaque origin; the hint returns next time */ }
            hint.remove();
          } }));
        panel.appendChild(hint);
      }
      const log = el("div", { class: "pv-panel-log pv-chat-log" });
      /* Follow the newest message unless the reviewer has scrolled up. */
      log.addEventListener("scroll", function () {
        S.ui.panelFollow = log.scrollTop + log.clientHeight >= log.scrollHeight - 8;
      });
      panel.appendChild(log);
      panel.appendChild(panelComposer());
      const foot = el("div", { class: "pv-panel-foot" });
      foot.appendChild(el("span", { class: "feedback-bar-state" },
        el("span", { class: "pv-dot" }), el("span", { class: "feedback-bar-state-text" })));
      foot.appendChild(el("span", { class: "feedback-bar-counts" },
        el("span", { class: "feedback-bar-count" }),
        el("span", { class: "feedback-bar-reviewed" }),
        el("span", { class: "feedback-bar-blocking is-alarm" })));
      foot.appendChild(el("span", { class: "feedback-bar-sent" }));
      /* Two verdicts on the plan, one group. Request changes is enabled with
         nothing written: it is a verdict on the plan, not on the comments. */
      const verdict = el("div", { class: "feedback-bar-verdict", role: "group", "aria-label": "Your verdict" });
      verdict.appendChild(el("button", { type: "button", class: "pv-btn is-alarm feedback-bar-send", text: "Request changes",
        onclick: function () { submitReview("request_changes"); } }));
      verdict.appendChild(el("button", { type: "button", class: "pv-btn feedback-bar-approve", text: "Approve",
        onclick: function () { submitReview("approve"); } }));
      foot.appendChild(verdict);
      panel.appendChild(foot);

      const dock = el("aside", { class: "pv-dock", "aria-label": "Conversation with the agent" });
      dock.appendChild(panel);
      shell.appendChild(dock);

      /* The handle: always in reach, the mark with the count. It keeps the
         old bar button's class so a page that opened the chat that way
         still does. */
      root.appendChild(el("button", { type: "button", class: "pv-panel-handle feedback-bar-chat",
        "aria-label": "Open the conversation with the agent", title: "Conversation",
        onclick: function () { setPanelOpen(!S.ui.chatOpen); } },
        agentMark(""), el("span", { class: "pv-panel-count" })));
      const right = root.querySelector(".pv-topbar-right");
      if (right && !right.querySelector(".pv-panel-link")) {
        right.insertBefore(el("button", { type: "button", class: "pv-panel-link",
          onclick: function () { setPanelOpen(true); } },
          "Conversation ", el("span", { class: "pv-panel-count" })), right.firstChild);
      }
      wireLeaving();
    }

    /* ---- the recovery panel -------------------------------------------

       Spec 4.3: a thread whose element is gone, and a draft whose target
       is gone, are listed at the top -- never silently dropped. The thread
       host is keyed and kept, so a reply composer opened on an orphaned
       thread survives the next render. */
    function renderRecovery() {
      let panel = document.querySelector(".pv-recovery");
      const orphans = core.unanchored(S.state).filter(function (t) { return !t.asked; });
      const drafts = orphanedDrafts();
      if (!orphans.length && !drafts.length) { if (panel) panel.remove(); return; }
      if (!panel) {
        panel = el("section", { class: "pv-recovery" });
        panel.appendChild(el("h2", { class: "pv-recovery-title", text: "Needs attention" }));
        panel.appendChild(el("p", { class: "pv-recovery-lead" }));
        panel.appendChild(el("div", { class: "pv-threads pv-threads-orphaned" }));
        panel.appendChild(el("div", { class: "pv-recovery-drafts" }));
        const host = noticeHost();
        host.parentNode.insertBefore(panel, host.nextSibling);
      }
      const lead = panel.querySelector(".pv-recovery-lead");
      lead.hidden = !orphans.length;
      lead.textContent = orphans.length === 1
        ? "This thread's element is no longer in the plan."
        : "These threads' elements are no longer in the plan.";
      renderThreadsIn(panel.querySelector(".pv-threads-orphaned"), orphans);
      const list = panel.querySelector(".pv-recovery-drafts");
      const key = drafts.map(function (d) { return d.id; }).join(" ");
      if (list.getAttribute("data-drafts") === key) return;
      list.setAttribute("data-drafts", key);
      list.replaceChildren();
      drafts.forEach(function (d) {
        const row = el("div", { class: "pv-orphan-draft", dataset: { composer: d.id } });
        row.appendChild(el("span", { class: "pv-orphan-draft-what", text: draftLabel(d) + (d.kind === "followup" ? " \u2014 its thread is gone" : " \u2014 its element is gone") }));
        row.appendChild(el("p", { class: "thread-text", text: d.text }));
        row.appendChild(el("button", { type: "button", class: "pv-btn is-quiet", text: "Discard", onclick: function () {
          dropDraft(d.id);
          renderRecovery();
        } }));
        list.appendChild(row);
      });
    }

    function draftLabel(d) {
      switch (d.kind) {
        case "comment": return "Comment on " + d.ref;
        case "answer": return "Answer to " + d.ref;
        case "reply": return "Reply on " + d.thread;
        case "ask": return "Question on " + (d.thread || d.ref);
        case "followup": return "Follow-up on a question";
        case "edit": return "Edit of " + d.thread;
        default: return "Message to the agent";
      }
    }

    /* A draft whose target no longer exists on the page. */
    function orphanedDrafts() {
      const map = loadDraftMap();
      const out = [];
      for (const id in map) {
        const d = map[id];
        if (!draftTarget(d)) out.push(d);
      }
      return out;
    }

    /* Where a draft's composer belongs now, or null. A thread that lost
       its element lives in the recovery panel, and a reply there is still
       a reply. */
    function draftTarget(d) {
      if (!S.root) return null;
      if (d.kind === "chat") return document.querySelector(".pv-chat-composers");
      if (d.kind === "comment" || d.kind === "answer" || (d.kind === "ask" && !d.thread)) {
        /* By ref, not by descent: a phase contains its tasks, and the
           first `.pv-composers` under a phase is its first task's. */
        return findByAttr(S.root, ".pv-composers", "data-composers-for", d.ref);
      }
      const t = S.state.threads.find(function (x) { return x.id === d.thread; });
      if (!t) return null;
      return document.querySelector('.thread[data-thread="' + d.thread + '"] .thread-composers');
    }

    /* ---- composers --------------------------------------------------- */

    /* Where an error line goes, and how it is attached. A toggle's line
       sits AFTER its label: text inside a <label> activates the control,
       so a line inside it would flip the mark when read. */
    function errorPlace(anchor) {
      const label = anchor.closest(".reviewed-toggle");
      if (label) return { host: label.parentNode || label, after: label };
      const host = anchor.closest(".composer, .feedback-bar, .thread") || anchor.parentNode;
      return { host: host, after: null };
    }

    function failed(anchor, e) {
      const place = errorPlace(anchor);
      let line = place.after
        ? (place.after.nextElementSibling && place.after.nextElementSibling.classList.contains("pv-error")
          ? place.after.nextElementSibling : null)
        : place.host.querySelector(":scope > .pv-error");
      if (!line) {
        line = el("span", { class: "pv-error", role: "alert" });
        /* Inside a phase's <summary>, a click on the line would toggle the
           phase. Reading an error is not a click on anything. */
        line.addEventListener("click", function (ev) { ev.preventDefault(); });
        if (place.after) place.after.insertAdjacentElement("afterend", line);
        else place.host.appendChild(line);
      }
      line.textContent = "Not sent: " + (e && e.message ? e.message : String(e));
    }

    /* A later success clears the line; an error is not forever. */
    function cleared(anchor) {
      if (!anchor || !anchor.isConnected) return;
      const place = errorPlace(anchor);
      if (place.after) {
        const next = place.after.nextElementSibling;
        if (next && next.classList.contains("pv-error")) next.remove();
      } else {
        place.host.querySelectorAll(":scope > .pv-error").forEach(function (n) { n.remove(); });
      }
    }

    function openComposer(spec) {
      const d = {
        id: spec.id || newId("composer"),
        clientId: spec.clientId || newId("cid"),
        kind: spec.kind,
        ref: spec.ref || null,
        thread: spec.thread || null,
        question: spec.kind === "answer" ? spec.ref.slice("question:".length) : null,
        revision: spec.revision || S.state.revision,
        text: spec.text || "",
        blocking: !!spec.blocking,
        quote: spec.quote || null,
      };
      const host = draftTarget(d);
      if (!host) return null;
      const existing = host.querySelector('[data-composer="' + d.id + '"]');
      if (existing) { if (!spec.silent) existing.querySelector("textarea").focus(); return existing; }
      /* One composer of a kind per target at a time: a second click
         focuses the open one rather than opening a twin. */
      const twin = Array.from(host.querySelectorAll(".composer")).find(function (c) {
        return c.getAttribute("data-kind") === d.kind;
      });
      if (twin && !spec.id) { twin.querySelector("textarea").focus(); return twin; }
      /* A composer opened for the reviewer's convenience, with nothing
         in it yet, is not a draft until they type. */
      if (!spec.lazy) saveDraft(d);

      const box = el("div", { class: "composer comment-box", dataset: { composer: d.id, kind: d.kind, revision: String(d.revision) } });
      const label = { comment: "Comment", answer: "Answer", reply: "Reply", ask: "Ask the agent", edit: "Edit", chat: "Message" }[d.kind];
      box.appendChild(el("span", { class: "composer-label", text: label }));
      const ta = el("textarea", { rows: "3", placeholder: d.kind === "answer" ? "Answer…" : d.kind === "ask" || d.kind === "chat" ? "Ask the agent…" : "Write…" });
      ta.value = d.text;
      ta.addEventListener("input", function () { d.text = ta.value; saveDraft(d); });
      box.appendChild(ta);
      /* One foot row: what the composer says about itself on the left (the
         blocking toggle, or who hears a question), the actions on the right. */
      const foot = el("div", { class: "comment-box-foot" });
      if (d.kind === "ask") foot.appendChild(el("span", { class: "composer-presence", text: presenceLine("question") }));
      let blockingBox = null;
      if (d.kind === "comment") {
        blockingBox = el("input", { type: "checkbox" });
        blockingBox.checked = d.blocking;
        blockingBox.addEventListener("change", function () { d.blocking = blockingBox.checked; saveDraft(d); });
        foot.appendChild(el("label", { class: "comment-box-blocking" }, blockingBox, warningIcon(), "Blocks approval"));
      }
      const actions = el("div", { class: "comment-box-actions" });
      const sendBtn = el("button", { type: "button", class: "pv-btn " + (d.kind === "ask" ? "is-agent" : "is-primary") + " composer-send", text: d.kind === "comment" ? "Add" : "Send" });
      const cancelBtn = el("button", { type: "button", class: "pv-btn is-quiet composer-cancel", text: "Cancel" });
      actions.appendChild(sendBtn);
      actions.appendChild(cancelBtn);
      foot.appendChild(actions);
      box.appendChild(foot);
      /* Closed by id, not by this node: a body swap while the send is in
         flight re-creates the composer from its draft, and the reply must
         close that one. */
      const close = function () {
        dropDraft(d.id);
        if (S.ui.chatComposerId === d.id) S.ui.chatComposerId = null;
        document.querySelectorAll('[data-composer="' + d.id + '"]').forEach(function (n) { n.remove(); });
        renderRecovery();
      };
      /* After a chat message is sent, the open panel keeps a place to
         write the next one. */
      const afterSend = function () {
        close();
        if (d.kind === "ask" || d.kind === "chat") markAsked();
        if (d.kind === "chat") ensureChatComposer();
      };
      cancelBtn.addEventListener("click", function () {
        close();
        /* An open panel always gets a composer back, so cancelling the
           chat's is closing the chat. */
        if (d.kind === "chat") setPanelOpen(false);
      });
      sendBtn.addEventListener("click", function () {
        const text = ta.value.trim();
        if (!text) return;
        const cmd = commandFor(d, text);
        if (!cmd) return;
        sendBtn.disabled = true;
        send(cmd).then(afterSend).catch(function (e) {
          const current = document.querySelector('[data-composer="' + d.id + '"] .composer-send');
          if (current) { current.disabled = false; failed(current, e); }
        });
      });
      ta.addEventListener("keydown", function (ev) {
        if ((ev.metaKey || ev.ctrlKey) && ev.key === "Enter") { ev.preventDefault(); sendBtn.click(); }
      });
      host.appendChild(box);
      if (!spec.silent) ta.focus({ preventScroll: true });
      return box;
    }

    /* The command a draft sends. `opened_revision` is the revision the
       composer OPENED against, whatever the page shows now, and the client
       id is the draft's, so a repeat is the same command. */
    function commandFor(d, text) {
      const base = { client_id: d.clientId };
      switch (d.kind) {
        case "comment": {
          const target = findRef(S.root, d.ref);
          return Object.assign(base, { cmd: "thread.open", ref: d.ref, text: text, blocking: !!d.blocking,
            quote: d.quote || (target ? elementQuote(target) : ""), opened_revision: d.revision });
        }
        case "answer": return Object.assign(base, { cmd: "question.answer", question: d.question, text: text, opened_revision: d.revision });
        case "reply": return Object.assign(base, { cmd: "thread.reply", thread: d.thread, text: text, opened_revision: d.revision });
        case "ask": {
          if (d.thread) return Object.assign(base, { cmd: "chat.send", thread: d.thread, text: text, opened_revision: d.revision });
          /* On an element with no thread yet: the server opens one and
             delivers the question in the same event. */
          const target = findRef(S.root, d.ref);
          return Object.assign(base, { cmd: "chat.send", ref: d.ref, text: text,
            quote: d.quote || (target ? elementQuote(target) : ""), opened_revision: d.revision });
        }
        case "edit": return Object.assign(base, { cmd: "thread.edit", thread: d.thread, text: text, opened_revision: d.revision });
        case "chat": return Object.assign(base, { cmd: "chat.send", text: text, opened_revision: d.revision });
        default: return null;
      }
    }

    /* Put every stored draft back where it belongs. Runs after every
       render, because a reply composer lives inside its thread and the
       thread may only just have arrived. Idempotent: a composer already on
       the page is left alone. */
    function restoreDrafts() {
      const map = loadDraftMap();
      for (const id in map) {
        const d = map[id];
        if (!draftTarget(d)) continue;
        /* A message half-written is not hidden behind a closed panel:
           the first time the page finds it, the panel opens. Once. A
           reviewer who then closes the panel has chosen. */
        openComposer({ id: d.id, clientId: d.clientId, kind: d.kind, ref: d.ref, thread: d.thread,
          revision: d.revision, text: d.text, blocking: d.blocking, quote: d.quote, silent: true });
      }
    }

    /* ---- per-element controls ----------------------------------------

       Every marked element gets a comment button. Threads and composers
       live on the FIRST element carrying a ref: the acceptance rows of a
       task share the task's ref, and a thread rendered once per row would
       show three times. A row's button still quotes the row. */
    function mountElements(root) {
      const seen = {};
      root.querySelectorAll("[data-plan-ref]").forEach(function (target) {
        const ref = target.getAttribute("data-plan-ref");
        const kind = refKind(ref);
        const isQuestion = kind === "question";
        const first = !seen[ref];
        seen[ref] = true;
        const slots = commentSlots(target);
        const quote = elementQuote(target);
        const btn = el("button", { type: "button", class: "comment-btn", title: isQuestion ? "Answer" : "Comment",
          "aria-label": isQuestion ? "Answer this question" : "Add comment" });
        btn.appendChild(commentIcon());
        btn.appendChild(el("span", { class: "comment-btn-label", text: isQuestion ? "Answer" : "Comment" }));
        btn.addEventListener("click", function (e) {
          e.stopPropagation();
          if (target.tagName === "DETAILS" && phaseIsShut(target)) setPhaseOpen(target, true, true);
          openComposer({ kind: isQuestion ? "answer" : "comment", ref: ref, quote: quote });
        });
        /* Asking is offered wherever commenting is, and looks different:
           a comment waits for the sent review, a question reaches the
           agent now. The two share one row. */
        const ask = el("button", { type: "button", class: "ask-btn" + (askedOnce() ? "" : " is-labelled"),
          "aria-label": "Ask the agent about this", dataset: { label: "Ask the agent", askFor: ref } });
        ask.appendChild(agentMark(""));
        ask.appendChild(el("span", { class: "ask-btn-label", text: "Ask the agent" }));
        ask.addEventListener("click", function (e) {
          e.stopPropagation();
          aimPanel(ref, quote, null);
        });
        (slots.btn || target).appendChild(el("span", { class: "el-actions" }, btn, ask));
        if (!first) return;
        const boxHost = slots.box || target;
        const mounted = [];
        if (isQuestion) {
          const q = ref.slice("question:".length);
          const answer = el("div", { class: "pv-answer", dataset: { answerFor: q }, hidden: true },
            el("div", { class: "pv-answer-head" },
              el("span", { class: "pv-answer-label", text: "Your answer" }),
              el("button", { type: "button", class: "pv-btn is-quiet pv-answer-edit", text: "Edit", onclick: function () {
                openComposer({ kind: "answer", ref: ref, text: S.state.answers[q] || "" });
              } }),
              el("button", { type: "button", class: "pv-btn is-quiet pv-answer-remove", text: "Remove", onclick: function (ev) {
                const btn = ev.currentTarget;
                send({ cmd: "question.answer", question: q, text: "", opened_revision: S.state.revision })
                  .catch(function (e) { failed(btn, e); });
              } })),
            el("p", { class: "pv-answer-text" }));
          mounted.push(answer);
        }
        mounted.push(el("div", { class: "pv-threads", dataset: { threadsFor: ref } }));
        mounted.push(el("div", { class: "pv-composers", dataset: { composersFor: ref } }));
        /* A phase's body holds its tasks. Its own comments go ABOVE them,
           under the phase's header, or they read as comments on the last
           task. Everything else appends after its own text. */
        if (target.tagName === "DETAILS") {
          const anchor = boxHost.firstChild;
          mounted.forEach(function (node) { boxHost.insertBefore(node, anchor); });
        } else {
          mounted.forEach(function (node) { boxHost.appendChild(node); });
        }
      });

      /* Reviewed marks on phase and task heads, as the static page has. */
      function toggle(container, ref) {
        const label = el("label", { class: "reviewed-toggle", dataset: { reviewedFor: ref } });
        const box = el("input", { type: "checkbox", class: "reviewed-box" });
        label.appendChild(box);
        label.appendChild(el("span", { class: "reviewed-toggle-text", text: "Mark reviewed" }));
        label.addEventListener("click", function (e) { e.stopPropagation(); });
        box.addEventListener("change", function () {
          const on = box.checked;
          const mark = S.pendingMarks[ref] || { on: on, count: 0 };
          mark.on = on;
          mark.count++;
          S.pendingMarks[ref] = mark;
          const settle = function () {
            mark.count--;
            /* While catching up the reply sits in the buffer, and the
               state does not have the mark yet: keep showing the choice
               until `drain` has applied it. */
            if (mark.count <= 0 && !S.syncing) delete S.pendingMarks[ref];
            renderReviewed();
          };
          send({ cmd: "element.reviewed", ref: ref, on: on })
            .then(function () {
              settle();
              cleared(findByAttr(document, ".reviewed-toggle", "data-reviewed-for", ref));
            })
            .catch(function (e) {
              settle();
              const current = findByAttr(document, ".reviewed-toggle", "data-reviewed-for", ref);
              if (current) failed(current, e);
            });
        });
        return label;
      }
      root.querySelectorAll("details.phase").forEach(function (details) {
        const ref = details.getAttribute("data-plan-ref");
        const line = details.querySelector("summary .phase-head-line");
        if (ref && line) line.appendChild(toggle(details, ref));
      });
      root.querySelectorAll('.task[data-plan-ref^="task:"]').forEach(function (task) {
        const ref = task.getAttribute("data-plan-ref");
        const line = task.querySelector(".task-head") || task;
        if (ref) line.appendChild(toggle(task, ref));
      });
    }

    function renderAll() {
      reconcilePending();
      renderPresence();
      renderThreads();
      renderAskMarks();
      renderAnswers();
      renderReviewed();
      renderBar();
      renderChat();
      renderNotices();
      renderRecovery();
      restoreDrafts();
      ensureChatComposer();
      renderRecovery();
      if (S.ui.pendingFocus && applyFocus(S.ui.pendingFocus)) S.ui.pendingFocus = null;
    }

    /* The render's orientation banner describes the static flow, which
       ends in copying feedback. A served page ends in Send review. */
    function mountBanner(root) {
      const text = root.querySelector(".pv-banner-text");
      if (text && !text.hasAttribute("data-served")) {
        text.setAttribute("data-served", "");
        text.replaceChildren(
          document.createTextNode("This plan is under live review. "),
          el("strong", { text: "Comment on anything, answer the questions, then send your review." }),
          document.createTextNode(" Comments reach the agent with your review; \u201cAsk the agent\u201d reaches it now."));
      }
      const steps = root.querySelector(".pv-banner-steps");
      if (steps && !steps.hasAttribute("data-served")) {
        steps.setAttribute("data-served", "");
        steps.replaceChildren(
          el("b", { text: "01" }), document.createTextNode(" skim  "),
          el("b", { text: "02" }), document.createTextNode(" comment  "),
          el("b", { text: "03" }), document.createTextNode(" send, or come back later"));
      }
    }

    S.mount = function (root, plan) {
      S.root = root;
      S.plan = plan;
      mountBanner(root);
      mountPresence(root);
      mountElements(root);
      mountPanel(root);
      noticeHost();
      renderAll();
      if (!S.socket && !S.lost) connect();
      /* Arriving is activity; a body swap is not. */
      if (!S.lastPing) S.lastPing = Date.now();
    };

    S.activity = ping;

    /* For the browser tests: what the page believes, as data. */
    S.debug = function () {
      return {
        connected: S.connected, page: S.pageId, syncing: S.syncing, gone: S.gone, lost: S.lost,
        reconnects: S.reconnects, applied: S.applied, revision: S.state.revision,
        planHash: S.state.planHash, lastSeq: S.state.lastSeq, presence: S.state.presence,
        submitted: S.state.submitted, chat: S.state.chat.length, chatOpen: S.ui.chatOpen,
        pending: Object.keys(S.pendingMarks).length,
        buffered: S.buffer.length,
        threads: S.state.threads.map(function (t) {
          return { id: t.id, target: t.target, status: t.status, blocking: t.blocking,
            messages: t.messages.map(function (m) { return m.actor + ": " + m.text; }) };
        }),
        answers: Object.assign({}, S.state.answers),
        reviewed: S.state.reviewed.slice(),
        drafts: loadDraftMap(),
        notices: Object.keys(S.ui).filter(function (k) { return k.indexOf("notice:") === 0; }).map(function (k) { return k.slice(7); }),
      };
    };
    /* For the browser tests: a frame as if the socket had delivered it. */
    S.injectFrame = function (frame) {
      if (S.syncing) S.buffer.push(frame); else applyFrame(frame, null);
    };
    S.resync = resync;
    return S;
  }

  core.debug = function () { return session ? session.debug() : null; };
  core.injectFrame = function (frame) { if (session) session.injectFrame(frame); };

  /* Activity, once per document. Spec 6.2: scroll, keys, pointer, and
     visibility, throttled to one ping per 30 seconds -- so a reader who
     reads for twenty minutes is not idle. */
  let activityWired = false;
  function wireActivity() {
    if (activityWired) return;
    activityWired = true;
    const mark = function () { if (session) session.activity(); };
    document.addEventListener("scroll", mark, { passive: true });
    document.addEventListener("keydown", mark);
    document.addEventListener("pointermove", mark, { passive: true });
    document.addEventListener("pointerdown", mark, { passive: true });
    document.addEventListener("visibilitychange", function () { if (!document.hidden) mark(); });
  }

  /* ---- mount and run ----------------------------------------------

     `mount(root)` builds every control from the markup under `root` and
     may run again against a swapped-in body. Observers from the previous
     mount are disconnected first. Global listeners -- print, the OS theme,
     activity -- are wired once and read the live document. */

  let mounted = { observers: [] };

  /* Expand every phase for print, then restore what the reader had.
     Once per document; the handlers read the DOM as it is when they run. */
  let printWired = false;
  function wirePrint() {
    if (printWired) return;
    printWired = true;
    let preprintState = null;
    const collapsibles = function () { return document.querySelectorAll("details.phase"); };
    window.addEventListener("beforeprint", function () {
      /* `phaseIsShut`, not `.open`: a phase caught mid-close is still
         technically open, and restoring it as open afterwards would leave
         the reader with a phase they had just clicked shut. */
      preprintState = Array.from(collapsibles()).map(function (d) { return !phaseIsShut(d); });
      collapsibles().forEach(function (d) { setPhaseOpen(d, true, false); });
    });
    window.addEventListener("afterprint", function () {
      if (!preprintState) return;
      collapsibles().forEach(function (d, i) { setPhaseOpen(d, preprintState[i], false); });
      preprintState = null;
    });
  }

  /* The summary click takeover and the expand/collapse controls are the
     same in both modes. */
  function mountDisclosure(root) {
    const firstPhase = root.querySelector("details.phase");
    if (!firstPhase) return;
    const collapsibles = function () { return root.querySelectorAll("details.phase"); };
    collapsibles().forEach(function (d) {
      const summary = d.querySelector("summary");
      if (!summary) return;
      summary.addEventListener("click", function (e) {
        if (e.defaultPrevented) return;
        if (e.target.closest && e.target.closest("a[href], button, input, select, textarea, label")) {
          return;
        }
        e.preventDefault();
        setPhaseOpen(d, phaseIsShut(d), true);
      });
    });
    const ctl = root.querySelector("#phases-actions") || document.createElement("div");
    if (ctl.querySelector(".pv-btn")) return;
    const expandBtn = document.createElement("button");
    expandBtn.type = "button";
    expandBtn.className = "pv-btn is-quiet";
    expandBtn.textContent = "expand all";
    expandBtn.addEventListener("click", function () {
      collapsibles().forEach(function (d) { setPhaseOpen(d, true, true); });
    });
    const collapseBtn = document.createElement("button");
    collapseBtn.type = "button";
    collapseBtn.className = "pv-btn is-quiet";
    collapseBtn.textContent = "collapse all";
    collapseBtn.addEventListener("click", function () {
      collapsibles().forEach(function (d) { setPhaseOpen(d, false, true); });
    });
    ctl.appendChild(expandBtn);
    ctl.appendChild(collapseBtn);
    if (!ctl.isConnected) firstPhase.parentNode.insertBefore(ctl, firstPhase);
    wirePrint();
  }

  function mount(root) {
    mounted.observers.forEach(function (o) { o.disconnect(); });
    mounted = { observers: [] };
    /* Chrome first, and outside the island guard: the theme toggle and the
       scroll cues are properties of the page, not of the plan data, so a
       document whose island failed to parse still gets a usable shell. */
    mountThemeToggle(root);
    mountScrollCues(root);

    const islandEl = root.querySelector("#plan-data");
    if (!islandEl) return;
    let plan;
    try { plan = core.parseIsland(islandEl.textContent); } catch (e) { return; }

    const served = root.getAttribute("data-artefacto-artifact") || (session && session.artifact);
    if (served) {
      if (!session) session = createSession(served);
      root.setAttribute("data-artefacto-artifact", served);
      mountDisclosure(root);
      wireActivity();
      session.mount(root, plan);
    } else {
      mountStatic(root, plan);
    }
  }
  core.mount = mount;

  function run() {
    /* The window.name trigger exists for the CI harness, which embeds this
       page via a sandboxed iframe's srcdoc (an about:srcdoc document has no
       URL fragment to carry #selftest). Inert otherwise: the selftest only
       appends a result marker. */
    if (location.hash === "#selftest" || window.name === "artefacto-selftest") selftest();
    else mount(document.body);
  }
  if (document.readyState !== "loading") {
    run();
  } else {
    document.addEventListener("DOMContentLoaded", run);
  }
})();
