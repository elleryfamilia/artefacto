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

**Both plan documents now carry a status block naming where the code
deliberately diverges from them. Read that before following either.**

## What is built and green

190 tests, `cargo fmt --all --check` and `cargo clippy --all-targets -D warnings`
clean.

| area | file | notes |
|---|---|---|
| state directory, `server.json`, startup lock | `src/server/state_dir.rs` | atomic write at 0600, page credential derived from the secret |
| event and frame envelope | `src/server/event.rs` | `reviewer.back` is **active**, per spec 6.2 |
| append-only log | `src/server/log.rs` | byte-offset replay, in-memory tail, strict corruption rules |
| the fold | `src/server/fold.rs`, `review.rs` | all state folds from the log |
| accept loop, guards | `src/server/http.rs` | also holds `Committer`, the mutation gate |
| bootstrap, cookie, CSP | `src/server/page.rs` | strips the document's own meta CSP, stamps every inline tag |
| WebSocket | `src/server/socket.rs` | **outbound only** — see below |
| page commands | `src/server/ingress.rs` | `POST /a/<artifact>/cmd` |
| `serve`, `stop`, `status` | `src/commands/serve.rs` | real double-fork daemon |

## Three things only running could establish

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

## What is next, in order

1. **The lease** (`src/server/lease.rs`). Codex critical #7: `acquire` must do
   decision, token minting, append and fold inside **one** `Committer`, or two
   callers both compute generation 1 and the loser holds an already-superseded
   token returned as `Ok`. `Waiting` mode records no pid; `Live` does.
2. **Delivery** (`src/server/delivery.rs`). Filter `is_internal` and
   `actor == Agent`. Codex critical #9: the server must persist **which frame
   was last offered to a session**, or "the next call acknowledges the previous
   frame" cannot be implemented by a fresh CLI process that knows only its
   token.
3. **`events` and `await`**. `--session` on both. Respond inside the route:
   dropping an unresponded `tiny_http::Request` yields a 500.
4. **`push`**. Codex critical #8: one `write_all` is **not** a transaction, so
   a revision plus its resolutions needs real framing — a commit marker that
   replay honours, or one recoverable log record.
5. **Presence, nudges, feedback document.** The feedback file must be written
   atomically *before* the `review.submitted` event that names its path.

## Still-open review findings

Both reports are saved outside the repo and are worth re-reading before the
remaining work: the critic's 42 findings and codex's two passes. The ones that
still apply to unbuilt code are listed above. Beyond those:

- `status --json` returns a minimal shape, not the full contract in spec 5.
- `open` is not implemented.
- Self-exit does not yet consider a live lease; the hook is marked in
  `should_self_exit`.
- There is still **no browser-level test**. Every socket path is driven by a
  fake client, which proves the server's half and nothing about a real
  browser's. That closes in plan 3.

## A spec inconsistency to resolve

Spec 6.2 lists `reviewer.back` as **active**; spec 5's `await` status table has
no `back` row. The code follows 6.2. Section 5 needs a `back` row, and
`reply --nudge` needs adding to its surface.
