# Artefacto Server Transport and Lifecycle Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> ## Implementation status — read this before executing anything
>
> **Tasks 1 through 7 are built and green** on the `feat/server-spine` branch.
> Do not execute them; read `src/server/` and the tests instead. This document
> is kept for the reasoning, not as instructions.
>
> **Where the code deliberately diverges from this plan:**
>
> - **Task 6 is wrong as written.** It specifies a read timeout on the upgraded
>   socket. That cannot be implemented: `tiny_http`'s `ReadWrite` is exactly
>   `Read + Write` with a blanket impl, so there is no `set_read_timeout`, no
>   `set_nonblocking`, no `AsRawFd` and no downcast, and the socket underneath
>   is unreachable. The shipped socket is therefore **outbound only** — one
>   thread owns it and only writes — and reviewer commands arrive over HTTP.
>   See the module docs in `src/server/socket.rs`.
> - **Disconnect detection needs a heartbeat.** With no reader, a write to a
>   departed peer succeeds until its RST arrives, so the writer pings every
>   20 seconds. This plan does not mention it; it was found by a failing test.
> - **`open` is not implemented yet**, and `status --json` returns a minimal
>   shape rather than the full contract in spec 5.
> - **Task 7's `close_inherited_except` is dangerous.** Closing descriptors by
>   number takes the bound listener with them and double-closes what Rust still
>   owns. The shipped `daemon.rs` closes nothing by number.

**Goal:** Stand up the artefacto server as a runnable daemon: one per repository, bound to loopback, owning an append-only event log, serving the rendered plan page to an authenticated browser over a WebSocket, with `serve`, `stop`, `status`, and `open`. When this plan is done you can start a server, open a plan in a browser, watch a frame arrive over the socket, and stop it cleanly. No agent, no lease, no review verbs — those are plan 2b.

**Architecture:** Blocking and thread-per-connection, no async runtime. The main thread accepts with `tiny_http`'s `recv_timeout` and spawns a thread per request, so a long poll in plan 2b occupies only its own thread. One append-only NDJSON log per server is the source of truth. The server is the only writer; CLI commands call it over loopback HTTP with a bearer secret.

**Tech Stack:** Rust 2021, edition floor 1.85. Existing: `clap`, `serde`, `serde_json`, `maud`, `pulldown-cmark`, `sha2`, `anyhow`. New: `tiny_http` 0.12, `tungstenite` 0.30, `libc`, `getrandom`. Tests use `assert_cmd`, `predicates`, `tempfile`.

**Spec:** `docs/specs/2026-09-06-artefacto-design.md`. Sections 4.2, 5, 8, and 9 are this plan's contract.

**Sibling plan:** `docs/plans/2026-09-09-artefacto-event-model.md` (plan 2b) builds the fold, page ingress, lease, delivery, and the agent verbs on top of this. The plan set in `docs/plans/2026-09-08-artefacto-extraction.md` listed six plans; item 2 is now 2a and 2b, so there are seven.

## Why this plan exists separately

An earlier single plan covering all of item 2 was reviewed by two independent models and by its own author's self-review. It drew 42 findings, six of them fatal. The findings clustered on a seam: transport, security, and lifecycle on one side; the event model, lease, and delivery on the other. They are split here so each is small enough to hold in one head, and so this half ships software you can run before the harder half starts.

**Findings this plan exists to get right**, all of which the earlier draft got wrong:

- Building a `tiny_http::Server` **before** forking. `Server::from_listener` spawns its accept thread at construction (`tiny_http-0.12.0/src/lib.rs:288`), and `fork()` keeps only the calling thread. The grandchild would hold a listening socket with nothing accepting on it, serve nothing, and self-exit 30 minutes later. Only `--foreground` would have worked, which is exactly the mode a test would use.
- Holding a page's mutex across a blocking `WebSocket::read()`, so any quiet tab deadlocked every broadcast, `page_count`, and through it the accept loop.
- Regenerating the page cookie on every start, so it could not survive the restart the persisted secret exists to support.
- Deleting `server.json` on shutdown, discarding the port and secret the plan's own restart test depends on.
- Classifying `reviewer.back` as passive when spec 6.2 lists it as active.

## A spec inconsistency this plan resolves

Spec 6.2 lists `reviewer.back` under **Active**. Spec 5's `await` status table has no `back` row. Both cannot be right.

This plan follows 6.2: `reviewer.back` is active. Plan 2b therefore returns `back` as an `await` status, and **spec section 5's table needs a `back` row added**. That edit is not made by this plan; it is listed here so it is not lost.

## Global Constraints

Every task's requirements implicitly include this section.

- Rust edition 2021, `rust-version = "1.85"`, `[toolchain] channel = "stable"` with `rustfmt` and `clippy`.
- Every task ends green on `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all`.
- Commit at the end of every task using Conventional Commits.
- **Nothing artefacto writes carries loadout's name.** Events declare `artefacto.event/1`, frames `artefacto.frame/1`. `tests/skill_examples.rs` enforces this.
- **Three locks, one order: `log` → `core` → `sockets`.** Never acquire them out of that order.
  - `log` is held across `fsync`. That is why it is a separate lock: appends serialize against each other without blocking any other route.
  - **No blocking operation ever happens while `core` or `sockets` is held** — no socket read or write, no `fsync`, no long poll, no browser launch. A handler that needs to send takes `sockets`, clones out the channel senders it needs, releases, and only then sends.
  - `core` and `sockets` must not be held at the same time. Holding `log` and then `core` is allowed, in that order.
  - Every function that takes a lock names which ones in its doc comment.
  - `std::sync::Mutex` is **not** reentrant. A function holding a guard must never call another that takes the same lock, so lock-taking functions come in pairs: a public one that locks, and an inner `_locked` one that takes the guard as a parameter. The earlier draft's `lease::acquire` called `lease::current`, which locked, unlocked, and let the caller re-lock — a check-then-act race that could hand out a token that was already superseded.
- **Loopback only.** Bind `127.0.0.1`. The `Host` header must be exactly `127.0.0.1:<port>` on every route; `localhost` is refused. On the development machine `http://localhost:<port>/` resolves and connects, so this check is the only thing rejecting it.
- **No CORS headers, ever.**
- The session secret is 256 random bits at mode `0600`, persisted across restarts, rotated only by `clean` (plan 4).
- **The page credential is derived from the persisted secret, never randomly regenerated**, or the cookie dies on every restart.
- **`server.json` is written atomically** (temp file, `fsync`, `rename`) and is **not** deleted on shutdown: it carries the port and secret that a restart must reuse. A dead pid in it means "no server", which `read_server_file` already reports.
- `seq` is server-wide, monotonic, and continues from the last logged value after a restart. Nothing renumbers it.
- **`reviewer.back` is active** (spec 6.2), against the earlier draft.
- macOS and Linux only.
- The real browser page is plan 3. This plan serves the existing static render with the server-mode adjustments in Task 5.

## File Structure

| file | responsibility |
|---|---|
| `src/server/mod.rs` | module root, the lock-order doc comment |
| `src/server/state_dir.rs` | state directory, `server.json`, secret, pid liveness, startup lock |
| `src/server/event.rs` | `artefacto.event/1` and `artefacto.frame/1` types |
| `src/server/log.rs` | append-only log: byte-offset replay, append, in-memory tail |
| `src/server/http.rs` | `Shared`, the accept loop, routing, `Host` and bearer guards |
| `src/server/page.rs` | bootstrap, cookie, Origin, CSP nonce, serving the page |
| `src/server/socket.rs` | WebSocket upgrade, one writer pump per page |
| `src/server/daemon.rs` | raw listener, double fork, readiness pipe |
| `src/client.rs` | the CLI's bearer-authenticated HTTP client |
| `src/commands/serve.rs` | `serve`, `stop`, `status`, `open` |
| `tests/support/mod.rs` | harness: server on a temp state dir, fake page client |
| `tests/server_security.rs` | Host, bootstrap, cookie, Origin, bearer, CSP |
| `tests/server_log.rs` | append, replay, `seq` continuity, corruption handling |
| `tests/server_lifecycle.rs` | serve, stop, status, open, restart, the daemonized server |

---

### Task 1: The state directory, `server.json`, and pid liveness

**Why this task exists:** every later task needs to know where the server keeps its files and whether a recorded server is real. Spec section 9 fixes the layout; section 4.2 requires the pid to be checked before `server.json` is trusted. Two details the earlier draft got wrong are pinned here by test: the file is written atomically at mode 0600, and it survives shutdown so a restart can reuse the port and secret.

**Files:**
- Create: `src/server/mod.rs`, `src/server/state_dir.rs`
- Modify: `src/lib.rs`, `Cargo.toml`

**Interfaces:**
- Produces:
  - `pub fn repo_root(cwd: &Path) -> anyhow::Result<PathBuf>`
  - `pub fn state_dir_in(base: &Path, repo_root: &Path) -> PathBuf` — base is the state root
  - `pub fn state_dir(repo_root: &Path) -> PathBuf` — `state_dir_in` with the ambient base
  - `pub struct ServerFile { pub pid: u32, pub port: u16, pub secret: String, pub started_at: String }`
  - `pub fn read_server_file(dir: &Path) -> Option<ServerFile>` — `None` when absent, corrupt, or the pid is dead
  - `pub fn read_server_file_any(dir: &Path) -> Option<ServerFile>` — ignores liveness; used to recover a port and secret after a clean shutdown
  - `pub fn write_server_file(dir: &Path, f: &ServerFile) -> anyhow::Result<()>` — atomic, mode 0600
  - `pub fn is_alive(pid: u32) -> bool`
  - `pub fn new_secret() -> String` — 64 lowercase hex characters
  - `pub fn derive_credential(secret: &str, purpose: &str) -> String` — domain-separated, for the page cookie
  - `pub struct StartupLock` — an exclusive lock file so two `serve` calls cannot both start a daemon

**Note on `state_dir_in`:** the earlier draft resolved the state root from `XDG_STATE_HOME` inside the path helper, and its tests called `std::env::set_var` to steer it. Cargo runs a test binary's tests on threads of one process, so that is a data race against any concurrent `std::env::var` as well as being order-dependent. The base is a parameter here; only `main` reads the environment.

- [ ] **Step 1: Add the dependencies**

```bash
cargo add tiny_http@0.12 tungstenite@0.30 libc getrandom
```

Expected: four new entries. Do **not** add `tokio`, `axum`, `hyper`, or `futures`; if one appears in `cargo tree`, an async runtime came in and the stack decision has been silently reversed.

- [ ] **Step 2: Write the failing test**

Create `src/server/state_dir.rs` with only its test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn sample(pid: u32) -> ServerFile {
        ServerFile {
            pid,
            port: 4321,
            secret: new_secret(),
            started_at: "2026-09-09T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn state_dir_is_keyed_by_repo_path() {
        let base = Path::new("/tmp/base");
        let a = state_dir_in(base, Path::new("/tmp/one"));
        let b = state_dir_in(base, Path::new("/tmp/two"));
        assert_ne!(a, b, "different repos must not share a state directory");
        assert_eq!(a, state_dir_in(base, Path::new("/tmp/one")), "must be stable");
        assert!(a.starts_with(base));
        let leaf = a.file_name().unwrap().to_string_lossy().to_string();
        assert_eq!(leaf.len(), 16, "16 hex characters of the repo-path digest");
        assert!(leaf.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn secret_is_256_bits_of_hex() {
        let s = new_secret();
        assert_eq!(s.len(), 64);
        assert!(s.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_ne!(s, new_secret());
    }

    #[test]
    fn the_page_credential_is_derived_not_random() {
        let secret = new_secret();
        let a = derive_credential(&secret, "page-cookie");
        assert_eq!(a, derive_credential(&secret, "page-cookie"), "same secret, same cookie");
        assert_ne!(
            a,
            derive_credential(&secret, "other"),
            "different purposes must not collide"
        );
        assert_ne!(a, secret, "the cookie must never be the bearer secret itself");
        assert_ne!(a, derive_credential(&new_secret(), "page-cookie"));
    }

    #[test]
    fn server_file_round_trips_at_0600() {
        let dir = tempfile::tempdir().unwrap();
        let f = sample(std::process::id());
        write_server_file(dir.path(), &f).unwrap();

        let mode = std::fs::metadata(dir.path().join("server.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "the file carries the secret");

        let back = read_server_file(dir.path()).expect("our own pid is alive");
        assert_eq!(back.port, 4321);
        assert_eq!(back.secret, f.secret);
    }

    #[test]
    fn rewriting_repairs_a_loosened_mode() {
        let dir = tempfile::tempdir().unwrap();
        write_server_file(dir.path(), &sample(std::process::id())).unwrap();
        let path = dir.path().join("server.json");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        write_server_file(dir.path(), &sample(std::process::id())).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "an atomic rename replaces the file, so the mode cannot be inherited from the old one"
        );
    }

    #[test]
    fn a_dead_pid_reads_as_no_server_but_keeps_the_port_and_secret() {
        let dir = tempfile::tempdir().unwrap();
        let f = sample(0); // pid 0 is never a live user process
        write_server_file(dir.path(), &f).unwrap();

        assert!(read_server_file(dir.path()).is_none(), "a dead pid means no server");
        let recovered = read_server_file_any(dir.path()).expect("the file itself is still there");
        assert_eq!(recovered.port, 4321, "a restart must rebind the same port");
        assert_eq!(recovered.secret, f.secret, "and keep the page's cookie valid");
    }

    #[test]
    fn a_corrupt_file_reads_as_no_server() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("server.json"), b"{not json").unwrap();
        assert!(read_server_file(dir.path()).is_none());
        assert!(read_server_file_any(dir.path()).is_none());
    }

    #[test]
    fn the_startup_lock_excludes_a_second_holder() {
        let dir = tempfile::tempdir().unwrap();
        let first = StartupLock::acquire(dir.path()).expect("first caller wins");
        assert!(
            StartupLock::acquire(dir.path()).is_none(),
            "two concurrent serve calls must not both start a daemon"
        );
        drop(first);
        assert!(StartupLock::acquire(dir.path()).is_some(), "released on drop");
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test --lib server::state_dir`
Expected: FAIL to compile, `cannot find function 'state_dir_in' in this scope`.

- [ ] **Step 4: Implement the module**

```rust
//! Where the server keeps its files, and whether a recorded server is real.
//!
//! Takes no locks. Reads the environment only through `state_dir`, which
//! `main` calls; everything else takes the base as a parameter so tests never
//! mutate process-global state.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

/// What must exist before the event log can be read. Everything else about a
/// running server is folded from the log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerFile {
    pub pid: u32,
    pub port: u16,
    pub secret: String,
    pub started_at: String,
}

pub fn repo_root(cwd: &Path) -> Result<PathBuf> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .context("running git rev-parse")?;
    if !out.status.success() {
        anyhow::bail!("not inside a git repository: {}", cwd.display());
    }
    Ok(PathBuf::from(String::from_utf8(out.stdout)?.trim()))
}

/// `<base>/artefacto/<16 hex of sha256(repo path)>`.
pub fn state_dir_in(base: &Path, repo_root: &Path) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(repo_root.to_string_lossy().as_bytes());
    let hash = format!("{:x}", hasher.finalize());
    base.join("artefacto").join(&hash[..16])
}

pub fn state_dir(repo_root: &Path) -> PathBuf {
    state_dir_in(&ambient_base(), repo_root)
}

fn ambient_base() -> PathBuf {
    // Not `if let ... { if ... }`: clippy rejects the nesting, and let-chains
    // need edition 2024, which this crate does not use.
    let xdg = std::env::var("XDG_STATE_HOME").unwrap_or_default();
    if !xdg.is_empty() {
        return PathBuf::from(xdg);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".local").join("state")
}

pub fn new_secret() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("the OS must provide randomness");
    hex(&bytes)
}

/// A second credential from the same secret, so the page's cookie survives a
/// restart without ever being the bearer secret. Domain-separated by
/// `purpose`, so one credential leaking does not yield another.
pub fn derive_credential(secret: &str, purpose: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(purpose.as_bytes());
    hasher.update([0u8]);
    hasher.update(secret.as_bytes());
    hex(&hasher.finalize())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Signal 0 asks "could I signal this process" without sending one.
///
/// Two known limits, both acceptable here: a process owned by another user
/// fails with EPERM and reads as dead, and a reused pid reads as alive. The
/// server is per-user and per-repository, and `stop` confirms identity over
/// the authenticated port before signalling anything.
pub fn is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // SAFETY: `kill` with signal 0 performs the permission and existence check
    // without delivering a signal. No memory is touched.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// The file as written, whether or not its process still exists. `serve` uses
/// this to recover the port and secret after a clean shutdown.
pub fn read_server_file_any(dir: &Path) -> Option<ServerFile> {
    let raw = fs::read_to_string(dir.join("server.json")).ok()?;
    serde_json::from_str(&raw).ok()
}

/// `None` for absent, corrupt, or dead — every caller treats those the same.
pub fn read_server_file(dir: &Path) -> Option<ServerFile> {
    let f = read_server_file_any(dir)?;
    if is_alive(f.pid) {
        Some(f)
    } else {
        None
    }
}

/// Written to a temp file at 0600, synced, then renamed over the target. A
/// truncate-in-place would leave a half-written file readable after a crash,
/// and would inherit a loosened mode from the file already there.
pub fn write_server_file(dir: &Path, f: &ServerFile) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let final_path = dir.join("server.json");
    let tmp_path = dir.join("server.json.tmp");

    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true).mode(0o600);
    let file = opts
        .open(&tmp_path)
        .with_context(|| format!("creating {}", tmp_path.display()))?;
    serde_json::to_writer_pretty(&file, f)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp_path, &final_path)
        .with_context(|| format!("renaming into {}", final_path.display()))?;
    Ok(())
}

/// An advisory exclusive lock, held for as long as the value lives. Two
/// `serve` invocations racing would otherwise both find no server and both
/// start a daemon.
pub struct StartupLock {
    _file: fs::File,
}

impl StartupLock {
    pub fn acquire(dir: &Path) -> Option<StartupLock> {
        fs::create_dir_all(dir).ok()?;
        let file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(dir.join("startup.lock"))
            .ok()?;
        // SAFETY: flock on a valid owned descriptor. LOCK_NB returns rather
        // than blocking when another process holds it.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            Some(StartupLock { _file: file })
        } else {
            None
        }
    }
}
```

`src/server/mod.rs`:

```rust
//! The artefacto server: one loopback daemon per repository.
//!
//! # Lock order
//!
//! `Shared` holds two locks: `core` (log, revisions, page auth) and the page
//! registry inside `sockets`. The order is **`core` before `sockets`, and
//! never both at once**. No blocking operation — a socket write, an `fsync`,
//! a long poll, a browser launch — runs while either is held.
//!
//! `std::sync::Mutex` is not reentrant. A function that holds a guard must
//! never call another that takes the same lock, so lock-taking functions come
//! in pairs: a public one that locks, and an inner `_locked` one that takes
//! the guard.

pub mod state_dir;
```

Add `pub mod server;` to `src/lib.rs`.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --lib server::state_dir`
Expected: PASS, 8 tests.

- [ ] **Step 6: Verify the gate**

Run: `cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --all`
Expected: all clean.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock src/lib.rs src/server/
git commit -m "feat(server): state directory, atomic server.json, derived page credential"
```

---

### Task 2: Event and frame types

**Why this task exists:** the envelope in spec 6.1 is a cross-process contract that the agent, the page, and a future loadout release all parse. Freezing it in types with a round-trip test stops the later tasks drifting the field names.

**One correction to carry:** the earlier draft classified `reviewer.back` as passive and wrote a test asserting that. Spec 6.2 lists it as **active**. The test below asserts the spec's classification. This is the one defect in the earlier draft that compiled, ran, and passed while still being wrong — proof that a green test says nothing about whether the code matches the contract.

**Files:**
- Create: `src/server/event.rs`
- Modify: `src/server/mod.rs`

**Interfaces:**
- Produces:
  - `pub struct Event { pub format, pub seq, pub ts, pub artifact, pub revision, pub actor, pub r#type, pub data }`
  - `pub enum Actor { Reviewer, Agent, Server }` serializing lowercase
  - `pub struct Frame { pub format: String, pub seq: u64, pub events: Vec<Event> }`
  - `pub const EVENT_FORMAT: &str = "artefacto.event/1";`
  - `pub const FRAME_FORMAT: &str = "artefacto.frame/1";`
  - `pub fn is_active(event_type: &str) -> bool`
  - `pub fn is_internal(event_type: &str) -> bool` — control records that are never delivered to anyone

**On `is_internal`:** plan 2b persists delivery cursors as log records, because spec 4.2 requires every piece of state to fold from the log. Those records must never reach a client. The earlier draft had no such predicate, so acknowledging an event appended a record that was itself delivered in the next frame — the agent received its own bookkeeping, and in live mode each flush produced an event that triggered another flush. The predicate is defined here so plan 2b cannot forget it.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn at(seq: u64, kind: &str) -> Event {
        Event {
            format: EVENT_FORMAT.to_string(),
            seq,
            ts: "2026-09-06T16:02:11Z".to_string(),
            artifact: "plan:auth-refactor".to_string(),
            revision: 3,
            actor: Actor::Reviewer,
            r#type: kind.to_string(),
            data: serde_json::Value::Null,
        }
    }

    #[test]
    fn an_event_serializes_to_the_spec_envelope() {
        let mut e = at(42, "thread.replied");
        e.data = serde_json::json!({ "thread": "c-3", "ref": "task:t-session-store" });
        let v: serde_json::Value = serde_json::to_value(&e).unwrap();
        assert_eq!(v["format"], "artefacto.event/1");
        assert_eq!(v["seq"], 42);
        assert_eq!(v["actor"], "reviewer");
        assert_eq!(v["type"], "thread.replied", "the wire field is `type`, not `r#type`");
        assert_eq!(v["data"]["thread"], "c-3");
    }

    #[test]
    fn an_event_round_trips() {
        let json = r#"{"format":"artefacto.event/1","seq":7,"ts":"2026-09-06T16:02:11Z",
            "artifact":"plan:x","revision":1,"actor":"agent","type":"revision.published",
            "data":{}}"#;
        let e: Event = serde_json::from_str(json).unwrap();
        assert_eq!(e.seq, 7);
        assert!(matches!(e.actor, Actor::Agent));
        assert_eq!(e.r#type, "revision.published");
    }

    #[test]
    fn a_frame_names_its_last_event_as_the_ack_point() {
        let f = Frame::of(vec![at(4, "thread.opened"), at(5, "thread.opened"), at(9, "chat.sent")]);
        assert_eq!(f.seq, 9, "a frame's seq is its last event's seq");
        assert_eq!(f.format, "artefacto.frame/1");
    }

    #[test]
    fn active_and_passive_match_spec_6_2() {
        // Spec 6.2 lists reviewer.back under Active. An earlier draft of this
        // plan put it in the passive list and asserted that; the test passed
        // and the classification was still wrong.
        for t in [
            "chat.sent",
            "review.submitted",
            "reviewer.idle",
            "reviewer.away",
            "reviewer.back",
            "server.stopping",
        ] {
            assert!(is_active(t), "{t} is active in spec 6.2");
        }
        for t in [
            "thread.opened",
            "thread.replied",
            "thread.edited",
            "thread.deleted",
            "question.answered",
            "element.reviewed",
        ] {
            assert!(!is_active(t), "{t} is passive in spec 6.2");
        }
    }

    #[test]
    fn control_records_are_internal_and_never_active() {
        assert!(is_internal("cursor.acked"));
        assert!(!is_active("cursor.acked"), "an internal record must not wake anyone");
        for t in ["chat.sent", "thread.opened", "revision.published"] {
            assert!(!is_internal(t), "{t} is part of the protocol");
        }
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib server::event`
Expected: FAIL to compile, `cannot find type 'Event' in this scope`.

- [ ] **Step 3: Implement the module**

```rust
//! The wire envelope. Spec section 6.1.

use serde::{Deserialize, Serialize};

pub const EVENT_FORMAT: &str = "artefacto.event/1";
pub const FRAME_FORMAT: &str = "artefacto.frame/1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Actor {
    Reviewer,
    Agent,
    Server,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub format: String,
    pub seq: u64,
    pub ts: String,
    pub artifact: String,
    pub revision: u32,
    pub actor: Actor,
    /// `type` is a Rust keyword; the wire name is plain `type`.
    #[serde(rename = "type")]
    pub r#type: String,
    pub data: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frame {
    pub format: String,
    pub seq: u64,
    pub events: Vec<Event>,
}

impl Frame {
    /// A frame's own `seq` is the acknowledgement point, so it is the seq of
    /// the last event in it — the one that caused the frame to be sent.
    pub fn of(events: Vec<Event>) -> Self {
        let seq = events.last().map(|e| e.seq).unwrap_or(0);
        Frame {
            format: FRAME_FORMAT.to_string(),
            seq,
            events,
        }
    }
}

/// Active events wake the agent; passive ones ride along with the next active
/// one in digest mode. Spec 6.2 — including `reviewer.back`, which is active.
pub fn is_active(event_type: &str) -> bool {
    matches!(
        event_type,
        "chat.sent"
            | "review.submitted"
            | "reviewer.idle"
            | "reviewer.away"
            | "reviewer.back"
            | "server.stopping"
    )
}

/// Control records the server writes so its own state folds from the log.
/// They are never delivered to an agent or a page. Without this, acknowledging
/// a frame appends a record that is itself delivered in the next frame.
pub fn is_internal(event_type: &str) -> bool {
    matches!(event_type, "cursor.acked" | "lease.taken" | "lease.released")
}
```

- [ ] **Step 4: Declare the module and run the tests**

Add `pub mod event;` to `src/server/mod.rs`.

Run: `cargo test --lib server::event`
Expected: PASS, 5 tests.

- [ ] **Step 5: Verify the gate**

Run: `cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --all`
Expected: all clean.

- [ ] **Step 6: Commit**

```bash
git add src/server/event.rs src/server/mod.rs
git commit -m "feat(server): freeze the event envelope with spec 6.2 classification"
```

---

### Task 3: The append-only event log

**Why this task exists:** spec 4.2 makes the log the only source of truth and 6.7 requires `seq` to continue across a restart. Two properties are load-bearing and were wrong in the earlier draft: recovery must not silently discard committed history, and reading the log must not re-parse the file on every call while holding a lock other routes need.

**What the earlier draft got wrong, pinned by test here:**

- It counted `line.len() + 1` bytes per good line. A file ending mid-newline made `good_bytes` exceed the file size, the truncation guard was skipped, and the next append was concatenated onto the previous line.
- Any malformed line **anywhere** stopped the replay and silently dropped every valid event after it. A single bad byte in the middle would discard the rest of the review.
- It re-opened and re-parsed the whole file on every `read_since`. Plan 2b's long poll calls that roughly 1,800 times per 90-second wait.
- A failed `sync_all` left `next_seq` unadvanced, so a later append could reuse a sequence number that may already be on disk.

**Files:**
- Create: `src/server/log.rs`, `tests/server_log.rs`
- Modify: `src/server/mod.rs`

**Interfaces:**
- Consumes: `event::{Event, Actor, EVENT_FORMAT}` (Task 2).
- Produces:
  - `pub struct EventLog`
  - `pub fn open(dir: &Path) -> anyhow::Result<EventLog>`
  - `pub fn append(&mut self, artifact: &str, revision: u32, actor: Actor, kind: &str, data: serde_json::Value) -> anyhow::Result<Event>`
  - `pub fn since(&self, cursor: u64) -> &[Event]` — a slice of the in-memory tail, no I/O
  - `pub fn last_seq(&self) -> u64`
  - `pub fn now_rfc3339() -> String`

**Memory:** the whole log is held in memory after replay. A `revision.published` event carries a full plan, so a long session with many revisions costs real memory. That is accepted for v1: one repository, one review, and `clean` truncates. If it ever matters, the fix is to keep only events above a checkpoint and page older ones from disk — not to go back to re-parsing per call.

- [ ] **Step 1: Write the failing test**

Create `tests/server_log.rs`:

```rust
use artefacto::server::event::Actor;
use artefacto::server::log::EventLog;

fn data() -> serde_json::Value {
    serde_json::json!({ "ref": "task:t-a" })
}

fn append(log: &mut EventLog, kind: &str) -> u64 {
    log.append("plan:x", 1, Actor::Reviewer, kind, data()).unwrap().seq
}

#[test]
fn append_assigns_monotonic_sequence_numbers() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = EventLog::open(dir.path()).unwrap();
    assert_eq!(append(&mut log, "thread.opened"), 1);
    assert_eq!(append(&mut log, "thread.opened"), 2);
    assert_eq!(log.last_seq(), 2);
}

#[test]
fn seq_continues_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut log = EventLog::open(dir.path()).unwrap();
        append(&mut log, "thread.opened");
        append(&mut log, "thread.opened");
    }
    let mut reopened = EventLog::open(dir.path()).unwrap();
    assert_eq!(reopened.last_seq(), 2, "a restart must not renumber");
    assert_eq!(append(&mut reopened, "thread.opened"), 3);
}

#[test]
fn since_is_exclusive_of_the_cursor_and_reads_no_disk() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = EventLog::open(dir.path()).unwrap();
    for _ in 0..5 {
        append(&mut log, "thread.opened");
    }
    let got = log.since(2);
    assert_eq!(got.len(), 3, "seq 3, 4, 5");
    assert_eq!(got[0].seq, 3, "the cursor names what was already delivered");
    assert_eq!(got[2].seq, 5);
    assert!(log.since(5).is_empty(), "caught up means nothing");
    assert!(log.since(99).is_empty(), "a cursor past the end is not an error");
}

#[test]
fn an_unterminated_tail_is_dropped_and_its_sequence_reused() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut log = EventLog::open(dir.path()).unwrap();
        append(&mut log, "thread.opened");
        append(&mut log, "thread.opened");
    }
    // A crash mid-write: a partial line with no terminating newline.
    let path = dir.path().join("events.ndjson");
    let mut raw = std::fs::read_to_string(&path).unwrap();
    raw.push_str("{\"format\":\"artefacto.event/1\",\"seq\":3,\"ts\"");
    std::fs::write(&path, raw).unwrap();

    let mut log = EventLog::open(dir.path()).unwrap();
    assert_eq!(log.last_seq(), 2, "a torn line was never a committed event");
    assert_eq!(append(&mut log, "thread.opened"), 3, "so seq 3 is still free");
    assert_eq!(log.since(0).len(), 3);

    // And the file is clean: reopening sees exactly three events.
    let reopened = EventLog::open(dir.path()).unwrap();
    assert_eq!(reopened.since(0).len(), 3, "the torn bytes were truncated, not concatenated");
}

#[test]
fn a_file_ending_mid_newline_is_recovered_without_corruption() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut log = EventLog::open(dir.path()).unwrap();
        append(&mut log, "thread.opened");
    }
    let path = dir.path().join("events.ndjson");
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.pop(); // drop the trailing newline
    std::fs::write(&path, bytes).unwrap();

    let mut log = EventLog::open(dir.path()).unwrap();
    // The line parses but was never terminated, so it is treated as torn.
    assert_eq!(log.last_seq(), 0, "an unterminated line is not a committed event");
    append(&mut log, "thread.opened");
    let reopened = EventLog::open(dir.path()).unwrap();
    assert_eq!(
        reopened.since(0).len(),
        1,
        "the next append must not be glued onto the unterminated line"
    );
}

#[test]
fn a_malformed_line_in_the_middle_is_a_hard_error() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut log = EventLog::open(dir.path()).unwrap();
        append(&mut log, "thread.opened");
        append(&mut log, "thread.opened");
        append(&mut log, "thread.opened");
    }
    let path = dir.path().join("events.ndjson");
    let lines: Vec<String> = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    let corrupted = format!("{}\n{{garbage}}\n{}\n", lines[0], lines[2]);
    std::fs::write(&path, corrupted).unwrap();

    let err = EventLog::open(dir.path()).expect_err("committed history must never be dropped silently");
    let text = format!("{err:#}");
    assert!(text.contains("line 2"), "the error names the line: {text}");
}

#[test]
fn a_sequence_gap_is_a_hard_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.ndjson");
    let line = |seq: u64| {
        format!(
            "{{\"format\":\"artefacto.event/1\",\"seq\":{seq},\"ts\":\"t\",\"artifact\":\"plan:x\",\
             \"revision\":1,\"actor\":\"reviewer\",\"type\":\"thread.opened\",\"data\":null}}\n"
        )
    };
    std::fs::create_dir_all(dir.path()).unwrap();
    std::fs::write(&path, format!("{}{}", line(1), line(3))).unwrap();

    let err = EventLog::open(dir.path()).expect_err("a gap means an event went missing");
    assert!(format!("{err:#}").contains("expected seq 2"));
}

#[test]
fn a_foreign_format_is_a_hard_error() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path()).unwrap();
    std::fs::write(
        dir.path().join("events.ndjson"),
        "{\"format\":\"something.else/9\",\"seq\":1,\"ts\":\"t\",\"artifact\":\"plan:x\",\
         \"revision\":1,\"actor\":\"reviewer\",\"type\":\"thread.opened\",\"data\":null}\n",
    )
    .unwrap();
    assert!(EventLog::open(dir.path()).is_err(), "a log from another tool is not ours to read");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test server_log`
Expected: FAIL to compile, `could not find 'log' in 'server'`.

- [ ] **Step 3: Implement the module**

```rust
//! The append-only event log. One per server, one server-wide `seq`.
//!
//! Holds only its own lock (`Shared.log`), and holds it across `fsync`. That
//! is the reason it is a separate lock: appends serialize against each other
//! without blocking any other route. It never takes `core` or `sockets`.
//!
//! Recovery rule: an **unterminated** final line is a torn write and is
//! truncated. Anything else that does not parse — a bad line in the middle, a
//! sequence gap, a foreign format — is a hard error. Dropping committed
//! history quietly is worse than refusing to start.

use crate::server::event::{Actor, Event, EVENT_FORMAT};
use anyhow::{bail, Context, Result};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// `Debug` is derived because the tests use `Result::expect_err`, which
/// requires `T: Debug` on the success type.
#[derive(Debug)]
pub struct EventLog {
    path: PathBuf,
    file: File,
    /// The whole log, in order. `since` slices this; nothing re-parses disk.
    events: Vec<Event>,
    next_seq: u64,
    /// Set when a write may or may not have reached the disk. Every later
    /// append refuses, because reusing a sequence number that might already be
    /// on disk is the one unrecoverable mistake this file can make.
    poisoned: bool,
}

impl EventLog {
    pub fn open(dir: &Path) -> Result<EventLog> {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let path = dir.join("events.ndjson");
        let mut events = Vec::new();

        if path.exists() {
            let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
            let mut offset = 0usize;
            let mut line_no = 0usize;
            let mut good_bytes = 0usize;

            while offset < bytes.len() {
                let rest = &bytes[offset..];
                let Some(nl) = rest.iter().position(|b| *b == b'\n') else {
                    // No terminator: a torn tail. Truncate it, whatever it holds.
                    break;
                };
                line_no += 1;
                let line = &rest[..nl];
                let text = std::str::from_utf8(line)
                    .with_context(|| format!("{}: line {line_no} is not utf-8", path.display()))?;
                let event: Event = serde_json::from_str(text)
                    .with_context(|| format!("{}: line {line_no} is not an event", path.display()))?;
                if event.format != EVENT_FORMAT {
                    bail!("{}: line {line_no} declares {}, not {EVENT_FORMAT}", path.display(), event.format);
                }
                let expected = events.len() as u64 + 1;
                if event.seq != expected {
                    bail!("{}: line {line_no} has seq {}, expected seq {expected}", path.display(), event.seq);
                }
                events.push(event);
                offset += nl + 1;
                good_bytes = offset;
            }

            if good_bytes < bytes.len() {
                let f = OpenOptions::new().write(true).open(&path)?;
                f.set_len(good_bytes as u64)?;
                f.sync_all()?;
            }
        }

        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let next_seq = events.len() as u64 + 1;
        Ok(EventLog { path, file, events, next_seq, poisoned: false })
    }

    pub fn last_seq(&self) -> u64 {
        self.next_seq - 1
    }

    /// Every event with `seq` strictly greater than `cursor`. In memory, so a
    /// long poll can call it as often as it likes.
    pub fn since(&self, cursor: u64) -> &[Event] {
        let idx = self.events.partition_point(|e| e.seq <= cursor);
        &self.events[idx..]
    }

    pub fn append(
        &mut self,
        artifact: &str,
        revision: u32,
        actor: Actor,
        kind: &str,
        data: serde_json::Value,
    ) -> Result<Event> {
        if self.poisoned {
            bail!("the event log is in an unknown state after a failed write; restart the server");
        }
        let event = Event {
            format: EVENT_FORMAT.to_string(),
            seq: self.next_seq,
            ts: now_rfc3339(),
            artifact: artifact.to_string(),
            revision,
            actor,
            r#type: kind.to_string(),
            data,
        };
        let line = serde_json::to_string(&event)?;
        // From here the write may be partially visible on disk, so any failure
        // poisons rather than being retried at the same sequence number.
        if let Err(e) = writeln!(self.file, "{line}").and_then(|()| self.file.sync_all()) {
            self.poisoned = true;
            return Err(e).with_context(|| format!("appending to {}", self.path.display()));
        }
        self.next_seq += 1;
        self.events.push(event.clone());
        Ok(event)
    }
}

/// RFC 3339 in UTC to the second. No date crate: the only consumers are a
/// human reading the log and a client echoing the string back.
pub fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let (y, m, d) = civil_from_days((secs / 86_400) as i64);
    let tod = secs % 86_400;
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", tod / 3600, (tod % 3600) / 60, tod % 60)
}

/// Howard Hinnant's days-to-civil algorithm, public domain.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::civil_from_days;

    #[test]
    fn civil_from_days_matches_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1), "the epoch");
        assert_eq!(civil_from_days(19_000), (2022, 1, 8));
        assert_eq!(civil_from_days(20_000), (2024, 10, 4));
        // A leap day, where naive implementations go wrong.
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
    }
}
```

- [ ] **Step 4: Declare the module and run the tests**

Add `pub mod log;` to `src/server/mod.rs`.

Run: `cargo test --test server_log && cargo test --lib server::log`
Expected: PASS, 8 integration tests and 1 unit test.

- [ ] **Step 5: Verify the gate**

Run: `cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --all`
Expected: all clean.

- [ ] **Step 6: Commit**

```bash
git add src/server/log.rs src/server/mod.rs tests/server_log.rs
git commit -m "feat(server): append-only log with strict recovery and an in-memory tail"
```

---

### Task 4: The HTTP skeleton, the guards, and the test harness

**Why this task exists:** every later route sits behind two guards, and every later test needs a server it can start and — crucially — stop. Spec 8 requires an exact `Host` match everywhere and a bearer secret on CLI routes.

**What the earlier draft got wrong:**

- `Harness::drop` called `server.unblock()`, which makes `recv_timeout` return `Ok(None)` once (`tiny_http-0.12.0/src/util/messages_queue.rs:69-95`). The run loop treated that as an ordinary idle tick and kept looping, so every test leaked a live server thread writing into a deleted temp directory. There is an explicit shutdown flag here, and `Drop` joins the thread.
- Self-exit measured "any HTTP request", so an unauthenticated request could keep the daemon alive, while a page connected over a WebSocket — which sends no further HTTP requests — could not. Spec 4.2 says "no page **and** no agent have been connected for 30 minutes". Self-exit here is a function of connections, and `last_request_at` only breaks ties.

**Files:**
- Create: `src/server/http.rs`, `tests/support/mod.rs`, `tests/server_security.rs`
- Modify: `src/server/mod.rs`

**Interfaces:**
- Consumes: `log::EventLog` (Task 3), `state_dir::derive_credential` (Task 1).
- Produces:
  - `pub struct Shared { pub log: Mutex<EventLog>, pub core: Mutex<Core>, pub sockets: PageSockets, pub secret: String, pub page_cookie: String, pub port: u16, stopping: AtomicBool }`
  - `pub struct Core { pub bootstrap: HashMap<String, (String, Instant)>, pub last_request_at: Instant }`
  - `pub fn Shared::new(dir: &Path, secret: String, port: u16) -> anyhow::Result<Shared>`
  - `pub fn Shared::stopping(&self) -> bool` and `pub fn Shared::request_stop(&self)`
  - `pub fn run(shared: Arc<Shared>, server: Arc<tiny_http::Server>, idle: Duration)`
  - `pub fn header(req: &Request, name: &'static str) -> Option<String>`
  - `pub fn host_ok(req: &Request, port: u16) -> bool`
  - `pub fn bearer_ok(req: &Request, secret: &str) -> bool`
  - `pub fn json_response(status: u16, body: &str) -> Response<Cursor<Vec<u8>>>`
  - `pub fn error_response(status: u16, code: &str, message: &str) -> Response<Cursor<Vec<u8>>>`

**API note confirmed by a prototype:** `tiny_http`'s `HeaderField::equiv` takes a `&'static str`. A helper accepting a plain `&str` does not compile (E0521). Also, every handler must call `request.respond(...)`; dropping an unresponded `Request` makes tiny_http answer 500.

- [ ] **Step 1: Write the test harness**

Create `tests/support/mod.rs`:

```rust
//! Starts a real server on a temp state directory and stops it on drop.
//! No test touches the developer's real state.

use artefacto::server::http::{run, Shared};
use std::io::{Read, Write};
use std::sync::Arc;

pub struct Harness {
    pub port: u16,
    pub secret: String,
    pub shared: Arc<Shared>,
    dir: Option<tempfile::TempDir>,
    server: Arc<tiny_http::Server>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Harness {
    pub fn start() -> Harness {
        Harness::start_with_idle(std::time::Duration::from_secs(3600))
    }

    pub fn start_with_idle(idle: std::time::Duration) -> Harness {
        let dir = tempfile::tempdir().unwrap();
        let secret = artefacto::server::state_dir::new_secret();
        let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
        let port = server.server_addr().to_ip().unwrap().port();
        let shared = Arc::new(Shared::new(dir.path(), secret.clone(), port).unwrap());

        let s = Arc::clone(&server);
        let sh = Arc::clone(&shared);
        let thread = std::thread::spawn(move || run(sh, s, idle));

        Harness { port, secret, shared, dir: Some(dir), server, thread: Some(thread) }
    }

    /// Stops this server and starts a new one over the **same state
    /// directory**. This is the log-sufficiency check: everything the new
    /// server knows, it read from the log.
    pub fn restart(mut self) -> Harness {
        let dir = self.dir.take().expect("a harness restarts only once per step");
        self.shutdown();
        let secret = self.secret.clone();
        let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
        let port = server.server_addr().to_ip().unwrap().port();
        let shared = Arc::new(Shared::new(dir.path(), secret.clone(), port).unwrap());
        let s = Arc::clone(&server);
        let sh = Arc::clone(&shared);
        let thread = std::thread::spawn(move || {
            run(sh, s, std::time::Duration::from_secs(3600))
        });
        Harness { port, secret, shared, dir: Some(dir), server, thread: Some(thread) }
    }

    fn shutdown(&mut self) {
        self.shared.request_stop();
        self.server.unblock();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }

    /// Bytes verbatim, so a test can send a hostile `Host` or omit a header. A
    /// polite HTTP client normalizes exactly what these tests attack with.
    pub fn raw(&self, request: &str) -> String {
        let mut s = std::net::TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        s.write_all(request.as_bytes()).unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        out
    }

    pub fn get(&self, path: &str, headers: &[(&str, &str)]) -> String {
        let mut req = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n", self.port);
        for (k, v) in headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str("Connection: close\r\n\r\n");
        self.raw(&req)
    }

    pub fn status_of(response: &str) -> u16 {
        response.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0)
    }

    /// Polls a condition instead of sleeping a fixed time: a fixed sleep is
    /// either slow or flaky, usually both.
    pub fn wait_for(&self, mut cond: impl FnMut() -> bool, what: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if cond() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("timed out waiting: {what}");
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.shutdown();
    }
}
```

`dir` is an `Option<TempDir>` because `restart` moves it out of a type that implements `Drop`, which is otherwise E0509.

- [ ] **Step 2: Write the failing security tests**

Create `tests/server_security.rs`:

```rust
mod support;
use support::Harness;

#[test]
fn the_exact_loopback_host_is_accepted() {
    let h = Harness::start();
    assert_eq!(Harness::status_of(&h.get("/healthz", &[])), 200);
}

#[test]
fn the_localhost_name_is_refused() {
    let h = Harness::start();
    let r = h.raw(&format!(
        "GET /healthz HTTP/1.1\r\nHost: localhost:{}\r\nConnection: close\r\n\r\n",
        h.port
    ));
    assert_eq!(
        Harness::status_of(&r),
        421,
        "localhost resolves to 127.0.0.1 on real machines, so the exact Host check \
         is the only thing keeping a rebinding attacker out"
    );
}

#[test]
fn a_foreign_host_header_is_refused() {
    let h = Harness::start();
    let r = h.raw("GET /healthz HTTP/1.1\r\nHost: evil.example.com\r\nConnection: close\r\n\r\n");
    assert_eq!(Harness::status_of(&r), 421);
}

#[test]
fn a_cli_route_without_the_bearer_is_refused() {
    let h = Harness::start();
    let r = h.get("/cli/status", &[]);
    assert_eq!(Harness::status_of(&r), 401);
    assert!(r.contains("\"code\":\"unauthorized\""), "errors are JSON, not HTML");
}

#[test]
fn a_cli_route_with_the_bearer_is_served() {
    let h = Harness::start();
    let auth = format!("Bearer {}", h.secret);
    assert_eq!(Harness::status_of(&h.get("/cli/status", &[("Authorization", &auth)])), 200);
}

#[test]
fn a_wrong_bearer_is_refused() {
    let h = Harness::start();
    assert_eq!(
        Harness::status_of(&h.get("/cli/status", &[("Authorization", "Bearer 0000")])),
        401
    );
}

#[test]
fn the_page_cookie_is_not_accepted_on_a_cli_route() {
    // Spec 14 asks for this crossed case explicitly.
    let h = Harness::start();
    let cookie = format!("artefacto_session={}", h.shared.page_cookie);
    assert_eq!(
        Harness::status_of(&h.get("/cli/status", &[("Cookie", &cookie)])),
        401,
        "a compromised page must not be able to drive the CLI surface"
    );
}

#[test]
fn no_response_ever_carries_cors_headers() {
    let h = Harness::start();
    let auth = format!("Bearer {}", h.secret);
    for path in ["/healthz", "/cli/status", "/nope"] {
        let r = h.get(path, &[("Authorization", &auth), ("Origin", "http://evil.example.com")]);
        assert!(
            !r.to_ascii_lowercase().contains("access-control-allow"),
            "{path} must never answer a cross-origin request"
        );
    }
}

#[test]
fn an_unknown_route_is_a_json_404() {
    let h = Harness::start();
    let r = h.get("/nope", &[]);
    assert_eq!(Harness::status_of(&r), 404);
    assert!(r.contains("\"ok\":false"));
}

#[test]
fn an_unauthenticated_request_does_not_keep_the_daemon_alive() {
    // Self-exit is about connections, not traffic. A stranger hitting the port
    // must not be able to hold a daemon open indefinitely.
    let h = Harness::start_with_idle(std::time::Duration::from_millis(200));
    h.get("/cli/status", &[]); // 401, unauthenticated
    h.wait_for(
        || h.shared.stopping() || !h.is_serving(),
        "the server should still self-exit despite unauthenticated traffic",
    );
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test --test server_security`
Expected: FAIL to compile, `could not find 'http' in 'server'`.

- [ ] **Step 4: Implement the skeleton**

```rust
//! Routing and the two guards every route sits behind.
//!
//! Lock discipline: `handle` takes `core` only for short, non-blocking
//! updates, and never while holding `log` or `sockets`.

use crate::server::log::EventLog;
use crate::server::state_dir::derive_credential;
use anyhow::Result;
use std::collections::HashMap;
use std::io::Cursor;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tiny_http::{Header, Request, Response};

pub struct Core {
    /// token -> (artifact, issued). Plan 2b adds the folded review state.
    pub bootstrap: HashMap<String, (String, Instant)>,
    pub last_request_at: Instant,
}

pub struct Shared {
    pub log: Mutex<EventLog>,
    pub core: Mutex<Core>,
    pub sockets: crate::server::socket::PageSockets,
    pub secret: String,
    /// Derived from `secret`, so it survives a restart. Never the secret.
    pub page_cookie: String,
    pub port: u16,
    stopping: AtomicBool,
}

impl Shared {
    pub fn new(dir: &Path, secret: String, port: u16) -> Result<Shared> {
        Ok(Shared {
            log: Mutex::new(EventLog::open(dir)?),
            core: Mutex::new(Core {
                bootstrap: HashMap::new(),
                last_request_at: Instant::now(),
            }),
            sockets: Default::default(),
            page_cookie: derive_credential(&secret, "page-cookie"),
            secret,
            port,
            stopping: AtomicBool::new(false),
        })
    }

    pub fn stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    pub fn request_stop(&self) {
        self.stopping.store(true, Ordering::SeqCst);
    }
}

/// The accept loop. One thread per request, so plan 2b's 90-second long poll
/// blocks only its own thread. A fixed worker pool would let N concurrent
/// polls starve every other route.
pub fn run(shared: Arc<Shared>, server: Arc<tiny_http::Server>, idle: Duration) {
    loop {
        if shared.stopping() {
            return;
        }
        match server.recv_timeout(Duration::from_millis(250)) {
            Ok(Some(request)) => {
                let shared = Arc::clone(&shared);
                std::thread::spawn(move || handle(shared, request));
            }
            // `unblock` also produces this, which is why the stopping flag is
            // checked at the top rather than relying on the timeout alone.
            Ok(None) => {
                if should_self_exit(&shared, idle) {
                    return;
                }
            }
            Err(_) => return,
        }
    }
}

/// Spec 4.2: exit when **no page and no agent** have been connected for the
/// idle window. Traffic is not the measure — an unauthenticated stranger must
/// not be able to hold the daemon open, and a page connected over a WebSocket
/// sends no further HTTP requests but is very much present.
fn should_self_exit(shared: &Arc<Shared>, idle: Duration) -> bool {
    if crate::server::socket::page_count(shared) > 0 {
        return false;
    }
    // Plan 2b adds `|| lease::current(shared).is_some()` here.
    let quiet = {
        let core = shared.core.lock().unwrap();
        core.last_request_at.elapsed()
    };
    quiet > idle
}

fn handle(shared: Arc<Shared>, request: Request) {
    if !host_ok(&request, shared.port) {
        let _ = request.respond(error_response(
            421,
            "bad_host",
            "this server answers only on 127.0.0.1 by address",
        ));
        return;
    }
    let url = request.url().to_string();

    if url == "/healthz" {
        let _ = request.respond(json_response(200, "{\"ok\":true}"));
        return;
    }
    if url == "/ws" {
        return crate::server::socket::handle_upgrade(&shared, request);
    }
    if let Some(token) = url.strip_prefix("/b/") {
        return crate::server::page::handle_bootstrap(&shared, request, token);
    }
    if let Some(artifact) = url.strip_prefix("/a/") {
        return crate::server::page::serve_page(&shared, request, artifact);
    }
    if let Some(rest) = url.strip_prefix("/cli/") {
        if !bearer_ok(&request, &shared.secret) {
            let _ = request.respond(error_response(401, "unauthorized", "bearer secret required"));
            return;
        }
        // Only an authenticated call counts as activity.
        shared.core.lock().unwrap().last_request_at = Instant::now();
        return cli_route(&shared, request, rest);
    }
    let _ = request.respond(error_response(404, "not_found", "no such route"));
}

/// Plan 2b fills this in. It answers now so the bearer guard protects
/// something real and `status` has a shape from the start.
fn cli_route(_shared: &Arc<Shared>, request: Request, rest: &str) {
    match rest {
        "status" => {
            let _ = request.respond(json_response(200, "{\"ok\":true,\"artifacts\":[]}"));
        }
        _ => {
            let _ = request.respond(error_response(404, "not_found", "no such route"));
        }
    }
}

/// `HeaderField::equiv` needs a `&'static str`; a `&str` parameter does not
/// compile here (E0521).
pub fn header(req: &Request, name: &'static str) -> Option<String> {
    req.headers()
        .iter()
        .find(|h| h.field.equiv(name))
        .map(|h| h.value.as_str().to_string())
}

/// Exactly `127.0.0.1:<port>`. Not `localhost`, not a bare address. This is
/// the DNS-rebinding defense and the only thing between a malicious page and
/// this server.
pub fn host_ok(req: &Request, port: u16) -> bool {
    header(req, "Host").is_some_and(|h| h == format!("127.0.0.1:{port}"))
}

/// Compared without an early return, so timing does not leak how much of the
/// secret was right.
pub fn bearer_ok(req: &Request, secret: &str) -> bool {
    let Some(value) = header(req, "Authorization") else {
        return false;
    };
    let Some(given) = value.strip_prefix("Bearer ") else {
        return false;
    };
    constant_time_eq(given.as_bytes(), secret.as_bytes())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

pub fn json_response(status: u16, body: &str) -> Response<Cursor<Vec<u8>>> {
    Response::from_string(body).with_status_code(status).with_header(
        Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).expect("static header"),
    )
}

/// One error shape for every route: a CLI parsing a failure should never have
/// to tell an HTML error page from a JSON one.
pub fn error_response(status: u16, code: &str, message: &str) -> Response<Cursor<Vec<u8>>> {
    let body = serde_json::json!({ "ok": false, "error": { "code": code, "message": message } });
    json_response(status, &body.to_string())
}
```

`Harness::is_serving` is a one-line helper that attempts a TCP connect to the port and reports whether it succeeded.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --test server_security`
Expected: PASS, 10 tests.

- [ ] **Step 6: Verify the gate**

Run: `cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --all`
Expected: all clean.

- [ ] **Step 7: Commit**

```bash
git add src/server/http.rs src/server/mod.rs tests/support/ tests/server_security.rs
git commit -m "feat(server): http skeleton, host and bearer guards, stoppable harness"
```

---

### Task 5: Bootstrap, the page cookie, Origin, and the CSP nonce

**Why this task exists:** the page cannot carry the bearer secret — a token in a URL leaks into history, referrers, and the agent's transcript. Spec 8 fixes the alternative: a one-time bootstrap URL that sets an `HttpOnly; SameSite=Strict` cookie and redirects to a tokenless URL, with a per-response CSP nonce.

**What the earlier draft got wrong:**

- **The served page's own meta CSP blocks the WebSocket.** `src/plan/render.rs:18` defines `default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; img-src data:; font-src data:` and `render.rs:588` emits it as `<meta http-equiv>`. A document under both a meta policy and a header policy must satisfy **both**; the meta policy has no `connect-src`, so it falls back to `default-src 'none'` and `ws://127.0.0.1:<port>` is refused no matter what header the server sends. The plan's own verification step told the implementer to confirm the socket connects. It could not.
- **Only one of three tags got a nonce.** The rendered page carries two `<script` tags (`<script type="application/json" id="plan-data">` and `<script>`) and one `<style`. A `replace("<script>", …)` stamps one of them. The test asserted only that *some* tag had a nonce, so it passed while the page stayed broken.
- **`origin_ok` accepted a missing `Origin`.** Correct for a top-level navigation, wrong for a page write or a socket handshake, where spec 8 requires "the cookie plus an exact Origin match".

**Files:**
- Create: `src/server/page.rs`
- Modify: `src/server/http.rs`, `src/server/mod.rs`, `tests/server_security.rs`

**Interfaces:**
- Consumes: `http::{Shared, header, error_response}` (Task 4), `state_dir::new_secret` (Task 1).
- Produces:
  - `pub const BOOTSTRAP_TTL: Duration = Duration::from_secs(300);`
  - `pub const COOKIE_NAME: &str = "artefacto_session";`
  - `pub fn mint_bootstrap(shared: &Shared, artifact: &str) -> anyhow::Result<String>`
  - `pub fn mint_bootstrap_aged(shared: &Shared, artifact: &str, age: Duration) -> anyhow::Result<String>`
  - `pub fn bootstrap_url(port: u16, token: &str) -> String`
  - `pub fn handle_bootstrap(shared: &Arc<Shared>, request: Request, token: &str)`
  - `pub fn serve_page(shared: &Arc<Shared>, request: Request, artifact: &str)`
  - `pub fn cookie_ok(req: &Request, shared: &Shared) -> bool`
  - `pub fn origin_ok_navigation(req: &Request, port: u16) -> bool` — absent is allowed
  - `pub fn origin_ok_strict(req: &Request, port: u16) -> bool` — absent is refused
  - `pub fn csp_header(port: u16, nonce: &str) -> Header`
  - `pub fn nonce() -> String`
  - `pub fn stamp_nonce(html: &str, nonce: &str) -> String`
  - `pub fn placeholder_document(artifact: &str) -> String`
  - `pub fn valid_artifact_id(id: &str) -> bool`

**What `/a/<artifact>` serves in this plan.** Nothing has been pushed yet — `push` is plan 2b — so this plan serves a **placeholder document** it defines itself, containing one inline `<style>` and one inline `<script>` so the nonce path is exercised deterministically. `stamp_nonce` is separately unit-tested against the **real** renderer output from `tests/fixtures/plan/kitchen-sink.json`, so the transformation is proven against the page it will actually serve once plan 2b lands. The earlier draft left the CSP test depending on whatever a never-specified placeholder happened to contain.

**One decision worth writing down:** the page cookie is a single server-wide value, so a bootstrap token minted for artifact A yields a cookie that also opens artifact B. The spec does not require per-artifact isolation and the reviewer is a single trusted user, so this is fine — but it is a decision, not an accident.

- [ ] **Step 1: Write the failing tests**

Append to `tests/server_security.rs`:

```rust
#[test]
fn a_bootstrap_token_sets_a_cookie_and_redirects_once() {
    let h = Harness::start();
    let token = h.mint_bootstrap("plan:x");

    let first = h.get(&format!("/b/{token}"), &[]);
    assert_eq!(Harness::status_of(&first), 302);
    let lower = first.to_ascii_lowercase();
    assert!(lower.contains("set-cookie:"));
    assert!(lower.contains("httponly"), "the page's own script must not read it");
    assert!(lower.contains("samesite=strict"));
    assert!(!lower.contains(&token.to_ascii_lowercase()), "the redirect target carries no token");

    assert_eq!(
        Harness::status_of(&h.get(&format!("/b/{token}"), &[])),
        403,
        "a bootstrap token is single use"
    );
}

#[test]
fn an_expired_bootstrap_token_is_refused_and_still_consumed() {
    let h = Harness::start();
    let token = h.mint_bootstrap_aged("plan:x", std::time::Duration::from_secs(301));
    assert_eq!(Harness::status_of(&h.get(&format!("/b/{token}"), &[])), 403);
    assert_eq!(
        Harness::status_of(&h.get(&format!("/b/{token}"), &[])),
        403,
        "an expired token is spent on presentation, so a leaked URL is never retryable"
    );
}

#[test]
fn an_artifact_id_with_a_newline_is_refused_at_mint_time() {
    let h = Harness::start();
    assert!(
        h.shared_mint("plan:x\r\nSet-Cookie: injected=1").is_err(),
        "the id reaches a Location header, so a CR or LF would be header injection"
    );
}

#[test]
fn the_page_cookie_survives_a_restart() {
    let h = Harness::start();
    let cookie = h.session_cookie("plan:x");
    let h = h.restart();
    assert_eq!(
        Harness::status_of(&h.get("/a/plan:x", &[("Cookie", &cookie)])),
        200,
        "the cookie is derived from the persisted secret, so a restart does not log the reviewer out"
    );
}

#[test]
fn a_page_route_without_the_cookie_is_refused() {
    let h = Harness::start();
    assert_eq!(Harness::status_of(&h.get("/a/plan:x", &[])), 401);
}

#[test]
fn a_page_route_with_a_foreign_origin_is_refused() {
    let h = Harness::start();
    let cookie = h.session_cookie("plan:x");
    assert_eq!(
        Harness::status_of(&h.get("/a/plan:x", &[("Cookie", &cookie), ("Origin", "http://evil.example.com")])),
        403
    );
}

#[test]
fn the_bearer_secret_is_not_accepted_on_a_page_route() {
    // The other half of spec 14's crossed-credential pair.
    let h = Harness::start();
    let auth = format!("Bearer {}", h.secret);
    assert_eq!(
        Harness::status_of(&h.get("/a/plan:x", &[("Authorization", &auth)])),
        401,
        "page routes accept the cookie and nothing else"
    );
}

#[test]
fn the_served_page_carries_a_nonce_csp_and_no_meta_policy() {
    let h = Harness::start();
    let cookie = h.session_cookie("plan:x");
    let r = h.get("/a/plan:x", &[("Cookie", &cookie)]);
    assert_eq!(Harness::status_of(&r), 200);

    let csp = r
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("content-security-policy:"))
        .expect("every page response carries a CSP header");
    assert!(csp.contains("default-src 'none'"));
    assert!(csp.contains("'nonce-"));
    assert!(!csp.contains("'unsafe-inline'"), "the nonce replaces unsafe-inline");
    assert!(csp.contains(&format!("connect-src ws://127.0.0.1:{}", h.port)));
    assert!(csp.contains("frame-ancestors 'none'"));
    assert!(!csp.contains("sandbox"), "a sandbox directive makes the origin opaque and kills the cookie");

    assert!(
        !r.contains("http-equiv=\"Content-Security-Policy\""),
        "the page's own meta policy has no connect-src, so leaving it in blocks the socket \
         whatever the header says"
    );
}

#[test]
fn every_inline_tag_in_the_served_page_carries_the_nonce() {
    let h = Harness::start();
    let cookie = h.session_cookie("plan:x");
    let r = h.get("/a/plan:x", &[("Cookie", &cookie)]);
    let body = r.split("\r\n\r\n").nth(1).expect("a body");
    let nonce = r
        .split("'nonce-")
        .nth(1)
        .and_then(|s| s.split('\'').next())
        .expect("a nonce in the header");

    let opens = body.matches("<script").count() + body.matches("<style").count();
    let stamped = body.matches(&format!("nonce=\"{nonce}\"")).count();
    assert!(opens > 0, "the document must actually contain inline tags to be a real test");
    assert_eq!(stamped, opens, "one unstamped tag is one blocked tag");
}

#[test]
fn two_responses_never_share_a_nonce() {
    let h = Harness::start();
    let cookie = h.session_cookie("plan:x");
    let grab = |r: &str| {
        r.split("'nonce-").nth(1).and_then(|s| s.split('\'').next()).map(str::to_string).unwrap()
    };
    let one = grab(&h.get("/a/plan:x", &[("Cookie", &cookie)]));
    let two = grab(&h.get("/a/plan:x", &[("Cookie", &cookie)]));
    assert_ne!(one, two, "a per-response nonce is the whole point");
}
```

And a unit test in `src/server/page.rs` proving the transformation against the **real** renderer, not the placeholder:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// The document this will serve once plan 2b lands is the real plan
    /// render, so the transformation is pinned against that, not against the
    /// placeholder this plan happens to serve.
    fn real_render() -> String {
        let raw = std::fs::read_to_string("tests/fixtures/plan/kitchen-sink.json").unwrap();
        let plan = crate::plan::model::parse(&raw).expect("the fixture is valid");
        crate::plan::render::render(&plan)
    }

    #[test]
    fn stamping_covers_every_inline_tag_of_the_real_page() {
        let html = real_render();
        assert!(html.matches("<script").count() >= 2, "the real page has a data island and a script");
        assert!(html.contains("<style"));

        let out = stamp_nonce(&html, "abc123");
        let opens = out.matches("<script").count() + out.matches("<style").count();
        assert_eq!(out.matches("nonce=\"abc123\"").count(), opens);
    }

    #[test]
    fn stamping_removes_the_pages_own_meta_policy() {
        let html = real_render();
        assert!(html.contains("http-equiv=\"Content-Security-Policy\""), "plan 1 emits one");
        let out = stamp_nonce(&html, "abc123");
        assert!(!out.contains("http-equiv=\"Content-Security-Policy\""));
        assert!(!out.contains("unsafe-inline"), "no trace of the static policy is left");
    }

    #[test]
    fn stamping_leaves_closing_tags_alone() {
        let out = stamp_nonce("<script>x</script><style>y</style>", "n");
        assert_eq!(out, "<script nonce=\"n\">x</script><style nonce=\"n\">y</style>");
    }

    #[test]
    fn an_id_with_control_characters_is_rejected() {
        assert!(valid_artifact_id("plan:auth-refactor"));
        assert!(!valid_artifact_id("plan:x\r\nSet-Cookie: a=b"));
        assert!(!valid_artifact_id("plan:x\n"));
        assert!(!valid_artifact_id(""));
        assert!(!valid_artifact_id("../etc/passwd"));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test server_security`
Expected: FAIL to compile, `could not find 'page' in 'server'`.

- [ ] **Step 3: Implement the module**

Key pieces; the rest follows the interfaces above.

```rust
/// Removes the page's own meta CSP and stamps `nonce` onto every inline
/// `<script` and `<style` opening tag.
///
/// Both halves must change together: a header naming a nonce the document
/// does not carry blanks the page, and leaving the meta policy in place
/// blocks the WebSocket because that policy has no `connect-src` and both
/// policies apply.
///
/// `</script>` and `</style>` are untouched: they begin `</`, so neither
/// matches the `<script` / `<style` prefixes searched for here.
pub fn stamp_nonce(html: &str, nonce: &str) -> String {
    let stripped = strip_meta_csp(html);
    stripped
        .replace("<script", &format!("<script nonce=\"{nonce}\""))
        .replace("<style", &format!("<style nonce=\"{nonce}\""))
}

fn strip_meta_csp(html: &str) -> String {
    const NEEDLE: &str = "<meta http-equiv=\"Content-Security-Policy\"";
    let Some(start) = html.find(NEEDLE) else {
        return html.to_string();
    };
    // The renderer emits no `>` inside the attribute values, so the next `>`
    // ends the tag. A unit test against the real render pins this.
    let Some(end) = html[start..].find('>') else {
        return html.to_string();
    };
    let mut out = String::with_capacity(html.len());
    out.push_str(&html[..start]);
    out.push_str(&html[start + end + 1..]);
    out
}

/// Artifact ids reach a `Location` header and a URL path. A CR or LF would be
/// header injection, so the shape is checked once, at mint time.
pub fn valid_artifact_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && !id.contains("..")
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '-' | '_' | '.'))
}

/// Absent `Origin` is allowed: a top-level navigation sends none.
pub fn origin_ok_navigation(req: &Request, port: u16) -> bool {
    match header(req, "Origin") {
        None => true,
        Some(o) => o == format!("http://127.0.0.1:{port}"),
    }
}

/// Absent `Origin` is refused. Spec 8 requires "the cookie plus an exact
/// Origin match" on page writes and the socket handshake, and a browser
/// always sends `Origin` on both — so absent means a non-browser client.
pub fn origin_ok_strict(req: &Request, port: u16) -> bool {
    header(req, "Origin").is_some_and(|o| o == format!("http://127.0.0.1:{port}"))
}

/// Spec 8. No `sandbox` directive: it makes the origin opaque, which breaks
/// the cookie the page authenticates with.
pub fn csp_header(port: u16, nonce: &str) -> Header {
    let policy = format!(
        "default-src 'none'; script-src 'nonce-{nonce}'; style-src 'nonce-{nonce}'; \
         img-src data:; font-src data:; connect-src ws://127.0.0.1:{port}; \
         base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
    );
    Header::from_bytes(&b"Content-Security-Policy"[..], policy.as_bytes()).expect("csp header")
}
```

`mint_bootstrap*` returns `Err` for an id failing `valid_artifact_id`. `consume_bootstrap` removes the token whatever the outcome, so an expired token is spent rather than retryable. `serve_page` checks `cookie_ok` then `origin_ok_navigation`, renders `placeholder_document(artifact)` through `stamp_nonce`, and answers with `csp_header`. `handle_bootstrap` sets `{COOKIE_NAME}={shared.page_cookie}; HttpOnly; SameSite=Strict; Path=/` and a `Location` of `/a/{artifact}`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --test server_security && cargo test --lib server::page`
Expected: PASS, 20 integration tests and 4 unit tests.

- [ ] **Step 5: Verify the gate, then commit**

```bash
cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --all
git add src/server/page.rs src/server/http.rs src/server/mod.rs tests/
git commit -m "feat(server): bootstrap cookie auth, strict origin, and a working CSP nonce"
```

---

### Task 6: The page WebSocket

**Why this task exists:** the socket is how plan 3's page hears anything, and this plan proves the transport before that page exists.

**What the earlier draft got wrong, and it is the subtlest failure in the whole review:** the connection thread held the per-page mutex across a blocking `WebSocket::read()`. A page that is not sending holds that lock indefinitely. `broadcast` then took the global registry lock and blocked on that same per-page mutex while holding it. So one quiet tab — the normal state of a page being read — deadlocked every broadcast, `page_count`, and, through `page_count`, the accept loop itself. The plan's own Task 6 test connected a silent page and then broadcast to it, so the step claiming "PASS, 18 tests" could not have passed.

**The shape that fixes it:** each connection owns exactly one thread that reads, and one writer thread that owns the write half and drains an `mpsc::Receiver<String>`. The registry holds only `Sender`s. Sending never blocks a lock, because the send is to a channel, and the actual socket write happens on the writer thread. `broadcast` clones the senders out under the lock, releases, then sends.

**Files:**
- Create: `src/server/socket.rs`
- Modify: `src/server/http.rs`, `src/server/mod.rs`, `tests/support/mod.rs`, `tests/server_security.rs`

**Interfaces:**
- Produces:
  - `pub struct PageSockets { pages: Mutex<Vec<PageHandle>> }`, `Default`
  - `pub struct PageHandle { id: u64, tx: mpsc::Sender<String> }`
  - `pub fn handle_upgrade(shared: &Arc<Shared>, request: Request)`
  - `pub fn broadcast(shared: &Shared, frame: &Frame)`
  - `pub fn page_count(shared: &Shared) -> usize`
- Test side: `Harness::connect_page`, `connect_page_raw`, `FakePage::{next_frame, send}`

**A harness detail that would have caused intermittent failures:** the earlier draft's fake page took the `WebSocket` from `tungstenite::connect`, called `into_inner()` for the `TcpStream`, and rebuilt it with `from_raw_socket`. Any bytes the handshake had already read past the HTTP response live in the discarded codec buffer, so a frame broadcast immediately after the upgrade is lost. Keep the type `WebSocket<MaybeTlsStream<TcpStream>>` and never unwrap it.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn a_socket_without_the_cookie_is_refused() {
    let h = Harness::start();
    assert!(h.connect_page_raw(None, Some(&h.origin())).is_err());
}

#[test]
fn a_socket_with_no_origin_header_is_refused() {
    let h = Harness::start();
    let cookie = h.session_cookie("plan:x");
    assert!(
        h.connect_page_raw(Some(&cookie), None).is_err(),
        "spec 8 requires an exact Origin on the handshake; a browser always sends one, \
         so absent means a non-browser client"
    );
}

#[test]
fn a_socket_with_a_foreign_origin_is_refused() {
    let h = Harness::start();
    let cookie = h.session_cookie("plan:x");
    assert!(h.connect_page_raw(Some(&cookie), Some("http://evil.example.com")).is_err());
}

#[test]
fn a_quiet_page_does_not_block_a_broadcast() {
    // The exact deadlock the earlier draft shipped: this page never sends.
    let h = Harness::start();
    let mut page = h.connect_page();
    h.broadcast_test_frame(7);
    let frame = page.next_frame();
    assert_eq!(frame["format"], "artefacto.frame/1");
    assert_eq!(frame["seq"], 7);
}

#[test]
fn a_quiet_page_does_not_block_the_accept_loop() {
    let h = Harness::start();
    let _page = h.connect_page();
    assert_eq!(
        Harness::status_of(&h.get("/healthz", &[])),
        200,
        "an open, silent page must not stop the server answering"
    );
}

#[test]
fn two_pages_both_receive_a_broadcast() {
    let h = Harness::start();
    let mut a = h.connect_page();
    let mut b = h.connect_page();
    h.wait_for(|| h.page_count() == 2, "both pages registered");
    h.broadcast_test_frame(3);
    assert_eq!(a.next_frame()["seq"], 3);
    assert_eq!(b.next_frame()["seq"], 3);
}

#[test]
fn a_closed_page_is_dropped_from_the_registry() {
    let h = Harness::start();
    let page = h.connect_page();
    h.wait_for(|| h.page_count() == 1, "the page registered");
    drop(page);
    h.wait_for(|| h.page_count() == 0, "the closed page should be dropped");
}

#[test]
fn an_open_page_keeps_the_server_alive() {
    // Spec 4.2: self-exit is "no page and no agent", not "no traffic".
    let h = Harness::start_with_idle(std::time::Duration::from_millis(200));
    let _page = h.connect_page();
    std::thread::sleep(std::time::Duration::from_millis(600));
    assert_eq!(
        Harness::status_of(&h.get("/healthz", &[])),
        200,
        "a page open and quiet must not let the daemon exit under the reviewer"
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test server_security`
Expected: FAIL to compile, `could not find 'socket' in 'server'`.

- [ ] **Step 3: Implement the module**

```rust
//! The page's WebSocket. One reader thread and one writer thread per page.
//!
//! Lock discipline: takes `sockets` only, for short non-blocking work. It
//! never writes to a socket while holding it — that is what the channel is
//! for — and it never takes `core`.

use crate::server::event::Frame;
use crate::server::http::{error_response, header, Shared};
use crate::server::page::{cookie_ok, origin_ok_strict};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use tiny_http::{Header, Request, Response};
use tungstenite::handshake::derive_accept_key;
use tungstenite::protocol::{Role, WebSocket};
use tungstenite::Message;

/// `Request::upgrade` returns a boxed `ReadWrite`, which does not itself
/// implement `Read` and `Write`. Delegating through a newtype is the fix.
struct Sock(Box<dyn tiny_http::ReadWrite + Send>);

impl Read for Sock {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}

impl Write for Sock {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

pub struct PageHandle {
    id: u64,
    tx: mpsc::Sender<String>,
}

#[derive(Default)]
pub struct PageSockets {
    pages: Mutex<Vec<PageHandle>>,
    next_id: AtomicU64,
}

pub fn page_count(shared: &Shared) -> usize {
    shared.sockets.pages.lock().unwrap().len()
}

/// Clones the senders out under the lock, releases it, then sends. Nothing
/// touches a socket while the registry is locked, so one stalled tab cannot
/// stall the others or the accept loop.
pub fn broadcast(shared: &Shared, frame: &Frame) {
    let text = serde_json::to_string(frame).expect("a frame always serializes");
    let senders: Vec<(u64, mpsc::Sender<String>)> = {
        let pages = shared.sockets.pages.lock().unwrap();
        pages.iter().map(|p| (p.id, p.tx.clone())).collect()
    };
    let mut gone = Vec::new();
    for (id, tx) in senders {
        if tx.send(text.clone()).is_err() {
            gone.push(id);
        }
    }
    if !gone.is_empty() {
        let mut pages = shared.sockets.pages.lock().unwrap();
        pages.retain(|p| !gone.contains(&p.id));
    }
}

pub fn handle_upgrade(shared: &Arc<Shared>, request: Request) {
    if !cookie_ok(&request, shared) {
        let _ = request.respond(error_response(401, "unauthorized", "no session cookie"));
        return;
    }
    if !origin_ok_strict(&request, shared.port) {
        let _ = request.respond(error_response(403, "bad_origin", "exact origin required"));
        return;
    }
    let Some(key) = header(&request, "Sec-WebSocket-Key") else {
        let _ = request.respond(error_response(400, "bad_handshake", "no Sec-WebSocket-Key"));
        return;
    };
    let accept = derive_accept_key(key.as_bytes());
    let response = Response::empty(101)
        .with_header(Header::from_bytes(&b"Upgrade"[..], &b"websocket"[..]).expect("static"))
        .with_header(Header::from_bytes(&b"Connection"[..], &b"Upgrade"[..]).expect("static"))
        .with_header(Header::from_bytes(&b"Sec-WebSocket-Accept"[..], accept.as_bytes()).expect("accept"));

    let stream = request.upgrade("websocket", response);
    let ws = WebSocket::from_raw_socket(Sock(stream), Role::Server, None);
    let id = shared.sockets.next_id.fetch_add(1, Ordering::SeqCst);
    let (tx, rx) = mpsc::channel::<String>();

    // The writer owns the socket. The reader borrows it back through the same
    // Mutex, but only ever with `try`-style short operations, so neither can
    // hold it across a blocking call.
    let ws = Arc::new(Mutex::new(ws));
    shared.sockets.pages.lock().unwrap().push(PageHandle { id, tx });

    let writer_ws = Arc::clone(&ws);
    let writer = std::thread::spawn(move || {
        for text in rx {
            let mut guard = writer_ws.lock().unwrap();
            if guard.send(Message::text(text)).is_err() {
                break;
            }
        }
    });

    // Reading happens on this thread with a read timeout, so the guard is held
    // for a bounded moment rather than until the page decides to speak.
    read_loop(shared, &ws, id);
    let _ = writer;
    let mut pages = shared.sockets.pages.lock().unwrap();
    pages.retain(|p| p.id != id);
}
```

**The one place the reader and writer must not fight.** `read_loop` sets a read timeout on the underlying stream before each `read()` so the call returns rather than blocking forever, releases the guard between attempts, and treats a timeout as "nothing to do". Plan 2b gives inbound messages meaning; here they are counted and dropped. The invariant to preserve, and to state in the module comment: **no thread holds the socket guard across an unbounded operation.**

- [ ] **Step 4: Add the fake page client**

Append to `tests/support/mod.rs`:

```rust
use tungstenite::client::IntoClientRequest;

pub struct FakePage {
    ws: tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
}

impl FakePage {
    pub fn next_frame(&mut self) -> serde_json::Value {
        loop {
            match self.ws.read().expect("the socket must stay open") {
                tungstenite::Message::Text(t) => {
                    return serde_json::from_str(&t).expect("frames are JSON")
                }
                _ => continue,
            }
        }
    }

    pub fn send(&mut self, value: serde_json::Value) {
        self.ws.send(tungstenite::Message::text(value.to_string())).expect("send");
    }
}

impl Harness {
    pub fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn connect_page(&self) -> FakePage {
        let cookie = self.session_cookie("plan:x");
        self.connect_page_raw(Some(&cookie), Some(&self.origin()))
            .expect("a cookie and a matching origin must be enough")
    }

    pub fn connect_page_raw(
        &self,
        cookie: Option<&str>,
        origin: Option<&str>,
    ) -> Result<FakePage, String> {
        let mut req = format!("ws://127.0.0.1:{}/ws", self.port)
            .into_client_request()
            .map_err(|e| e.to_string())?;
        if let Some(c) = cookie {
            req.headers_mut().insert("Cookie", c.parse().unwrap());
        }
        if let Some(o) = origin {
            req.headers_mut().insert("Origin", o.parse().unwrap());
        }
        // Keep the stream type tungstenite handed back. Unwrapping it and
        // rebuilding with from_raw_socket discards the codec's buffer, losing
        // any frame that arrived immediately after the handshake.
        tungstenite::connect(req).map(|(ws, _)| FakePage { ws }).map_err(|e| e.to_string())
    }
}
```

- [ ] **Step 5: Run the tests, verify the gate, commit**

Run: `cargo test --test server_security`
Expected: PASS, 28 tests.

```bash
cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --all
git add src/server/socket.rs src/server/http.rs src/server/mod.rs tests/
git commit -m "feat(server): page websocket with a per-page writer, no I/O under a lock"
```

---

### Task 7: `serve`, `stop`, `status`, `open`, and the daemon

**Why this task exists:** this is the task that makes the plan runnable. It is also where the earlier draft's worst bug lived.

**What the earlier draft got wrong:**

- **It built the `tiny_http::Server` before forking.** `Server::from_listener` spawns its accept thread at construction (`tiny_http-0.12.0/src/lib.rs:288`), and `fork()` keeps only the calling thread. The grandchild would have had a listening socket and nothing accepting on it: every request would hang, `last_activity` would never update, and the daemon would self-exit half an hour later having served nothing. `--foreground` would have passed every test.
- **`fork() == -1` was treated as the parent branch**, so a failed fork silently exited success.
- **The parent exited before the grandchild wrote `server.json`**, so `status` immediately after `serve` was a race — in the test suite and in real use.
- **`serve` deleted `server.json` on shutdown**, discarding the port and secret that the plan's own restart test requires.

**Files:**
- Create: `src/server/daemon.rs`, `src/commands/serve.rs`, `src/client.rs`, `tests/server_lifecycle.rs`
- Modify: `src/cli.rs`, `src/commands/mod.rs`, `src/server/mod.rs`, `src/lib.rs`

**Interfaces:**
- Produces:
  - `pub fn daemonize(log_path: &Path) -> anyhow::Result<Readiness>` — returns only in the grandchild
  - `pub struct Readiness` with `fn ready(self)` and `fn fail(self, msg: &str) -> !`
  - `pub fn serve(args: &ServeArgs) -> anyhow::Result<()>`, `stop`, `status`, `open`
  - `pub struct Client` — bearer-authenticated, maps a missing or dead server to exit **4**
  - `pub fn parse_duration(s: &str) -> anyhow::Result<Option<Duration>>` — `15m`, `90s`, `off`

**`parse_duration` returns `Option`** because spec 16 says idle and away can be set to `off`. The earlier draft took these as `String` and never parsed them.

- [ ] **Step 1: Write the failing tests**

Create `tests/server_lifecycle.rs`. The tests that matter most:

```rust
#[test]
fn a_daemonized_server_actually_answers() {
    // The earlier draft would have passed every other test in this file while
    // failing this one: --foreground hides a daemon that cannot accept.
    let repo = git_repo();
    run(&repo, &["serve", "--no-open"]).success();
    let port = port_of(&repo);
    let body = reqwest_free_get(port, "/healthz");
    stop(&repo);
    assert!(body.contains("\"ok\":true"), "the daemon must serve, not just exist");
}

#[test]
fn the_daemon_is_reparented_to_init() {
    let repo = git_repo();
    run(&repo, &["serve", "--no-open"]).success();
    let pid = pid_of(&repo);
    let ppid = parent_of(pid);
    stop(&repo);
    assert_eq!(ppid, 1, "PPID 1 is what proves it detached rather than merely surviving");
}

#[test]
fn status_immediately_after_serve_is_not_a_race() {
    // serve returns only once the grandchild has signalled readiness.
    for _ in 0..20 {
        let repo = git_repo();
        run(&repo, &["serve", "--no-open"]).success();
        run(&repo, &["status", "--json"]).success();
        stop(&repo);
    }
}

#[test]
fn a_restart_rebinds_the_same_port_and_keeps_the_secret() {
    let repo = git_repo();
    run(&repo, &["serve", "--no-open"]).success();
    let (first_port, first_secret) = (port_of(&repo), secret_of(&repo));
    stop(&repo);
    run(&repo, &["serve", "--no-open"]).success();
    let (second_port, second_secret) = (port_of(&repo), secret_of(&repo));
    stop(&repo);
    assert_eq!(first_port, second_port, "an open page must be able to reconnect");
    assert_eq!(first_secret, second_secret, "and its cookie must still be valid");
}

#[test]
fn stop_does_not_discard_the_port_and_secret() {
    let repo = git_repo();
    run(&repo, &["serve", "--no-open"]).success();
    stop(&repo);
    assert!(
        server_json_path(&repo).exists(),
        "server.json carries the port and secret a restart reuses; a dead pid inside it \
         already means 'no server'"
    );
}

#[test]
fn status_without_a_server_exits_4() {
    let repo = git_repo();
    run(&repo, &["status", "--json"]).code(4);
}

#[test]
fn status_json_never_prints_the_secret() {
    let repo = git_repo();
    run(&repo, &["serve", "--no-open"]).success();
    let secret = secret_of(&repo);
    let out = run(&repo, &["status", "--json"]).stdout_string();
    stop(&repo);
    assert!(!out.contains(&secret), "a token must not be obtainable from something that reads status");
}

#[test]
fn two_concurrent_serves_start_one_daemon() {
    let repo = git_repo();
    let a = spawn(&repo, &["serve", "--no-open"]);
    let b = spawn(&repo, &["serve", "--no-open"]);
    a.wait();
    b.wait();
    let pid = pid_of(&repo);
    stop(&repo);
    assert!(is_alive(pid), "the startup lock keeps the two from racing into two daemons");
}

#[test]
fn open_prints_a_bootstrap_url() {
    let repo = git_repo();
    run(&repo, &["serve", "--no-open"]).success();
    let out = run(&repo, &["open", "--artifact", "plan:x", "--no-open"]).stdout_string();
    stop(&repo);
    assert!(out.contains(&format!("http://127.0.0.1:{}", port_of(&repo))));
    assert!(out.contains("/b/"), "the bootstrap path is what sets the cookie");
}

#[test]
fn durations_parse_including_off() {
    use artefacto::commands::serve::parse_duration;
    assert_eq!(parse_duration("15m").unwrap(), Some(std::time::Duration::from_secs(900)));
    assert_eq!(parse_duration("90s").unwrap(), Some(std::time::Duration::from_secs(90)));
    assert_eq!(parse_duration("off").unwrap(), None, "spec 16 allows off");
    assert!(parse_duration("later").is_err());
}
```

Every helper (`git_repo`, `run`, `stop`, `port_of`, `pid_of`, `secret_of`, `parent_of`, `server_json_path`, `spawn`, `reqwest_free_get`) is defined in `tests/support/mod.rs` and listed in Appendix A. **None of them sets an environment variable**: each spawned command gets `XDG_STATE_HOME` through `Command::env`, and the test-side path is computed with `state_dir_in(repo.join("state"), repo_root)`. The earlier draft called `std::env::set_var` from tests that Cargo runs concurrently on threads of one process, which is both order-dependent and a documented data race.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test server_lifecycle`
Expected: FAIL, `unrecognized subcommand 'serve'`.

- [ ] **Step 3: Implement the daemon**

```rust
//! Detaching from the terminal.
//!
//! Ordering is the whole content of this module. The listener is a **raw
//! `std::net::TcpListener`**, bound before the fork: a file descriptor
//! survives fork, but a `tiny_http::Server` does not, because constructing one
//! spawns an accept thread and `fork` keeps only the calling thread. The
//! server is built in the grandchild, from the inherited descriptor.
//!
//! The parent does not exit until the grandchild signals readiness on a pipe,
//! so a command run straight after `serve` always finds a live server.

use anyhow::{bail, Context, Result};
use std::os::unix::io::AsRawFd;
use std::path::Path;

extern "C" {
    fn fork() -> i32;
    fn setsid() -> i32;
}

pub struct Readiness {
    write_fd: i32,
}

impl Readiness {
    /// Tell the waiting parent the server is up. Called after `server.json`
    /// is written and the listener is accepting.
    pub fn ready(self) {
        let _ = write_all(self.write_fd, b"K");
        close(self.write_fd);
    }

    /// Tell the parent why it failed, then leave. The parent prints this, so
    /// the user sees the real reason rather than a silent exit.
    pub fn fail(self, msg: &str) -> ! {
        let _ = write_all(self.write_fd, format!("E{msg}").as_bytes());
        close(self.write_fd);
        std::process::exit(1);
    }
}

pub fn daemonize(log_path: &Path) -> Result<Readiness> {
    // Open the log and /dev/null before forking: after a fork there is exactly
    // one thread, and doing allocation-heavy work there is a hazard best
    // avoided entirely.
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .with_context(|| format!("opening {}", log_path.display()))?;
    let devnull = std::fs::File::open("/dev/null")?;
    let (read_fd, write_fd) = pipe()?;

    // SAFETY: the standard double-fork sequence. Every syscall is checked, and
    // -1 is an error rather than the parent branch.
    unsafe {
        match fork() {
            -1 => bail!("fork failed: {}", std::io::Error::last_os_error()),
            0 => {}
            _ => {
                // Parent: wait for the grandchild, then report.
                close(write_fd);
                return parent_wait(read_fd);
            }
        }
        if setsid() < 0 {
            bail!("setsid failed: {}", std::io::Error::last_os_error());
        }
        // The second fork gives up session leadership, so the daemon can never
        // acquire a controlling terminal.
        match fork() {
            -1 => bail!("second fork failed: {}", std::io::Error::last_os_error()),
            0 => {}
            _ => std::process::exit(0),
        }
        libc::dup2(log.as_raw_fd(), libc::STDOUT_FILENO);
        libc::dup2(log.as_raw_fd(), libc::STDERR_FILENO);
        libc::dup2(devnull.as_raw_fd(), libc::STDIN_FILENO);
        close_inherited_except(&[libc::STDIN_FILENO, libc::STDOUT_FILENO, libc::STDERR_FILENO, write_fd]);
    }
    Ok(Readiness { write_fd })
}
```

`parent_wait` reads the pipe. `K` means the grandchild is serving, and the parent returns success without ever reaching the run loop. `E<message>` means it failed, and the parent prints that message and exits non-zero. End of file with nothing read means the grandchild died, which is also an error. `close_inherited_except` closes descriptors above the ones listed, which spec 4.2 asks for.

- [ ] **Step 4: Implement `serve` in the order that works**

```rust
pub fn serve(args: &ServeArgs) -> Result<()> {
    let root = state_dir::repo_root(&std::env::current_dir()?)?;
    let dir = state_dir::state_dir(&root);
    std::fs::create_dir_all(&dir)?;

    // One winner. Two concurrent serves would otherwise both find no server.
    let Some(_lock) = state_dir::StartupLock::acquire(&dir) else {
        // Someone else is starting one; wait briefly for their server.json.
        return wait_for_peer(&dir, args);
    };
    if let Some(existing) = state_dir::read_server_file(&dir) {
        if !args.no_open {
            open_browser_for(&existing, None)?;
        }
        return Ok(());
    }

    // Reuse the recorded port and secret even after a clean shutdown, so an
    // open page reconnects and its cookie stays valid.
    let previous = state_dir::read_server_file_any(&dir);
    let secret = previous.as_ref().map(|p| p.secret.clone()).unwrap_or_else(state_dir::new_secret);

    // A RAW listener, before the fork. Binding here means a refusal reaches
    // the caller's stderr with a message they can act on.
    let listener = bind_preferring(args.port.or(previous.as_ref().map(|p| p.port)))
        .context("could not bind a loopback port; if this environment forbids listening \
                  sockets, run `artefacto serve --foreground` in your own terminal")?;
    let port = listener.local_addr()?.port();

    let readiness = if args.foreground { None } else { Some(daemon::daemonize(&dir.join("server.log"))?) };

    // Everything below runs in the grandchild.
    let write = || -> Result<Arc<Shared>> {
        state_dir::write_server_file(&dir, &ServerFile {
            pid: std::process::id(),
            port,
            secret: secret.clone(),
            started_at: log::now_rfc3339(),
        })?;
        Ok(Arc::new(Shared::new(&dir, secret.clone(), port)?))
    };
    let shared = match write() {
        Ok(s) => s,
        Err(e) => match readiness {
            Some(r) => r.fail(&format!("{e:#}")),
            None => return Err(e),
        },
    };

    // NOW build the tiny_http server, in the grandchild, from the inherited
    // descriptor. Constructing it spawns the accept thread, which is why this
    // cannot happen before the fork.
    let server = Arc::new(tiny_http::Server::from_listener(listener, None)
        .map_err(|e| anyhow::anyhow!("{e}"))?);

    if let Some(r) = readiness {
        r.ready();
    }
    http::run(shared, server, SELF_EXIT);
    Ok(())
    // server.json is deliberately left in place: it carries the port and
    // secret the next start reuses, and its dead pid already reads as
    // "no server".
}
```

- [ ] **Step 5: Implement `stop`, `status`, and `open`**

- `stop` reads `server.json`, calls `POST /cli/stop` with the bearer secret, and waits for the port to close. **It does not fall back to a blind `SIGTERM`**: a pid can be reused, and signalling an unrelated process is worse than failing. If the route does not answer, it reports that the server is unreachable and names the pid so the user can decide.
- `status --json` prints, per spec 5: port, artifacts, revisions, open and unanchored thread counts, last event seq, each lease's `acked_seq`, the lease holder and its age, reviewer presence, the exact `events --follow` command line, and `state_dir`. In this plan the review fields are empty or zero, because plan 2b fills them; the **shape** is fixed here and asserted, so plan 2b only populates it. It never prints the secret or a session token.
- `open --artifact ID` mints a bootstrap token, prints the URL, and opens the browser unless `--no-open`. The no-argument form opens the artifact index, which is plan 4; here it is a clear error naming `--artifact`.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test --test server_lifecycle`
Expected: PASS, 10 tests.

- [ ] **Step 7: Verify by hand, and report what you saw**

```bash
cargo run --quiet -- serve --no-open
STATE="$(cargo run --quiet -- status --json | python3 -c 'import json,sys; print(json.load(sys.stdin)["state_dir"])')"
PID="$(python3 -c "import json; print(json.load(open('$STATE/server.json'))['pid'])")"
ps -o pid,ppid,stat -p "$PID"
cargo run --quiet -- open --artifact plan:x
```

Expected: `PPID` is `1`. The browser opens the placeholder page. In devtools, the console shows **no CSP violation**, and the Network tab shows the `/ws` request completing as `101 Switching Protocols`. Then `cargo run --quiet -- stop` and confirm `status --json` exits 4.

Report what you saw. Do not claim the socket connects without looking at it — the earlier draft's meta-CSP bug is invisible except in that console.

- [ ] **Step 8: Verify the gate, then commit**

```bash
cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --all
git add src/server/daemon.rs src/commands/serve.rs src/client.rs src/cli.rs src/commands/mod.rs src/lib.rs tests/
git commit -m "feat(server): serve, stop, status, open, and a daemon that actually accepts"
```

---

## What this plan deliberately leaves out

- **Everything about the review itself.** The fold, page ingress, threads, the lease, delivery cursors, and the agent verbs are plan 2b (`docs/plans/2026-09-09-artefacto-event-model.md`). This plan serves a placeholder document, because nothing can be pushed yet.
- **The page rewrite.** Plan 3.
- **The artifact index, `list`, posters.** Plan 4. `open` here requires `--artifact`.
- **`skill --print`, `--install`, cargo-dist.** Plan 5.
- **The loadout dispatcher.** Plan 6. Nothing here edits the rosita repository.
- **`clean`.** Plan 4.
- **The file-based fallback transport** from spec 13. Three spikes cleared the loopback path on the primary target, so it stays a documented contingency.

**What this leaves uncovered, stated plainly:** there is no browser-level test. The socket is driven by a fake page client, which proves the server's half and nothing about a real browser's. The meta-CSP bug that two reviewers caught in the earlier draft is exactly the class of defect a fake client cannot see, which is why Task 7's hand-verification step requires looking at the devtools console. The headless-Chromium smoke arrives with the page in plan 3.

## Verification for the whole plan

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --all
```

Then, by hand, in this order — `serve` first, because nothing else starts a server in this plan:

```bash
cargo run --quiet -- serve --no-open
cargo run --quiet -- status --json
cargo run --quiet -- open --artifact plan:x
# look at the browser: no CSP violation, /ws is 101
cargo run --quiet -- stop
cargo run --quiet -- status --json   # exits 4
```

## Appendix A: the test harness

`tests/support/mod.rs`. Each helper is added by the task that first uses it. None touches the developer's real state directory, and **none sets an environment variable**: spawned commands receive `XDG_STATE_HOME` through `Command::env`, and test-side paths come from `state_dir_in`.

| helper | signature | contract | task |
|---|---|---|---|
| `Harness::start` | `fn start() -> Harness` | Server on a random port over a temp state dir, self-exit effectively disabled. | 4 |
| `Harness::start_with_idle` | `fn start_with_idle(idle: Duration) -> Harness` | Same, with a real self-exit window. | 4 |
| `restart` | `fn restart(self) -> Harness` | New server over the **same** state directory and secret. | 4 |
| `raw` | `fn raw(&self, request: &str) -> String` | Bytes verbatim, for hostile headers. | 4 |
| `get` | `fn get(&self, path: &str, headers: &[(&str, &str)]) -> String` | Well-formed GET with the correct `Host`. | 4 |
| `Harness::status_of` | `fn status_of(response: &str) -> u16` | Parses the status line. | 4 |
| `wait_for` | `fn wait_for(&self, cond: impl FnMut() -> bool, what: &str)` | Polls up to five seconds. Never sleep a fixed time. | 4 |
| `is_serving` | `fn is_serving(&self) -> bool` | Whether a TCP connect to the port succeeds. | 4 |
| `mint_bootstrap` | `fn mint_bootstrap(&self, artifact: &str) -> String` | Mints through the server's own state. Panics on an invalid id. | 5 |
| `shared_mint` | `fn shared_mint(&self, artifact: &str) -> anyhow::Result<String>` | The fallible form, for the id-validation test. | 5 |
| `mint_bootstrap_aged` | `fn mint_bootstrap_aged(&self, artifact: &str, age: Duration) -> String` | Backdates so expiry is testable without waiting. | 5 |
| `session_cookie` | `fn session_cookie(&self, artifact: &str) -> String` | Walks the real redirect and returns the `Set-Cookie` value. | 5 |
| `origin` | `fn origin(&self) -> String` | `http://127.0.0.1:<port>`. | 6 |
| `connect_page` | `fn connect_page(&self) -> FakePage` | Cookie plus matching Origin. Panics if refused. | 6 |
| `connect_page_raw` | `fn connect_page_raw(&self, cookie: Option<&str>, origin: Option<&str>) -> Result<FakePage, String>` | Either header omitted or wrong. | 6 |
| `page_count` | `fn page_count(&self) -> usize` | Connected sockets. | 6 |
| `broadcast_test_frame` | `fn broadcast_test_frame(&self, seq: u64)` | Broadcasts a frame carrying one real event — never an empty one, which spec 6.1 forbids. | 6 |
| `FakePage::next_frame` | `fn next_frame(&mut self) -> serde_json::Value` | Blocks for the next text frame, parsed. | 6 |
| `FakePage::send` | `fn send(&mut self, value: serde_json::Value)` | Sends one JSON message as the page. | 6 |
| `git_repo` | `fn git_repo() -> tempfile::TempDir` | A `git init`-ed temp dir; the state dir is keyed by the git root. | 7 |
| `run` | `fn run(repo: &TempDir, args: &[&str]) -> Out` | Runs the built binary with `XDG_STATE_HOME` set through `env`. `Out` exposes `success()`, `code(i32)`, `stdout_string()`. | 7 |
| `spawn` | `fn spawn(repo: &TempDir, args: &[&str]) -> Child` | The same, without waiting, for the concurrent-serve test. | 7 |
| `stop` | `fn stop(repo: &TempDir)` | `artefacto stop`, tolerating an already-stopped server. | 7 |
| `server_json_path` | `fn server_json_path(repo: &TempDir) -> PathBuf` | Computed with `state_dir_in`; reads no environment. | 7 |
| `port_of` / `pid_of` / `secret_of` | `fn …(repo: &TempDir) -> …` | Fields out of `server.json`. | 7 |
| `parent_of` | `fn parent_of(pid: u32) -> u32` | `ps -o ppid= -p <pid>`, for the detachment test. | 7 |
| `reqwest_free_get` | `fn reqwest_free_get(port: u16, path: &str) -> String` | A one-shot GET over `TcpStream`, so the tests add no HTTP client dependency. | 7 |

**One rule for all of them:** a helper may observe state or stand in for the reviewer, but never skip a code path the production caller would take. `session_cookie` walks the real redirect rather than reading the cookie out of `Shared`. A harness that shortcuts the code under test proves nothing.
