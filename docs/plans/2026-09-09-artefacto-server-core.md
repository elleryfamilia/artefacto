# artefacto Server Core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stand up the artefacto server: a per-repository loopback daemon that owns an append-only event log, hands frames to one leased agent at a time, authenticates a browser page over a cookie and a WebSocket, and serves the agent verbs `push`, `events`, `await`, `ack`, `reply`, and `resolve`. Every piece is tested against a fake page client; the real page is plan 3.

**Architecture:** Blocking and thread-per-connection, with no async runtime. The main thread accepts requests with `tiny_http`'s `recv_timeout` and spawns a thread per request, so a 90-second long poll occupies only its own thread and never starves the accept loop. One append-only NDJSON log per server is the single source of truth; every piece of live state is a fold over it, rebuilt on start. The server is the only writer. CLI commands never touch the log; they call the server over loopback HTTP with a bearer secret.

**Tech Stack:** Rust 2021, edition floor 1.85. Existing: `clap`, `serde`, `serde_json`, `maud`, `pulldown-cmark`, `sha2`, `anyhow`. New: `tiny_http` 0.12 (blocking HTTP), `tungstenite` 0.30 (synchronous WebSocket), `libc` (fork, setsid, kill liveness), `getrandom` (secret and token bytes). Tests use `assert_cmd`, `predicates`, and `tempfile`, all already present.

**Spec:** `docs/specs/2026-09-06-artefacto-design.md`. Sections 4.2, 5, 6, 8, and 9 are this plan's contract. Read them before Task 1; the plan argues from the spec and does not restate all of it.

**Prior plan:** `docs/plans/2026-09-08-artefacto-extraction.md` shipped the binary, the plan model, the renderer, and the static commands. All of it is green and must stay green.

## Why these dependencies

The stack was chosen by measurement, not preference, and the numbers are recorded here so a reviewer can challenge them.

- A prototype confirmed `tiny_http` + `tungstenite` handle one HTTP route, a blocking long poll, a concurrent request served while the poll was blocked, and a real WebSocket upgrade with `derive_accept_key` and `WebSocket::from_raw_socket`. About 15 lines of glue.
- The two crates add **18** new transitive crates. artefacto pulls 67 today. The async alternative (`tokio` + `axum`) roughly triples the tree.
- Daemonizing forks. A process that has already started an async runtime cannot fork safely, because the runtime's threads do not survive it. Blocking code has no such constraint, and `serve` daemonizes before it binds anything.

Three sandbox risks from spec section 13 were probed before this plan was written, and all three came back clear on the primary target (macOS, Claude Code's sandbox): the loopback bind succeeds; a double-forked, `setsid`-ed daemon is reparented to pid 1 and survives the tool call that started it; and a separate process reaches that port. The file-based fallback transport in section 13 is therefore **not** built in this plan. It stays a documented contingency.

## Global Constraints

Every task's requirements implicitly include this section.

- Rust edition 2021, `rust-version = "1.85"`, `[toolchain] channel = "stable"` with `rustfmt` and `clippy`.
- Every task ends green on `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --all`.
- Commit at the end of every task using Conventional Commits.
- **Nothing artefacto writes carries loadout's name.** Events declare `artefacto.event/1`, frames `artefacto.frame/1`, feedback `artefacto.feedback/1`. See spec section 11.1. `tests/skill_examples.rs` already enforces this and must keep passing.
- **Loopback only.** Bind `127.0.0.1`. The `Host` header must be exactly `127.0.0.1:<port>` on every route. `localhost` is not served. This is not theoretical: on the development machine `http://localhost:<port>/` resolves and connects, so the exact-match check is the only thing rejecting it. It gets its own test.
- **No CORS headers, ever.**
- The session secret is 256 random bits, stored in a mode `0600` file, persisted across restarts, rotated only by `clean`.
- **The server is the only writer of the event log.** CLI commands call the server. If a CLI command ever opens the log for writing, the design is broken.
- All live state folds from the log: revisions, threads, answers, reviewed marks, delivery cursors, and the lease. `server.json` holds only pid, port, secret, and started-at, because those must exist before the log can be read.
- `seq` is server-wide, monotonic, and continues from the last logged value after a restart. `clean` truncates; it never renumbers.
- Delivery is **at-least-once** against a persisted per-lease cursor. Every handler must be safe to run twice.
- `await` exits 0 for every non-error outcome and prints one JSON object with `status`, `seq`, `session`, and `events`. Non-zero only for real errors: **4** no server, **6** lease held or superseded token, **7** stale `base_revision`.
- Default `await` timeout is **90 seconds**.
- Lease TTL is **5 minutes**. Server self-exit is **30 minutes** with no page and no agent.
- macOS and Linux only. No Windows.
- The real browser page is plan 3. This plan serves the existing statically rendered HTML and tests every socket path against a fake page client.

## File Structure

| file | responsibility |
|---|---|
| `src/server/mod.rs` | module root; `Server` struct, the accept loop, graceful shutdown |
| `src/server/state_dir.rs` | state directory resolution, `server.json`, secret, pid liveness |
| `src/server/event.rs` | `artefacto.event/1` and `artefacto.frame/1` types |
| `src/server/log.rs` | append-only log: append with fsync, replay, `seq` assignment |
| `src/server/fold.rs` | live state folded from the log |
| `src/server/lease.rs` | lease, session tokens, generations, TTL, takeover |
| `src/server/http.rs` | routing, `Host` guard, bearer guard, error shape |
| `src/server/page.rs` | bootstrap tokens, cookie, Origin check, CSP nonce, page HTML |
| `src/server/socket.rs` | WebSocket upgrade and page frame delivery |
| `src/server/delivery.rs` | cursors, digest and live passive rules, frame assembly |
| `src/server/daemon.rs` | double fork, `setsid`, descriptor redirect, self-exit timer |
| `src/client.rs` | the CLI's HTTP client to the server (bearer auth) |
| `src/commands/serve.rs` | `serve`, `stop`, `status`, `open` |
| `src/commands/agent.rs` | `events`, `await`, `ack`, `reply`, `resolve` |
| `src/commands/plan.rs` | existing, plus `push` |
| `tests/support/mod.rs` | test harness: spawn a server on a temp state dir, fake page client |
| `tests/server_security.rs` | Host, bootstrap, cookie, Origin, bearer, CSP |
| `tests/server_log.rs` | append, replay, `seq` continuity, log sufficiency |
| `tests/server_lease.rs` | lease, TTL, dead pid, takeover, superseded token |
| `tests/server_delivery.rs` | cursors, at-least-once, digest and live rules |
| `tests/server_agent.rs` | `events`, `await`, `push`, `reply`, `resolve`, `ack` |

---

### Task 1: The state directory, `server.json`, and pid liveness

**Why this task exists:** every other task needs to know where the server keeps its files and whether a recorded server is actually alive. Spec section 9 fixes the layout, and section 4.2 says every CLI command checks the pid before trusting `server.json`. Getting this wrong means a dead server looks alive forever, and every later command hangs against a port nobody is listening on.

**Files:**
- Create: `src/server/mod.rs`, `src/server/state_dir.rs`, `tests/server_log.rs` (the file is created here, its first test lands in Task 3)
- Modify: `src/lib.rs`, `Cargo.toml`

**Interfaces:**
- Produces:
  - `pub fn state_dir(repo_root: &Path) -> PathBuf`
  - `pub fn repo_root(cwd: &Path) -> anyhow::Result<PathBuf>`
  - `pub struct ServerFile { pub pid: u32, pub port: u16, pub secret: String, pub started_at: String }`
  - `pub fn read_server_file(dir: &Path) -> Option<ServerFile>` — `None` when absent, unreadable, or the pid is dead
  - `pub fn write_server_file(dir: &Path, f: &ServerFile) -> anyhow::Result<()>` — creates with mode `0600`
  - `pub fn is_alive(pid: u32) -> bool`
  - `pub fn new_secret() -> String` — 64 lowercase hex characters, 256 bits

- [ ] **Step 1: Add the dependencies**

```bash
cargo add tiny_http@0.12 tungstenite@0.30 libc getrandom
```

Expected: `Cargo.toml` gains four entries and `Cargo.lock` updates. Do not add `tokio`, `axum`, `hyper`, or `futures`. If any appears in `cargo tree`, something pulled an async runtime in and the choice recorded above has been silently reversed.

- [ ] **Step 2: Write the failing test**

Create `src/server/state_dir.rs` with only its test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn state_dir_is_keyed_by_repo_path() {
        let a = state_dir(Path::new("/tmp/one"));
        let b = state_dir(Path::new("/tmp/two"));
        assert_ne!(a, b, "different repos must not share a state directory");
        assert_eq!(a, state_dir(Path::new("/tmp/one")), "must be stable");
        assert!(a.ends_with(state_dir(Path::new("/tmp/one")).file_name().unwrap()));
    }

    #[test]
    fn secret_is_256_bits_of_hex() {
        let s = new_secret();
        assert_eq!(s.len(), 64, "256 bits is 64 hex characters");
        assert!(
            s.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        assert_ne!(s, new_secret(), "two calls must not collide");
    }

    #[test]
    fn server_file_round_trips_at_0600() {
        let dir = tempfile::tempdir().unwrap();
        let f = ServerFile {
            pid: std::process::id(),
            port: 4321,
            secret: new_secret(),
            started_at: "2026-09-09T00:00:00Z".to_string(),
        };
        write_server_file(dir.path(), &f).unwrap();

        let mode = std::fs::metadata(dir.path().join("server.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "the secret file must not be group or world readable"
        );

        let back = read_server_file(dir.path()).expect("our own pid is alive");
        assert_eq!(back.port, 4321);
        assert_eq!(back.secret, f.secret);
    }

    #[test]
    fn a_dead_pid_reads_as_no_server() {
        let dir = tempfile::tempdir().unwrap();
        let f = ServerFile {
            pid: 0,
            port: 4321,
            secret: new_secret(),
            started_at: "2026-09-09T00:00:00Z".to_string(),
        };
        write_server_file(dir.path(), &f).unwrap();
        assert!(
            read_server_file(dir.path()).is_none(),
            "a dead pid means no server"
        );
    }

    #[test]
    fn a_corrupt_file_reads_as_no_server() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("server.json"), b"{not json").unwrap();
        assert!(read_server_file(dir.path()).is_none());
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test --lib server::state_dir`
Expected: FAIL to compile, `cannot find function 'state_dir' in this scope`.

- [ ] **Step 4: Implement the module**

Write above the test module in `src/server/state_dir.rs`:

```rust
//! Where the server keeps its files, and whether a recorded server is real.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

/// What must exist before the event log can be read. Everything else about a
/// running server is folded from the log, not stored here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerFile {
    pub pid: u32,
    pub port: u16,
    pub secret: String,
    pub started_at: String,
}

/// The git root of the directory the command was invoked from. One server per
/// repository is anchored to this path.
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

/// `~/.local/state/artefacto/<repo-hash>/`. The hash keys the directory by the
/// repository path so two checkouts of the same project never collide.
pub fn state_dir(repo_root: &Path) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(repo_root.to_string_lossy().as_bytes());
    let hash = format!("{:x}", hasher.finalize());
    base_state_dir().join("artefacto").join(&hash[..16])
}

fn base_state_dir() -> PathBuf {
    // Not `if let ... { if ... }`: clippy rejects the nesting, and let-chains
    // need edition 2024, which this crate does not use.
    let xdg = std::env::var("XDG_STATE_HOME").unwrap_or_default();
    if !xdg.is_empty() {
        return PathBuf::from(xdg);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".local").join("state")
}

/// 256 bits from the OS, as lowercase hex.
pub fn new_secret() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("the OS must provide randomness");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Signal 0 asks "could I signal this process" without sending anything.
pub fn is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // SAFETY: `kill` with signal 0 performs the permission and existence
    // check without delivering a signal. No memory is touched.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// `None` for absent, unreadable, corrupt, or dead. Every caller treats all
/// four the same way: there is no server.
pub fn read_server_file(dir: &Path) -> Option<ServerFile> {
    let raw = fs::read_to_string(dir.join("server.json")).ok()?;
    let f: ServerFile = serde_json::from_str(&raw).ok()?;
    if is_alive(f.pid) { Some(f) } else { None }
}

/// Created at mode 0600 because it carries the session secret. The mode is set
/// at open time, not afterwards, so the file is never briefly world readable.
pub fn write_server_file(dir: &Path, f: &ServerFile) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = dir.join("server.json");
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true).mode(0o600);
    let file = opts
        .open(&path)
        .with_context(|| format!("creating {}", path.display()))?;
    serde_json::to_writer_pretty(&file, f)?;
    Ok(())
}
```

- [ ] **Step 5: Declare the module**

`src/server/mod.rs`:

```rust
//! The artefacto server: one loopback daemon per repository.

pub mod state_dir;
```

Add to `src/lib.rs`, keeping the existing modules:

```rust
pub mod server;
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test --lib server::state_dir`
Expected: PASS, 5 tests.

- [ ] **Step 7: Verify the gate**

Run: `cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --all`
Expected: all clean. Clippy will object to `unsafe` without a safety comment if the lint is on; the `libc::kill` call already carries one.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml Cargo.lock src/lib.rs src/server/
git commit -m "feat(server): resolve the state directory and read server.json safely"
```

---

### Task 2: Event and frame types

**Why this task exists:** the envelope in spec section 6.1 is a cross-process contract. The agent parses it, the page parses it, and a future loadout release parses it. Freezing the shape in types with a round-trip test now means the later tasks cannot quietly drift the field names.

**Files:**
- Create: `src/server/event.rs`
- Modify: `src/server/mod.rs`

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces:
  - `pub struct Event { pub format: String, pub seq: u64, pub ts: String, pub artifact: String, pub revision: u32, pub actor: Actor, pub r#type: String, pub data: serde_json::Value }`
  - `pub enum Actor { Reviewer, Agent, Server }` serializing to `"reviewer" | "agent" | "server"`
  - `pub struct Frame { pub format: String, pub seq: u64, pub events: Vec<Event> }`
  - `pub const EVENT_FORMAT: &str = "artefacto.event/1";`
  - `pub const FRAME_FORMAT: &str = "artefacto.frame/1";`
  - `pub fn is_active(event_type: &str) -> bool`

- [ ] **Step 1: Write the failing test**

Create `src/server/event.rs` with only its test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_event_serializes_to_the_spec_envelope() {
        let e = Event {
            format: EVENT_FORMAT.to_string(),
            seq: 42,
            ts: "2026-09-06T16:02:11Z".to_string(),
            artifact: "plan:auth-refactor".to_string(),
            revision: 3,
            actor: Actor::Reviewer,
            r#type: "thread.replied".to_string(),
            data: serde_json::json!({ "thread": "c-3", "ref": "task:t-session-store" }),
        };
        let v: serde_json::Value = serde_json::to_value(&e).unwrap();
        assert_eq!(v["format"], "artefacto.event/1");
        assert_eq!(v["seq"], 42);
        assert_eq!(v["actor"], "reviewer");
        assert_eq!(
            v["type"], "thread.replied",
            "the field is `type`, not `r#type`"
        );
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
        let mk = |seq| Event {
            format: EVENT_FORMAT.to_string(),
            seq,
            ts: "t".to_string(),
            artifact: "plan:x".to_string(),
            revision: 1,
            actor: Actor::Reviewer,
            r#type: "thread.opened".to_string(),
            data: serde_json::Value::Null,
        };
        let f = Frame::of(vec![mk(4), mk(5), mk(9)]);
        assert_eq!(f.seq, 9, "a frame's seq is its last event's seq");
        assert_eq!(f.format, "artefacto.frame/1");
    }

    #[test]
    fn active_and_passive_are_split_exactly_as_the_spec_says() {
        for t in [
            "chat.sent",
            "review.submitted",
            "reviewer.idle",
            "reviewer.away",
            "server.stopping",
        ] {
            assert!(is_active(t), "{t} is active");
        }
        for t in [
            "thread.opened",
            "thread.replied",
            "thread.edited",
            "thread.deleted",
            "question.answered",
            "element.reviewed",
            "reviewer.back",
        ] {
            assert!(!is_active(t), "{t} is passive");
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

/// Active events wake the agent. Passive events ride along with the next
/// active one in digest mode. Spec sections 6.2 and 6.4.
pub fn is_active(event_type: &str) -> bool {
    matches!(
        event_type,
        "chat.sent" | "review.submitted" | "reviewer.idle" | "reviewer.away" | "server.stopping"
    )
}
```

- [ ] **Step 4: Declare the module**

Add to `src/server/mod.rs`:

```rust
pub mod event;
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --lib server::event`
Expected: PASS, 4 tests.

- [ ] **Step 6: Verify the gate**

Run: `cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --all`
Expected: all clean.

- [ ] **Step 7: Commit**

```bash
git add src/server/event.rs src/server/mod.rs
git commit -m "feat(server): freeze the event and frame envelope"
```

---

### Task 3: The append-only event log

**Why this task exists:** spec section 4.2 makes the log the only source of truth, and section 6.7 requires `seq` to continue across a restart. If `seq` restarted at 1, an agent's `--since` cursor would silently replay the whole history or skip it entirely. The log is also the thing a crash can corrupt, so a half-written last line must not take the server down.

**Files:**
- Create: `src/server/log.rs`
- Modify: `src/server/mod.rs`, `tests/server_log.rs`

**Interfaces:**
- Consumes: `event::{Event, Actor, EVENT_FORMAT}` from Task 2.
- Produces:
  - `pub struct EventLog { path: PathBuf, next_seq: u64 }`
  - `pub fn open(dir: &Path) -> anyhow::Result<EventLog>` — replays to find the highest `seq`
  - `pub fn append(&mut self, artifact: &str, revision: u32, actor: Actor, kind: &str, data: serde_json::Value) -> anyhow::Result<Event>`
  - `pub fn read_since(&self, since: u64) -> anyhow::Result<Vec<Event>>` — events with `seq > since`
  - `pub fn last_seq(&self) -> u64`

- [ ] **Step 1: Write the failing test**

Create `tests/server_log.rs`:

```rust
use artefacto::server::event::Actor;
use artefacto::server::log::EventLog;

fn json() -> serde_json::Value {
    serde_json::json!({ "ref": "task:t-a" })
}

#[test]
fn append_assigns_monotonic_sequence_numbers() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = EventLog::open(dir.path()).unwrap();
    let a = log
        .append("plan:x", 1, Actor::Reviewer, "thread.opened", json())
        .unwrap();
    let b = log
        .append("plan:x", 1, Actor::Reviewer, "thread.opened", json())
        .unwrap();
    assert_eq!(a.seq, 1);
    assert_eq!(b.seq, 2);
    assert_eq!(log.last_seq(), 2);
}

#[test]
fn seq_continues_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut log = EventLog::open(dir.path()).unwrap();
        log.append("plan:x", 1, Actor::Reviewer, "thread.opened", json())
            .unwrap();
        log.append("plan:x", 1, Actor::Reviewer, "thread.opened", json())
            .unwrap();
    }
    let mut reopened = EventLog::open(dir.path()).unwrap();
    assert_eq!(reopened.last_seq(), 2, "a restart must not renumber");
    let next = reopened
        .append("plan:x", 1, Actor::Reviewer, "thread.opened", json())
        .unwrap();
    assert_eq!(next.seq, 3, "the cursor an agent holds stays meaningful");
}

#[test]
fn read_since_is_exclusive_of_the_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = EventLog::open(dir.path()).unwrap();
    for _ in 0..5 {
        log.append("plan:x", 1, Actor::Reviewer, "thread.opened", json())
            .unwrap();
    }
    let got = log.read_since(2).unwrap();
    assert_eq!(got.len(), 3, "seq 3, 4, 5");
    assert_eq!(got[0].seq, 3, "the cursor names what was already delivered");
    assert_eq!(got[4 - 2].seq, 5);
    assert!(
        log.read_since(5).unwrap().is_empty(),
        "caught up means nothing"
    );
}

#[test]
fn a_truncated_last_line_does_not_take_the_log_down() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut log = EventLog::open(dir.path()).unwrap();
        log.append("plan:x", 1, Actor::Reviewer, "thread.opened", json())
            .unwrap();
        log.append("plan:x", 1, Actor::Reviewer, "thread.opened", json())
            .unwrap();
    }
    let path = dir.path().join("events.ndjson");
    let mut raw = std::fs::read_to_string(&path).unwrap();
    raw.push_str("{\"format\":\"artefacto.event/1\",\"seq\":3,\"ts\"");
    std::fs::write(&path, raw).unwrap();

    let mut log = EventLog::open(dir.path()).unwrap();
    assert_eq!(log.last_seq(), 2, "the torn line is not a committed event");
    let next = log
        .append("plan:x", 1, Actor::Reviewer, "thread.opened", json())
        .unwrap();
    assert_eq!(
        next.seq, 3,
        "seq 3 was never committed, so it is still free"
    );
    assert_eq!(
        log.read_since(0).unwrap().len(),
        3,
        "and the log still reads clean"
    );
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --test server_log`
Expected: FAIL to compile, `could not find 'log' in 'server'`.

- [ ] **Step 3: Implement the module**

Create `src/server/log.rs`:

```rust
//! The append-only event log. One per server, server-wide `seq`.
//!
//! The server is the only writer. Every piece of live state is a fold over
//! this file, so a torn final line must be dropped rather than trusted: a
//! half-written event was never acknowledged to anyone.

use crate::server::event::{Actor, EVENT_FORMAT, Event};
use anyhow::{Context, Result};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

pub struct EventLog {
    path: PathBuf,
    next_seq: u64,
}

impl EventLog {
    /// Replays the file to find the highest committed `seq`. A line that does
    /// not parse is dropped, and everything after it is dropped too: the file
    /// is append-only, so the first bad line marks where the last crash was.
    pub fn open(dir: &Path) -> Result<EventLog> {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let path = dir.join("events.ndjson");
        let mut last = 0u64;
        let mut good_bytes = 0u64;
        if path.exists() {
            let file = File::open(&path)?;
            for line in BufReader::new(file).lines() {
                let line = match line {
                    Ok(l) => l,
                    Err(_) => break,
                };
                match serde_json::from_str::<Event>(&line) {
                    Ok(e) => {
                        last = e.seq;
                        good_bytes += line.len() as u64 + 1;
                    }
                    Err(_) => break,
                }
            }
            // Drop the torn tail so the next append starts on a clean line.
            let on_disk = std::fs::metadata(&path)?.len();
            if good_bytes < on_disk {
                let f = OpenOptions::new().write(true).open(&path)?;
                f.set_len(good_bytes)?;
                f.sync_all()?;
            }
        }
        Ok(EventLog {
            path,
            next_seq: last + 1,
        })
    }

    pub fn last_seq(&self) -> u64 {
        self.next_seq - 1
    }

    /// Appends one event and returns it with its assigned `seq`. The write is
    /// flushed and synced before returning, because the caller is about to
    /// tell a client the event happened.
    pub fn append(
        &mut self,
        artifact: &str,
        revision: u32,
        actor: Actor,
        kind: &str,
        data: serde_json::Value,
    ) -> Result<Event> {
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
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(f, "{line}")?;
        f.sync_all()?;
        self.next_seq += 1;
        Ok(event)
    }

    /// Every event with `seq` strictly greater than `since`. The cursor names
    /// what was already delivered, so it is never included.
    pub fn read_since(&self, since: u64) -> Result<Vec<Event>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let file = File::open(&self.path)?;
        let mut out = Vec::new();
        for line in BufReader::new(file).lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => break,
            };
            match serde_json::from_str::<Event>(&line) {
                Ok(e) if e.seq > since => out.push(e),
                Ok(_) => {}
                Err(_) => break,
            }
        }
        Ok(out)
    }
}

/// RFC 3339 in UTC, to the second. No dependency on a date crate: the only
/// consumer is a human reading the log and a client echoing the string back.
fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = secs / 86_400;
    let tod = secs % 86_400;
    let (y, m, d) = civil_from_days(days as i64);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

/// Howard Hinnant's days-to-civil algorithm, public domain. Correct for every
/// date this program will ever see.
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
```

- [ ] **Step 4: Declare the module**

Add to `src/server/mod.rs`:

```rust
pub mod log;
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --test server_log`
Expected: PASS, 4 tests.

- [ ] **Step 6: Add a unit test for the date helper**

The date maths is the one place here that can be silently wrong, and the log's timestamps are the only human-readable record of when something happened. Append to `src/server/log.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::civil_from_days;

    #[test]
    fn civil_from_days_matches_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1), "the epoch");
        assert_eq!(civil_from_days(19_000), (2022, 1, 8));
        assert_eq!(civil_from_days(20_000), (2024, 10, 4));
        // A leap day, which is where naive implementations go wrong.
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
    }
}
```

Run: `cargo test --lib server::log`
Expected: PASS, 1 test.

- [ ] **Step 7: Verify the gate**

Run: `cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --all`
Expected: all clean.

- [ ] **Step 8: Commit**

```bash
git add src/server/log.rs src/server/mod.rs tests/server_log.rs
git commit -m "feat(server): append-only event log with a restart-safe sequence"
```

---

### Task 4: The HTTP skeleton, the guards, and the test harness

**Why this task exists:** every later task is a route on this server, and every route is only as safe as the two guards in front of it. Spec section 8 requires an exact `Host` match on every route and a bearer secret on CLI routes. The accept loop's shape also decides whether a 90-second long poll can starve everything else, so it is fixed here rather than discovered in Task 10.

**Files:**
- Create: `src/server/http.rs`, `tests/support/mod.rs`, `tests/server_security.rs`
- Modify: `src/server/mod.rs`

**Interfaces:**
- Consumes: `log::EventLog` (Task 3), `state_dir::{ServerFile, new_secret}` (Task 1).
- Produces:
  - `pub struct Shared { pub core: Mutex<Core>, pub secret: String, pub port: u16 }`
  - `pub struct Core { pub log: EventLog, pub last_activity: Instant }`
  - `pub fn run(shared: Arc<Shared>, server: Arc<tiny_http::Server>, idle: Duration)`
  - `pub fn header(req: &Request, name: &'static str) -> Option<String>`
  - `pub fn host_ok(req: &Request, port: u16) -> bool`
  - `pub fn bearer_ok(req: &Request, secret: &str) -> bool`
  - `pub fn error_response(status: u16, code: &str, message: &str) -> Response<std::io::Cursor<Vec<u8>>>`
  - `pub fn stopping(&self) -> bool` on `Shared` — set once by `stop` so a blocked long poll can return `stopped` instead of dying with the process

**API note learned from a prototype:** `tiny_http`'s `HeaderField::equiv` takes a `&'static str`. A helper that accepts a plain `&str` does not compile (`E0521`, "argument requires that `'1` must outlive `'static`"). The signature above is the one that works.

- [ ] **Step 1: Write the test harness**

Create `tests/support/mod.rs`:

```rust
//! Spawns a real server on a temp state directory and tears it down.
//! Every server test uses this; none of them touch the user's real state.

use artefacto::server::http::Shared;
use std::io::{Read, Write};
use std::sync::Arc;

pub struct Harness {
    pub port: u16,
    pub secret: String,
    pub dir: tempfile::TempDir,
    server: Arc<tiny_http::Server>,
}

impl Harness {
    pub fn start() -> Harness {
        let dir = tempfile::tempdir().unwrap();
        let secret = artefacto::server::state_dir::new_secret();
        let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
        let port = server.server_addr().to_ip().unwrap().port();
        let shared = Arc::new(Shared::new(dir.path(), secret.clone(), port).unwrap());
        let s = Arc::clone(&server);
        std::thread::spawn(move || {
            artefacto::server::http::run(shared, s, std::time::Duration::from_secs(3600))
        });
        Harness { port, secret, dir, server }
    }

    /// A raw request, so a test can send a hostile `Host` or omit a header
    /// entirely. A polite HTTP client would normalize exactly what we test.
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
        response
            .split_whitespace()
            .nth(1)
            .and_then(|c| c.parse().ok())
            .unwrap_or(0)
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.server.unblock();
    }
}
```

- [ ] **Step 2: Write the failing security tests**

Create `tests/server_security.rs`:

```rust
mod support;
use support::Harness;

#[test]
fn the_exact_loopback_host_is_accepted() {
    let h = Harness::start();
    let r = h.get("/healthz", &[]);
    assert_eq!(Harness::status_of(&r), 200, "127.0.0.1:<port> is the only accepted Host");
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
        "localhost resolves to 127.0.0.1 on real machines, so only the exact \
         Host check keeps a rebinding attacker out"
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
    let r = h.get("/cli/status", &[("Authorization", &auth)]);
    assert_eq!(Harness::status_of(&r), 200);
}

#[test]
fn a_wrong_bearer_is_refused() {
    let h = Harness::start();
    let r = h.get("/cli/status", &[("Authorization", "Bearer 00000000")]);
    assert_eq!(Harness::status_of(&r), 401);
}

#[test]
fn no_response_ever_carries_cors_headers() {
    let h = Harness::start();
    let auth = format!("Bearer {}", h.secret);
    for path in ["/healthz", "/cli/status"] {
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
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test --test server_security`
Expected: FAIL to compile, `could not find 'http' in 'server'`.

- [ ] **Step 4: Implement the guards and the accept loop**

Create `src/server/http.rs`:

```rust
//! Routing and the two guards every route sits behind.

use crate::server::log::EventLog;
use anyhow::Result;
use std::io::Cursor;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tiny_http::{Header, Request, Response};

/// State that must be serialized across threads. One lock, held briefly:
/// the workload is one reviewer and one agent, so contention is not a design
/// problem and a single lock is far easier to reason about than several.
pub struct Core {
    pub log: EventLog,
    pub last_activity: Instant,
}

pub struct Shared {
    pub core: Mutex<Core>,
    pub secret: String,
    pub port: u16,
}

impl Shared {
    pub fn new(dir: &Path, secret: String, port: u16) -> Result<Shared> {
        Ok(Shared {
            core: Mutex::new(Core {
                log: EventLog::open(dir)?,
                last_activity: Instant::now(),
            }),
            secret,
            port,
        })
    }
}

/// The accept loop. One thread per request, so a long poll blocks only its own
/// thread. A fixed worker pool would let N concurrent long polls starve every
/// other route, which a prototype confirmed.
pub fn run(shared: Arc<Shared>, server: Arc<tiny_http::Server>, idle: Duration) {
    loop {
        match server.recv_timeout(Duration::from_millis(250)) {
            Ok(Some(request)) => {
                let shared = Arc::clone(&shared);
                std::thread::spawn(move || handle(shared, request));
            }
            Ok(None) => {
                // Nothing arrived this tick. This is also where self-exit is
                // decided, so an abandoned daemon does not live forever.
                let quiet = {
                    let core = shared.core.lock().unwrap();
                    core.last_activity.elapsed()
                };
                if quiet > idle {
                    return;
                }
            }
            Err(_) => return,
        }
    }
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
    {
        let mut core = shared.core.lock().unwrap();
        core.last_activity = Instant::now();
    }
    let url = request.url().to_string();
    if url == "/healthz" {
        let _ = request.respond(json_response(200, "{\"ok\":true}"));
        return;
    }
    if let Some(rest) = url.strip_prefix("/cli/") {
        if !bearer_ok(&request, &shared.secret) {
            let _ = request.respond(error_response(401, "unauthorized", "bearer secret required"));
            return;
        }
        return cli_route(shared, request, rest);
    }
    let _ = request.respond(error_response(404, "not_found", "no such route"));
}

/// Task 8 fills this in with `status`, `stop`, and the agent verbs. It answers
/// now so the bearer guard has something real to protect.
fn cli_route(_shared: Arc<Shared>, request: Request, rest: &str) {
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

/// Exactly `127.0.0.1:<port>`. Not `localhost`, not a bare address, not a
/// trailing dot. This is the DNS-rebinding defense, and it is the only thing
/// standing between a malicious page and this server.
pub fn host_ok(req: &Request, port: u16) -> bool {
    header(req, "Host").is_some_and(|h| h == format!("127.0.0.1:{port}"))
}

/// `Authorization: Bearer <secret>`, compared without an early return so the
/// comparison time does not leak how much of the secret was right.
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
    Response::from_string(body)
        .with_status_code(status)
        .with_header(
            Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
                .expect("static header"),
        )
}

/// One error shape for every route. A CLI parsing a failure should never have
/// to tell an HTML error page from a JSON one.
pub fn error_response(status: u16, code: &str, message: &str) -> Response<Cursor<Vec<u8>>> {
    let body = serde_json::json!({
        "ok": false,
        "error": { "code": code, "message": message }
    });
    json_response(status, &body.to_string())
}
```

- [ ] **Step 5: Declare the module and wire the harness**

Add to `src/server/mod.rs`:

```rust
pub mod http;
```

`tests/support/mod.rs` is a module, not a test binary. Cargo compiles every file in `tests/` as its own binary, so a bare `support.rs` would be built as an empty test target and warn. The `support/mod.rs` layout avoids that; each test file declares `mod support;`.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test --test server_security`
Expected: PASS, 8 tests.

- [ ] **Step 7: Verify the gate**

Run: `cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --all`
Expected: all clean.

- [ ] **Step 8: Commit**

```bash
git add src/server/http.rs src/server/mod.rs tests/support/ tests/server_security.rs
git commit -m "feat(server): http skeleton with host and bearer guards"
```

---

### Task 5: Bootstrap tokens, the page cookie, Origin, and the CSP nonce

**Why this task exists:** the page cannot carry a bearer secret, because a token in a URL leaks into history, referrers, and the agent's transcript. Spec section 8 fixes the alternative: a one-time bootstrap URL that sets an `HttpOnly; SameSite=Strict` cookie and redirects to a tokenless URL. The CSP nonce belongs here too, because the served page carries an inline script and an inline stylesheet, and a bare `default-src 'none'` would blank it.

**Files:**
- Create: `src/server/page.rs`
- Modify: `src/server/http.rs`, `src/server/mod.rs`, `tests/server_security.rs`

**Interfaces:**
- Consumes: `http::{Shared, header, json_response, error_response}` (Task 4).
- Produces:
  - `pub fn mint_bootstrap(shared: &Shared, artifact: &str) -> String` — returns the token
  - `pub fn bootstrap_url(port: u16, token: &str) -> String`
  - `pub fn consume_bootstrap(shared: &Shared, token: &str) -> Option<String>` — the artifact id, once
  - `pub fn cookie_ok(req: &Request, shared: &Shared) -> bool`
  - `pub fn origin_ok(req: &Request, port: u16) -> bool`
  - `pub fn csp_header(port: u16, nonce: &str) -> Header`
  - `pub fn nonce() -> String`
- Adds to `Core`: `pub bootstrap: HashMap<String, (String, Instant)>`, `pub page_cookie: String`

- [ ] **Step 1: Write the failing tests**

Append to `tests/server_security.rs`:

```rust
#[test]
fn a_bootstrap_token_sets_a_cookie_and_redirects_once() {
    let h = Harness::start();
    let token = h.mint_bootstrap("plan:x");

    let first = h.get(&format!("/b/{token}"), &[]);
    assert_eq!(Harness::status_of(&first), 302, "bootstrap redirects to a tokenless URL");
    let lower = first.to_ascii_lowercase();
    assert!(lower.contains("set-cookie:"), "it must set the session cookie");
    assert!(lower.contains("httponly"), "the page's script must not be able to read it");
    assert!(lower.contains("samesite=strict"), "no cross-site request may carry it");
    assert!(!lower.contains(&token.to_ascii_lowercase()), "the redirect target carries no token");

    let second = h.get(&format!("/b/{token}"), &[]);
    assert_eq!(Harness::status_of(&second), 403, "a bootstrap token is single use");
}

#[test]
fn an_expired_bootstrap_token_is_refused() {
    let h = Harness::start();
    let token = h.mint_bootstrap_aged("plan:x", std::time::Duration::from_secs(301));
    let r = h.get(&format!("/b/{token}"), &[]);
    assert_eq!(Harness::status_of(&r), 403, "tokens expire after 5 minutes");
}

#[test]
fn a_page_route_without_the_cookie_is_refused() {
    let h = Harness::start();
    let r = h.get("/a/plan:x", &[]);
    assert_eq!(Harness::status_of(&r), 401);
}

#[test]
fn a_page_route_with_a_foreign_origin_is_refused() {
    let h = Harness::start();
    let cookie = h.session_cookie("plan:x");
    let r = h.get(
        "/a/plan:x",
        &[("Cookie", &cookie), ("Origin", "http://evil.example.com")],
    );
    assert_eq!(Harness::status_of(&r), 403, "an exact Origin match or nothing");
}

#[test]
fn the_page_carries_a_nonce_csp_that_permits_what_the_page_contains() {
    let h = Harness::start();
    let cookie = h.session_cookie("plan:x");
    let r = h.get("/a/plan:x", &[("Cookie", &cookie)]);
    assert_eq!(Harness::status_of(&r), 200);

    let csp = r
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("content-security-policy:"))
        .expect("every page response carries a CSP");
    assert!(csp.contains("default-src 'none'"));
    assert!(csp.contains("'nonce-"), "inline script and style run from a nonce");
    assert!(!csp.contains("'unsafe-inline'"), "the nonce replaces unsafe-inline");
    assert!(csp.contains(&format!("connect-src ws://127.0.0.1:{}", h.port)));
    assert!(csp.contains("frame-ancestors 'none'"));
    assert!(
        !csp.contains("sandbox"),
        "a sandbox directive makes the origin opaque and would break the cookie"
    );

    let nonce = csp
        .split("'nonce-")
        .nth(1)
        .and_then(|s| s.split('\'').next())
        .expect("a nonce value");
    assert!(
        r.contains(&format!("nonce=\"{nonce}\"")),
        "the inline tags must carry the same nonce the header names"
    );
}

#[test]
fn two_responses_never_share_a_nonce() {
    let h = Harness::start();
    let cookie = h.session_cookie("plan:x");
    let one = h.get("/a/plan:x", &[("Cookie", &cookie)]);
    let two = h.get("/a/plan:x", &[("Cookie", &cookie)]);
    let grab = |r: &str| {
        r.split("'nonce-")
            .nth(1)
            .and_then(|s| s.split('\'').next())
            .map(str::to_string)
            .unwrap()
    };
    assert_ne!(grab(&one), grab(&two), "a per-response nonce is the whole point");
}
```

Add to `tests/support/mod.rs`:

```rust
impl Harness {
    /// Mints through the same path `push` and `open` use.
    pub fn mint_bootstrap(&self, artifact: &str) -> String {
        artefacto::server::page::mint_bootstrap(&self.shared, artifact)
    }

    /// Mints a token whose clock is already `age` old, so expiry is testable
    /// without sleeping for five minutes.
    pub fn mint_bootstrap_aged(&self, artifact: &str, age: std::time::Duration) -> String {
        artefacto::server::page::mint_bootstrap_aged(&self.shared, artifact, age)
    }

    /// Walks the real bootstrap redirect and returns the cookie it set.
    pub fn session_cookie(&self, artifact: &str) -> String {
        let token = self.mint_bootstrap(artifact);
        let response = self.get(&format!("/b/{token}"), &[]);
        response
            .lines()
            .find(|l| l.to_ascii_lowercase().starts_with("set-cookie:"))
            .and_then(|l| l.splitn(2, ": ").nth(1))
            .and_then(|v| v.split(';').next())
            .expect("bootstrap must set a cookie")
            .to_string()
    }
}
```

`Harness` gains a `pub shared: Arc<Shared>` field, kept alongside the thread so tests can mint through the server's own state rather than a copy.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test server_security`
Expected: FAIL to compile, `could not find 'page' in 'server'`.

- [ ] **Step 3: Implement the page routes**

Create `src/server/page.rs`:

```rust
//! How a browser authenticates, and the policy the page is served under.
//!
//! The page cannot hold the bearer secret: a token in a URL ends up in
//! history, in a referrer, and in the agent's transcript. So a one-time
//! bootstrap URL trades itself for an HttpOnly cookie and redirects to a
//! tokenless address.

use crate::server::http::{error_response, header, json_response, Shared};
use crate::server::state_dir::new_secret;
use std::io::Cursor;
use std::time::{Duration, Instant};
use tiny_http::{Header, Request, Response};

pub const BOOTSTRAP_TTL: Duration = Duration::from_secs(300);
pub const COOKIE_NAME: &str = "artefacto_session";

pub fn mint_bootstrap(shared: &Shared, artifact: &str) -> String {
    mint_bootstrap_aged(shared, artifact, Duration::ZERO)
}

/// `age` backdates the token so expiry can be tested without waiting. Real
/// callers pass `Duration::ZERO`.
pub fn mint_bootstrap_aged(shared: &Shared, artifact: &str, age: Duration) -> String {
    let token = new_secret();
    let issued = Instant::now() - age;
    let mut core = shared.core.lock().unwrap();
    core.bootstrap
        .insert(token.clone(), (artifact.to_string(), issued));
    token
}

pub fn bootstrap_url(port: u16, token: &str) -> String {
    format!("http://127.0.0.1:{port}/b/{token}")
}

/// Consumes the token whatever the outcome. A token that was presented once is
/// spent, even if it had already expired, so a leaked URL is never retryable.
pub fn consume_bootstrap(shared: &Shared, token: &str) -> Option<String> {
    let mut core = shared.core.lock().unwrap();
    let (artifact, issued) = core.bootstrap.remove(token)?;
    if issued.elapsed() > BOOTSTRAP_TTL {
        return None;
    }
    Some(artifact)
}

pub fn handle_bootstrap(shared: &Shared, request: Request, token: &str) {
    let Some(artifact) = consume_bootstrap(shared, token) else {
        let _ = request.respond(error_response(
            403,
            "bad_bootstrap",
            "this link was already used or has expired; run `artefacto open` for a fresh one",
        ));
        return;
    };
    let cookie = { shared.core.lock().unwrap().page_cookie.clone() };
    let response = Response::empty(302)
        .with_header(
            Header::from_bytes(
                &b"Set-Cookie"[..],
                format!("{COOKIE_NAME}={cookie}; HttpOnly; SameSite=Strict; Path=/").as_bytes(),
            )
            .expect("cookie header"),
        )
        .with_header(
            Header::from_bytes(&b"Location"[..], format!("/a/{artifact}").as_bytes())
                .expect("location header"),
        );
    let _ = request.respond(response);
}

pub fn cookie_ok(req: &Request, shared: &Shared) -> bool {
    let expected = { shared.core.lock().unwrap().page_cookie.clone() };
    let Some(raw) = header(req, "Cookie") else {
        return false;
    };
    raw.split(';')
        .map(str::trim)
        .filter_map(|kv| kv.strip_prefix(&format!("{COOKIE_NAME}=")))
        .any(|v| v == expected)
}

/// Absent is fine — a plain navigation sends no Origin. Present and wrong is
/// not.
pub fn origin_ok(req: &Request, port: u16) -> bool {
    match header(req, "Origin") {
        None => true,
        Some(o) => o == format!("http://127.0.0.1:{port}"),
    }
}

pub fn nonce() -> String {
    new_secret()[..32].to_string()
}

/// Spec section 8. No `sandbox`: it makes the origin opaque, which breaks the
/// cookie the page authenticates with.
pub fn csp_header(port: u16, nonce: &str) -> Header {
    let policy = format!(
        "default-src 'none'; script-src 'nonce-{nonce}'; style-src 'nonce-{nonce}'; \
         img-src data:; font-src data:; connect-src ws://127.0.0.1:{port}; \
         base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
    );
    Header::from_bytes(&b"Content-Security-Policy"[..], policy.as_bytes()).expect("csp header")
}

/// The page served in this plan is the existing static render with a nonce
/// stamped onto its inline tags. Plan 3 replaces the asset; the nonce contract
/// stays.
pub fn serve_page(shared: &Shared, request: Request, artifact: &str) {
    if !cookie_ok(&request, shared) {
        let _ = request.respond(error_response(401, "unauthorized", "no session cookie"));
        return;
    }
    if !origin_ok(&request, shared.port) {
        let _ = request.respond(error_response(403, "bad_origin", "origin not allowed"));
        return;
    }
    let n = nonce();
    let html = render_with_nonce(shared, artifact, &n);
    let response = Response::from_string(html)
        .with_header(
            Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..])
                .expect("content type"),
        )
        .with_header(csp_header(shared.port, &n));
    let _ = request.respond(response);
}

/// Stamps the nonce onto the two inline tags the renderer emits. A CSP that
/// names a nonce the document does not carry blanks the page, so this and
/// `csp_header` must always be changed together.
fn render_with_nonce(shared: &Shared, artifact: &str, nonce: &str) -> String {
    let body = { shared.core.lock().unwrap().rendered_body(artifact) };
    body.replace("<script>", &format!("<script nonce=\"{nonce}\">"))
        .replace("<style>", &format!("<style nonce=\"{nonce}\">"))
}

/// Kept so `json_response` and `Cursor` stay used while Task 6 lands.
pub fn ok_json() -> Response<Cursor<Vec<u8>>> {
    json_response(200, "{\"ok\":true}")
}
```

`Core` gains two fields, initialized in `Shared::new`:

```rust
pub bootstrap: std::collections::HashMap<String, (String, std::time::Instant)>,
pub page_cookie: String,
```

`page_cookie` is `new_secret()` at construction. It is not the bearer secret: the page and the CLI authenticate on separate credentials, so a page compromise never yields the CLI's.

`Core::rendered_body(artifact)` returns the current revision's rendered HTML. Until Task 11 stores revisions, it returns the static render of the last pushed plan, or a placeholder document for an unknown artifact.

- [ ] **Step 4: Route to the new handlers**

In `src/server/http.rs`'s `handle`, before the `/cli/` arm:

```rust
    if let Some(token) = url.strip_prefix("/b/") {
        return crate::server::page::handle_bootstrap(&shared, request, token);
    }
    if let Some(artifact) = url.strip_prefix("/a/") {
        return crate::server::page::serve_page(&shared, request, artifact);
    }
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --test server_security`
Expected: PASS, 14 tests.

- [ ] **Step 6: Verify the gate**

Run: `cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --all`
Expected: all clean.

- [ ] **Step 7: Commit**

```bash
git add src/server/page.rs src/server/http.rs src/server/mod.rs tests/
git commit -m "feat(server): bootstrap cookie auth and a per-response CSP nonce"
```

---

### Task 6: The page WebSocket and the fake page client

**Why this task exists:** the reviewer's page is the other half of every event flow, and plan 3 does not exist yet. Without a fake page client, every delivery test in Tasks 9 through 12 would have nothing to talk to. Spec section 8 also requires the handshake to carry the cookie and an exact Origin, which is a different code path from a normal GET and needs its own test.

**Files:**
- Create: `src/server/socket.rs`
- Modify: `src/server/http.rs`, `src/server/mod.rs`, `tests/support/mod.rs`, `tests/server_security.rs`

**Interfaces:**
- Consumes: `page::{cookie_ok, origin_ok}` (Task 5), `event::Frame` (Task 2).
- Produces:
  - `pub struct PageSockets` — the connected pages, behind the same lock discipline as `Core`
  - `pub fn handle_upgrade(shared: &Arc<Shared>, request: Request)`
  - `pub fn broadcast(shared: &Shared, frame: &Frame)`
  - `pub fn page_count(shared: &Shared) -> usize`
- Test-side: `Harness::connect_page() -> FakePage` with `FakePage::{send, next_frame, close}`

**API notes confirmed by a prototype:** `tiny_http::Request::upgrade(protocol, response)` returns `Box<dyn tiny_http::ReadWrite + Send>`, which does not itself implement `Read`/`Write`; wrap it in a newtype that delegates. `tungstenite::WebSocket::from_raw_socket(sock, Role::Server, None)` then drives it. The client side builds a request with `IntoClientRequest` and inserts `Cookie` and `Origin` headers before `tungstenite::connect`. Both halves are verified working.

- [ ] **Step 1: Write the failing tests**

Append to `tests/server_security.rs`:

```rust
#[test]
fn a_socket_without_the_cookie_is_refused() {
    let h = Harness::start();
    assert!(h.connect_page_raw(None, None).is_err(), "no cookie, no socket");
}

#[test]
fn a_socket_with_a_foreign_origin_is_refused() {
    let h = Harness::start();
    let cookie = h.session_cookie("plan:x");
    assert!(
        h.connect_page_raw(Some(&cookie), Some("http://evil.example.com")).is_err(),
        "the socket enforces Origin as strictly as the page routes do"
    );
}

#[test]
fn a_socket_with_the_cookie_receives_broadcast_frames() {
    let h = Harness::start();
    let mut page = h.connect_page();
    h.broadcast_test_frame(7);
    let frame = page.next_frame();
    assert_eq!(frame["format"], "artefacto.frame/1");
    assert_eq!(frame["seq"], 7);
}

#[test]
fn a_closed_page_is_dropped_from_the_broadcast_set() {
    let h = Harness::start();
    let page = h.connect_page();
    assert_eq!(h.page_count(), 1);
    drop(page);
    // The server notices on its next write, so broadcast once and re-check.
    h.broadcast_test_frame(1);
    h.wait_for(|| h.page_count() == 0, "the closed page should be dropped");
}
```

Append to `tests/support/mod.rs`:

```rust
use tungstenite::client::IntoClientRequest;

pub struct FakePage {
    ws: tungstenite::WebSocket<std::net::TcpStream>,
}

impl FakePage {
    pub fn next_frame(&mut self) -> serde_json::Value {
        loop {
            let msg = self.ws.read().expect("the socket must stay open");
            if let tungstenite::Message::Text(t) = msg {
                return serde_json::from_str(&t).expect("frames are JSON");
            }
        }
    }

    pub fn send(&mut self, value: serde_json::Value) {
        self.ws
            .send(tungstenite::Message::text(value.to_string()))
            .expect("send");
    }
}

impl Harness {
    pub fn connect_page(&self) -> FakePage {
        let cookie = self.session_cookie("plan:x");
        let origin = format!("http://127.0.0.1:{}", self.port);
        self.connect_page_raw(Some(&cookie), Some(&origin))
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
        match tungstenite::connect(req) {
            Ok((ws, _)) => Ok(FakePage { ws: unwrap_stream(ws) }),
            Err(e) => Err(e.to_string()),
        }
    }

    pub fn page_count(&self) -> usize {
        artefacto::server::socket::page_count(&self.shared)
    }

    pub fn broadcast_test_frame(&self, seq: u64) {
        let frame = artefacto::server::event::Frame {
            format: artefacto::server::event::FRAME_FORMAT.to_string(),
            seq,
            events: vec![],
        };
        artefacto::server::socket::broadcast(&self.shared, &frame);
    }

    /// Polls a condition rather than sleeping a fixed time, so the suite is
    /// neither slow nor flaky on a loaded machine.
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

/// `tungstenite::connect` returns a `MaybeTlsStream`; this server is plaintext
/// loopback only, so the plain variant is the only one that can occur.
fn unwrap_stream(
    ws: tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
) -> tungstenite::WebSocket<std::net::TcpStream> {
    let (stream, config) = {
        let cfg = *ws.get_config();
        match ws.into_inner() {
            tungstenite::stream::MaybeTlsStream::Plain(s) => (s, cfg),
            _ => unreachable!("loopback is never TLS"),
        }
    };
    tungstenite::WebSocket::from_raw_socket(stream, tungstenite::protocol::Role::Client, Some(config))
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test server_security`
Expected: FAIL to compile, `could not find 'socket' in 'server'`.

- [ ] **Step 3: Implement the socket module**

Create `src/server/socket.rs`:

```rust
//! The page's WebSocket. One per open tab; frames fan out to all of them.

use crate::server::event::Frame;
use crate::server::http::{error_response, header, Shared};
use crate::server::page::{cookie_ok, origin_ok};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use tiny_http::{Header, Request, Response};
use tungstenite::handshake::derive_accept_key;
use tungstenite::protocol::{Role, WebSocket};
use tungstenite::Message;

/// `Request::upgrade` hands back a boxed `ReadWrite`, which does not itself
/// implement `Read` and `Write`. Delegating through a newtype is the whole fix.
pub struct Sock(Box<dyn tiny_http::ReadWrite + Send>);

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

type Page = Arc<Mutex<WebSocket<Sock>>>;

#[derive(Default)]
pub struct PageSockets {
    pages: Mutex<Vec<Page>>,
}

pub fn page_count(shared: &Shared) -> usize {
    shared.sockets.pages.lock().unwrap().len()
}

/// Writes to every connected page and drops the ones that fail. A page that
/// went away is not an error: the reviewer closed a tab.
pub fn broadcast(shared: &Shared, frame: &Frame) {
    let text = serde_json::to_string(frame).expect("a frame always serializes");
    let mut pages = shared.sockets.pages.lock().unwrap();
    pages.retain(|page| {
        let mut ws = page.lock().unwrap();
        ws.send(Message::text(text.clone())).is_ok()
    });
}

pub fn handle_upgrade(shared: &Arc<Shared>, request: Request) {
    if !cookie_ok(&request, shared) {
        let _ = request.respond(error_response(401, "unauthorized", "no session cookie"));
        return;
    }
    if !origin_ok(&request, shared.port) {
        let _ = request.respond(error_response(403, "bad_origin", "origin not allowed"));
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
        .with_header(
            Header::from_bytes(&b"Sec-WebSocket-Accept"[..], accept.as_bytes()).expect("accept"),
        );
    let stream = request.upgrade("websocket", response);
    let ws = Arc::new(Mutex::new(WebSocket::from_raw_socket(
        Sock(stream),
        Role::Server,
        None,
    )));
    shared.sockets.pages.lock().unwrap().push(Arc::clone(&ws));

    // This thread owns the read side for the life of the connection. Plan 3
    // gives these messages meaning; here they are logged and dropped, which is
    // enough to prove the transport and to keep the socket's ping/pong alive.
    loop {
        let msg = {
            let mut guard = ws.lock().unwrap();
            guard.read()
        };
        match msg {
            Ok(Message::Close(_)) | Err(_) => break,
            Ok(_) => continue,
        }
    }
    let mut pages = shared.sockets.pages.lock().unwrap();
    pages.retain(|p| !Arc::ptr_eq(p, &ws));
}
```

`Shared` gains `pub sockets: PageSockets`, defaulted in `Shared::new`.

- [ ] **Step 4: Route the upgrade**

In `handle`, before the `/a/` arm, because `/ws` must not be read as an artifact id:

```rust
    if url == "/ws" {
        return crate::server::socket::handle_upgrade(&shared, request);
    }
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --test server_security`
Expected: PASS, 18 tests.

- [ ] **Step 6: Verify the gate**

Run: `cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --all`
Expected: all clean.

- [ ] **Step 7: Commit**

```bash
git add src/server/socket.rs src/server/http.rs src/server/mod.rs tests/
git commit -m "feat(server): cookie-gated page websocket and a fake page client"
```

---

### Task 7: The agent lease and session tokens

**Why this task exists:** spec section 4.2 makes the lease the thing that lets a poll-mode agent hold together across many short commands. Without it, an `await` returning every 90 seconds would drop and re-take a lock on every cycle, and presence would flicker. The generation number is what stops a superseded agent from still writing, which is the failure the spec calls out by name.

**Files:**
- Create: `src/server/lease.rs`, `tests/server_lease.rs`
- Modify: `src/server/http.rs`, `src/server/mod.rs`

**Interfaces:**
- Consumes: `state_dir::{is_alive, new_secret}` (Task 1), `http::Shared` (Task 4).
- Produces:
  - `pub struct Lease { pub name: String, pub generation: u64, pub token: String, pub pid: u32, pub acked_seq: u64, pub taken_at: Instant, pub mode: Mode }`
  - `pub enum Mode { Live, Waiting }` — `events --follow` is live, `await` is waiting
  - `pub enum LeaseError { Held { holder: String, age_secs: u64 }, Superseded }`
  - `pub fn acquire(shared: &Shared, name: &str, pid: u32, mode: Mode, takeover: bool) -> Result<Lease, LeaseError>`
  - `pub fn validate(shared: &Shared, token: &str) -> Result<Lease, LeaseError>` — also refreshes the TTL
  - `pub fn release(shared: &Shared, token: &str)`
  - `pub fn current(shared: &Shared) -> Option<Lease>` — after expiry and dead-pid collection
  - `pub const TTL: Duration = Duration::from_secs(300);`

- [ ] **Step 1: Write the failing tests**

Create `tests/server_lease.rs`:

```rust
mod support;
use artefacto::server::lease::{self, LeaseError, Mode};
use support::Harness;

#[test]
fn the_first_agent_takes_the_lease() {
    let h = Harness::start();
    let l = lease::acquire(&h.shared, "claude", std::process::id(), Mode::Waiting, false).unwrap();
    assert_eq!(l.generation, 1);
    assert!(!l.token.is_empty());
    assert_eq!(l.acked_seq, 0, "a fresh lease starts at the beginning of the log");
}

#[test]
fn a_second_agent_is_refused_and_told_who_holds_it() {
    let h = Harness::start();
    lease::acquire(&h.shared, "claude", std::process::id(), Mode::Waiting, false).unwrap();
    let err = lease::acquire(&h.shared, "codex", std::process::id(), Mode::Waiting, false)
        .expect_err("one agent at a time");
    match err {
        LeaseError::Held { holder, .. } => {
            assert_eq!(holder, "claude", "the refusal names the holder so nobody has to guess")
        }
        other => panic!("expected Held, got {other:?}"),
    }
}

#[test]
fn takeover_bumps_the_generation_and_supersedes_the_old_token() {
    let h = Harness::start();
    let first = lease::acquire(&h.shared, "claude", std::process::id(), Mode::Waiting, false).unwrap();
    let second = lease::acquire(&h.shared, "codex", std::process::id(), Mode::Waiting, true).unwrap();

    assert_eq!(second.generation, 2);
    assert!(matches!(
        lease::validate(&h.shared, &first.token),
        Err(LeaseError::Superseded)
    ), "a stale agent that lost the lease must not be able to write");
    assert!(lease::validate(&h.shared, &second.token).is_ok());
}

#[test]
fn an_expired_lease_is_released() {
    let h = Harness::start();
    let l = lease::acquire(&h.shared, "claude", std::process::id(), Mode::Waiting, false).unwrap();
    h.age_lease(lease::TTL + std::time::Duration::from_secs(1));

    assert!(lease::current(&h.shared).is_none(), "past its TTL the lease is gone");
    let next = lease::acquire(&h.shared, "codex", std::process::id(), Mode::Waiting, false)
        .expect("an expired lease does not need a takeover");
    assert_eq!(next.generation, 2);
    assert!(matches!(
        lease::validate(&h.shared, &l.token),
        Err(LeaseError::Superseded)
    ));
}

#[test]
fn a_lease_held_by_a_dead_process_is_released() {
    let h = Harness::start();
    // Pid 0 is never a live user process, so this stands in for a crashed agent.
    lease::acquire(&h.shared, "claude", 0, Mode::Waiting, false).unwrap();
    assert!(
        lease::current(&h.shared).is_none(),
        "a crashed agent must not hold the lease until its TTL runs out"
    );
    assert!(lease::acquire(&h.shared, "codex", std::process::id(), Mode::Waiting, false).is_ok());
}

#[test]
fn any_agent_call_refreshes_the_ttl() {
    let h = Harness::start();
    let l = lease::acquire(&h.shared, "claude", std::process::id(), Mode::Waiting, false).unwrap();
    h.age_lease(lease::TTL - std::time::Duration::from_secs(5));
    lease::validate(&h.shared, &l.token).expect("still inside the TTL");
    h.age_lease(lease::TTL - std::time::Duration::from_secs(5));
    lease::validate(&h.shared, &l.token)
        .expect("the previous call refreshed it, so this is not expired either");
}

#[test]
fn releasing_frees_the_lease_immediately() {
    let h = Harness::start();
    let l = lease::acquire(&h.shared, "claude", std::process::id(), Mode::Live, false).unwrap();
    lease::release(&h.shared, &l.token);
    assert!(lease::current(&h.shared).is_none(), "a --follow disconnect frees it at once");
    assert!(lease::acquire(&h.shared, "codex", std::process::id(), Mode::Waiting, false).is_ok());
}
```

Add to `tests/support/mod.rs`:

```rust
impl Harness {
    /// Backdates the lease so TTL behaviour is testable without sleeping for
    /// five minutes. The production path never calls this.
    pub fn age_lease(&self, by: std::time::Duration) {
        artefacto::server::lease::age_for_tests(&self.shared, by);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test server_lease`
Expected: FAIL to compile, `could not find 'lease' in 'server'`.

- [ ] **Step 3: Implement the lease**

Create `src/server/lease.rs`:

```rust
//! One agent acts at a time. The token, not the process, is the identity.

use crate::server::http::Shared;
use crate::server::state_dir::{is_alive, new_secret};
use std::time::{Duration, Instant};

pub const TTL: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// `events --follow` holds the socket open: the page shows "agent live".
    Live,
    /// `await` is between polls: the page shows "agent waiting".
    Waiting,
}

#[derive(Debug, Clone)]
pub struct Lease {
    pub name: String,
    pub generation: u64,
    pub token: String,
    pub pid: u32,
    pub acked_seq: u64,
    pub taken_at: Instant,
    pub mode: Mode,
}

#[derive(Debug)]
pub enum LeaseError {
    Held { holder: String, age_secs: u64 },
    Superseded,
}

/// Collects a lease that has expired or whose process is gone, then returns
/// whatever is left. Every entry point calls this first, so a dead holder is
/// never observed by anyone.
pub fn current(shared: &Shared) -> Option<Lease> {
    let mut core = shared.core.lock().unwrap();
    if let Some(l) = &core.lease {
        if l.taken_at.elapsed() > TTL || !is_alive(l.pid) {
            core.lease = None;
        }
    }
    core.lease.clone()
}

/// The generation only ever increases, and it increases whenever the lease
/// changes hands. That is what makes an old token identifiable as stale
/// rather than merely unknown.
pub fn acquire(
    shared: &Shared,
    name: &str,
    pid: u32,
    mode: Mode,
    takeover: bool,
) -> Result<Lease, LeaseError> {
    let held = current(shared);
    let mut core = shared.core.lock().unwrap();
    let next_generation = core.lease_generation + 1;

    if let Some(existing) = held {
        if !takeover {
            return Err(LeaseError::Held {
                holder: existing.name.clone(),
                age_secs: existing.taken_at.elapsed().as_secs(),
            });
        }
    }

    let lease = Lease {
        name: name.to_string(),
        generation: next_generation,
        token: new_secret(),
        pid,
        acked_seq: core.acked_seq_for(name),
        taken_at: Instant::now(),
        mode,
    };
    core.lease_generation = next_generation;
    core.lease = Some(lease.clone());
    Ok(lease)
}

/// Validating is also refreshing: any agent call keeps the lease alive, which
/// is what lets a 90-second `await` cycle hold a 5-minute lease.
pub fn validate(shared: &Shared, token: &str) -> Result<Lease, LeaseError> {
    let Some(existing) = current(shared) else {
        return Err(LeaseError::Superseded);
    };
    if existing.token != token {
        return Err(LeaseError::Superseded);
    }
    let mut core = shared.core.lock().unwrap();
    if let Some(l) = core.lease.as_mut() {
        l.taken_at = Instant::now();
    }
    Ok(existing)
}

pub fn release(shared: &Shared, token: &str) {
    let mut core = shared.core.lock().unwrap();
    if core.lease.as_ref().is_some_and(|l| l.token == token) {
        core.lease = None;
    }
}

/// Test-only clock control. Production code never backdates a lease.
#[doc(hidden)]
pub fn age_for_tests(shared: &Shared, by: Duration) {
    let mut core = shared.core.lock().unwrap();
    if let Some(l) = core.lease.as_mut() {
        l.taken_at -= by;
    }
}
```

`Core` gains `pub lease: Option<Lease>` and `pub lease_generation: u64`, both starting empty and zero, plus `fn acked_seq_for(&self, name: &str) -> u64`, which reads the cursor Task 9 persists and returns 0 until then.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --test server_lease`
Expected: PASS, 7 tests.

- [ ] **Step 5: Verify the gate**

Run: `cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --all`
Expected: all clean.

- [ ] **Step 6: Commit**

```bash
git add src/server/lease.rs src/server/http.rs src/server/mod.rs tests/
git commit -m "feat(server): agent lease with session tokens and generations"
```

---

### Task 8: `serve`, `stop`, `status`, `open`, and the daemon

**Why this task exists:** this is the first task that produces something a person can run. Spec section 4.2 fixes the daemonizing sequence and the port and secret persistence that let an open page reconnect after a crash. The self-exit timer is here too, because a daemon nobody can see is a daemon nobody remembers to kill.

**Files:**
- Create: `src/server/daemon.rs`, `src/commands/serve.rs`, `src/client.rs`, `tests/server_lifecycle.rs`
- Modify: `src/cli.rs`, `src/commands/mod.rs`, `src/server/mod.rs`, `src/lib.rs`

**Interfaces:**
- Consumes: `state_dir::*` (Task 1), `http::{Shared, run}` (Task 4), `page::bootstrap_url` (Task 5).
- Produces:
  - `pub fn daemonize(log_path: &Path) -> anyhow::Result<()>` — returns only in the grandchild
  - `pub fn serve(args: &ServeArgs) -> anyhow::Result<()>`
  - `pub fn stop() -> anyhow::Result<()>`
  - `pub fn status(json: bool) -> anyhow::Result<()>`
  - `pub fn open(artifact: Option<&str>) -> anyhow::Result<()>`
  - `pub struct Client { base: String, secret: String }` with `get`/`post` returning `anyhow::Result<serde_json::Value>`
  - Exit code **4** when no server is running

**Verified by a spike:** the double fork plus `setsid` sequence below was run on macOS. The daemon was reparented to pid 1, survived the shell that started it exiting, and kept serving its port. Binding *before* the fork is deliberate: a bind failure must be reported to the caller's stderr, not lost into a log file the caller has not been told about yet.

- [ ] **Step 1: Write the failing tests**

Create `tests/server_lifecycle.rs`:

```rust
use assert_cmd::Command;
use predicates::prelude::*;

fn artefacto(dir: &std::path::Path) -> Command {
    let mut c = Command::cargo_bin("artefacto").unwrap();
    c.current_dir(dir);
    // Every test gets its own state root, so a test run never touches the
    // developer's real server.
    c.env("XDG_STATE_HOME", dir.join("state"));
    c
}

fn git_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir.path())
        .status()
        .unwrap();
    dir
}

#[test]
fn status_without_a_server_exits_4() {
    let repo = git_repo();
    artefacto(repo.path())
        .args(["status", "--json"])
        .assert()
        .code(4)
        .stdout(predicate::str::contains("\"ok\":false"));
}

#[test]
fn serve_then_status_then_stop() {
    let repo = git_repo();
    artefacto(repo.path()).args(["serve", "--no-open"]).assert().success();

    artefacto(repo.path())
        .args(["status", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"port\""))
        .stdout(predicate::str::contains("\"ok\":true"))
        .stdout(predicate::str::contains("\"secret\"").not().and(
            predicate::str::contains("\"session\"").not()
        ));

    artefacto(repo.path()).args(["stop"]).assert().success();
    artefacto(repo.path()).args(["status", "--json"]).assert().code(4);
}

#[test]
fn a_restart_rebinds_the_same_port() {
    let repo = git_repo();
    artefacto(repo.path()).args(["serve", "--no-open"]).assert().success();
    let first = port_of(repo.path());
    artefacto(repo.path()).args(["stop"]).assert().success();

    artefacto(repo.path()).args(["serve", "--no-open"]).assert().success();
    let second = port_of(repo.path());
    artefacto(repo.path()).args(["stop"]).assert().success();

    assert_eq!(first, second, "an open page must be able to reconnect after a restart");
}

#[test]
fn a_second_serve_does_not_start_a_second_daemon() {
    let repo = git_repo();
    artefacto(repo.path()).args(["serve", "--no-open"]).assert().success();
    let first = pid_of(repo.path());
    artefacto(repo.path()).args(["serve", "--no-open"]).assert().success();
    let second = pid_of(repo.path());
    artefacto(repo.path()).args(["stop"]).assert().success();
    assert_eq!(first, second, "serve is idempotent while a live server exists");
}

#[test]
fn a_stale_server_file_with_a_dead_pid_is_replaced() {
    let repo = git_repo();
    artefacto(repo.path()).args(["serve", "--no-open"]).assert().success();
    let path = server_json_path(repo.path());
    let live = pid_of(repo.path());
    artefacto(repo.path()).args(["stop"]).assert().success();

    // Leave a file behind pointing at a process that is gone.
    let mut v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    v["pid"] = serde_json::json!(0);
    std::fs::write(&path, v.to_string()).unwrap();

    artefacto(repo.path()).args(["serve", "--no-open"]).assert().success();
    let fresh = pid_of(repo.path());
    artefacto(repo.path()).args(["stop"]).assert().success();
    assert_ne!(fresh, live, "a dead pid means no server, so serve starts a real one");
}

#[test]
fn open_prints_a_bootstrap_url_that_carries_a_token() {
    let repo = git_repo();
    artefacto(repo.path()).args(["serve", "--no-open"]).assert().success();
    let out = artefacto(repo.path())
        .args(["open", "--artifact", "plan:x", "--no-open"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    artefacto(repo.path()).args(["stop"]).assert().success();

    let url = String::from_utf8(out).unwrap();
    assert!(url.contains("http://127.0.0.1:"));
    assert!(url.contains("/b/"), "the bootstrap path is what sets the cookie");
}
```

Add the three small readers `port_of`, `pid_of`, and `server_json_path` to the bottom of the same file; they read `server.json` under `XDG_STATE_HOME` and are three lines each:

```rust
fn server_json_path(repo: &std::path::Path) -> std::path::PathBuf {
    let root = artefacto::server::state_dir::repo_root(repo).unwrap();
    // The test sets XDG_STATE_HOME on the child process, so read it the same
    // way the child does rather than assuming the default location.
    std::env::set_var("XDG_STATE_HOME", repo.join("state"));
    artefacto::server::state_dir::state_dir(&root).join("server.json")
}

fn field_of(repo: &std::path::Path, key: &str) -> u64 {
    let raw = std::fs::read_to_string(server_json_path(repo)).unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    v[key].as_u64().unwrap()
}

fn port_of(repo: &std::path::Path) -> u64 {
    field_of(repo, "port")
}

fn pid_of(repo: &std::path::Path) -> u64 {
    field_of(repo, "pid")
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test server_lifecycle`
Expected: FAIL, `unrecognized subcommand 'serve'`.

- [ ] **Step 3: Add the CLI types**

In `src/cli.rs`, add to `Command`:

```rust
    /// Start the review server for this repository.
    Serve(ServeArgs),
    /// Stop the running server.
    Stop,
    /// Report what the server is doing.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Mint a fresh bootstrap URL and open the browser.
    Open {
        #[arg(long)]
        artifact: Option<String>,
        #[arg(long)]
        no_open: bool,
    },
```

```rust
#[derive(Args, Debug)]
pub struct ServeArgs {
    /// Bind this port instead of the recorded one.
    #[arg(long)]
    pub port: Option<u16>,
    /// Nudge the agent after this long with the page open and quiet.
    #[arg(long, default_value = "15m")]
    pub idle: String,
    /// Nudge after every page socket has been closed this long.
    #[arg(long, default_value = "5m")]
    pub away: String,
    /// Whether passive events flush on their own.
    #[arg(long, value_enum, default_value = "digest")]
    pub passive: PassiveMode,
    #[arg(long)]
    pub no_open: bool,
    /// Stay in the foreground instead of daemonizing.
    #[arg(long)]
    pub foreground: bool,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum PassiveMode {
    Digest,
    Live,
}
```

- [ ] **Step 4: Implement the daemon**

Create `src/server/daemon.rs`:

```rust
//! Detaching from the terminal. Proven on macOS by a spike before this plan
//! was written: the grandchild is reparented to pid 1 and outlives the shell
//! that started it.

use anyhow::{Context, Result};
use std::path::Path;

extern "C" {
    fn fork() -> i32;
    fn setsid() -> i32;
}

/// Fork, `setsid`, fork again, then redirect the standard descriptors into
/// `log_path`. Returns only in the final grandchild; the parent and the
/// intermediate child call `exit(0)`.
///
/// The second fork is what stops the daemon from ever acquiring a controlling
/// terminal: after `setsid` the child is a session leader, and only a session
/// leader can acquire one.
pub fn daemonize(log_path: &Path) -> Result<()> {
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .with_context(|| format!("opening {}", log_path.display()))?;

    // SAFETY: the standard double-fork daemonize sequence. No allocation and
    // no locking happens between the forks, so no lock can be inherited held.
    unsafe {
        if fork() != 0 {
            std::process::exit(0);
        }
        if setsid() < 0 {
            anyhow::bail!("setsid failed");
        }
        if fork() != 0 {
            std::process::exit(0);
        }
        let fd = std::os::unix::io::AsRawFd::as_raw_fd(&log);
        libc::dup2(fd, libc::STDOUT_FILENO);
        libc::dup2(fd, libc::STDERR_FILENO);
        let devnull = std::fs::File::open("/dev/null")?;
        libc::dup2(std::os::unix::io::AsRawFd::as_raw_fd(&devnull), libc::STDIN_FILENO);
        std::mem::forget(devnull);
    }
    std::mem::forget(log);
    Ok(())
}
```

- [ ] **Step 5: Implement the commands**

Create `src/commands/serve.rs`. The shape that matters:

```rust
pub fn serve(args: &ServeArgs) -> Result<()> {
    let root = state_dir::repo_root(&std::env::current_dir()?)?;
    let dir = state_dir::state_dir(&root);

    // Idempotent: a live server means there is nothing to do.
    if let Some(existing) = state_dir::read_server_file(&dir) {
        if !args.no_open {
            open_artifact(&existing, None)?;
        }
        return Ok(());
    }

    // Bind before forking so a refusal reaches the caller's stderr. Some agent
    // sandboxes forbid listening sockets, and the message has to say so.
    let recorded = previous_port(&dir);
    let listener = bind_preferring(args.port.or(recorded))
        .context("could not bind a loopback port; run `artefacto serve --foreground` \
                  in your own terminal if this environment forbids listening sockets")?;
    let port = listener.server_addr().to_ip().expect("an ip listener").port();

    let secret = previous_secret(&dir).unwrap_or_else(state_dir::new_secret);

    if !args.foreground {
        daemon::daemonize(&dir.join("server.log"))?;
    }

    state_dir::write_server_file(&dir, &ServerFile {
        pid: std::process::id(),
        port,
        secret: secret.clone(),
        started_at: now_rfc3339(),
    })?;

    let shared = Arc::new(Shared::new(&dir, secret, port)?);
    http::run(shared, Arc::new(listener), Duration::from_secs(30 * 60));
    let _ = std::fs::remove_file(dir.join("server.json"));
    Ok(())
}
```

Notes an implementer needs:

- `bind_preferring(Some(p))` tries `p` and falls back to port 0 when it is taken. The spec requires re-binding the recorded port so an open page reconnects; when that fails, the new port is recorded and open pages show "server restarted on a new port" once their retries run out.
- The secret is reused from the previous `server.json` when present, so the page's cookie survives a restart. Only `clean` rotates it.
- `stop` reads `server.json`, calls `POST /cli/stop`, and falls back to `SIGTERM` if the route does not answer within two seconds.
- `status` prints port, artifacts, revisions, thread counts, last event seq, each lease's `acked_seq`, the lease holder and its age, reviewer presence, and the exact `events --follow` command line. It **never** prints the secret or a session token: spec section 5 says a token must not be obtainable from something that only reads status. The test above asserts that absence.
- `open` mints a bootstrap token, prints the URL, and opens the browser unless `--no-open`. `--artifact` is required in this plan; the no-argument form opens the artifact index, which is plan 4.

`src/client.rs` is the CLI's side: it reads `server.json`, sends `Authorization: Bearer <secret>` on every call, and maps a missing or dead server to exit code 4.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test --test server_lifecycle`
Expected: PASS, 6 tests.

- [ ] **Step 7: Verify by hand, and report what you saw**

```bash
cargo run --quiet -- serve --no-open
cargo run --quiet -- status --json
cargo run --quiet -- open --artifact plan:x
```

Expected: `status` prints a port and `"ok":true`; the `open` URL loads a page in the browser with no certificate or CSP error in the devtools console. Then confirm the daemon really detached:

```bash
STATE="$(cargo run --quiet -- status --json | python3 -c 'import json,sys; print(json.load(sys.stdin)["state_dir"])')"
PID="$(python3 -c "import json; print(json.load(open('$STATE/server.json'))['pid'])")"
ps -o pid,ppid,stat -p "$PID"
cargo run --quiet -- stop
```

Expected: `PPID` is `1`. That is the number that proves the daemon detached rather than merely surviving the shell. Do not claim it detached without seeing it. This requires `status --json` to include `state_dir`; add it if it is not there.

- [ ] **Step 8: Verify the gate**

Run: `cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --all`
Expected: all clean.

- [ ] **Step 9: Commit**

```bash
git add src/server/daemon.rs src/commands/serve.rs src/client.rs src/cli.rs src/commands/mod.rs src/lib.rs tests/server_lifecycle.rs
git commit -m "feat(server): serve, stop, status, open, and the detached daemon"
```

---

### Task 9: Delivery cursors and the passive rules

**Why this task exists:** spec section 6.4 contains the one rule that is easy to get subtly wrong and impossible to notice in manual testing: there is **no separate passive buffer**, because a frame's contents are always computed from the cursor. Get that wrong and a passive event is delivered once by a poll and again by the next wake-up, so the agent replies twice to the same comment. The live-mode rate limit is a rate limit, not a count, and that distinction is also tested here.

**Files:**
- Create: `src/server/delivery.rs`, `tests/server_delivery.rs`
- Modify: `src/server/http.rs`, `src/server/mod.rs`

**Interfaces:**
- Consumes: `event::{Event, Frame, is_active}` (Task 2), `log::EventLog` (Task 3), `lease` (Task 7).
- Produces:
  - `pub fn frame_since(shared: &Shared, cursor: u64) -> Option<Frame>` — everything after the cursor up to and including the first active event
  - `pub fn ack(shared: &Shared, name: &str, seq: u64)` — persists the cursor as an event
  - `pub fn cursor_for(shared: &Shared, name: &str) -> u64`
  - `pub fn should_flush_passive(shared: &Shared, now: Instant) -> bool` — live mode only
  - `pub fn mark_flushed(shared: &Shared, now: Instant)`
  - `pub fn passive_since(shared: &Shared, cursor: u64) -> Vec<Event>` — what a `timeout` frame carries
  - The clock is injected by **passing `now` as a parameter**, not by a clock object. Tests hand it a fixed `Instant`; production hands it `Instant::now()`.
  - `pub const PASSIVE_MIN_GAP: Duration = Duration::from_secs(30);`
  - `pub const PASSIVE_MAX_AGE: Duration = Duration::from_secs(300);`

- [ ] **Step 1: Write the failing tests**

Create `tests/server_delivery.rs`:

```rust
mod support;
use artefacto::server::delivery;
use support::Harness;

#[test]
fn a_frame_stops_at_the_first_active_event() {
    let h = Harness::start();
    h.log_passive("thread.opened");   // seq 1
    h.log_passive("question.answered"); // seq 2
    h.log_active("chat.sent");        // seq 3
    h.log_active("review.submitted"); // seq 4

    let frame = delivery::frame_since(&h.shared, 0).expect("something to deliver");
    assert_eq!(frame.seq, 3, "the frame ends at the earliest active event");
    assert_eq!(frame.events.len(), 3, "and carries the passive events before it");
    assert_eq!(
        frame.events[2].r#type, "chat.sent",
        "the last event in a frame is the one that caused it"
    );
}

#[test]
fn a_passive_event_is_never_delivered_twice() {
    let h = Harness::start();
    h.log_passive("thread.opened");   // seq 1
    h.log_active("chat.sent");        // seq 2

    let first = delivery::frame_since(&h.shared, 0).unwrap();
    assert_eq!(first.events.len(), 2);
    delivery::ack(&h.shared, "claude", first.seq);

    h.log_active("chat.sent");        // seq 3
    let second = delivery::frame_since(&h.shared, delivery::cursor_for(&h.shared, "claude")).unwrap();
    assert_eq!(second.events.len(), 1, "the acked passive event must not ride along again");
    assert_eq!(second.events[0].seq, 3);
}

#[test]
fn passive_events_alone_produce_no_frame_in_digest_mode() {
    let h = Harness::start();
    h.log_passive("thread.opened");
    h.log_passive("element.reviewed");
    assert!(
        delivery::frame_since(&h.shared, 0).is_none(),
        "digest mode does not wake the agent for passive traffic"
    );
}

#[test]
fn the_cursor_survives_a_restart() {
    let h = Harness::start();
    h.log_active("chat.sent");
    delivery::ack(&h.shared, "claude", 1);
    let restarted = h.restart();
    assert_eq!(
        delivery::cursor_for(&restarted.shared, "claude"),
        1,
        "an agent that restarts with no memory resumes where it left off"
    );
}

#[test]
fn an_unacked_frame_is_delivered_again() {
    let h = Harness::start();
    h.log_active("chat.sent");
    let first = delivery::frame_since(&h.shared, delivery::cursor_for(&h.shared, "claude")).unwrap();
    // The agent dies here, before acking.
    let again = delivery::frame_since(&h.shared, delivery::cursor_for(&h.shared, "claude")).unwrap();
    assert_eq!(
        first.seq, again.seq,
        "at-least-once: a crash between receiving and acting replays the frame"
    );
}

#[test]
fn live_mode_sends_at_most_one_passive_frame_per_thirty_seconds() {
    let h = Harness::start_live();
    let t0 = std::time::Instant::now();
    h.log_passive("thread.opened");
    assert!(delivery::should_flush_passive(&h.shared, t0), "the first flush is allowed");
    delivery::mark_flushed(&h.shared, t0);

    h.log_passive("thread.replied");
    assert!(
        !delivery::should_flush_passive(&h.shared, t0 + std::time::Duration::from_secs(29)),
        "29 seconds later is still inside the rate limit"
    );
    assert!(
        delivery::should_flush_passive(&h.shared, t0 + std::time::Duration::from_secs(31)),
        "31 seconds later is allowed again"
    );
}

#[test]
fn live_mode_flushes_a_long_running_storm_at_the_age_cap() {
    let h = Harness::start_live();
    let t0 = std::time::Instant::now();
    let mut flushes = 0;
    // A thousand edits a minute must not produce a thousand frames.
    for i in 0..1000 {
        h.log_passive("thread.edited");
        let now = t0 + std::time::Duration::from_millis(i * 60);
        if delivery::should_flush_passive(&h.shared, now) {
            delivery::mark_flushed(&h.shared, now);
            flushes += 1;
        }
    }
    assert!(
        flushes <= 3,
        "60 seconds of storm at one frame per 30 seconds is at most 3 frames, got {flushes}"
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test server_delivery`
Expected: FAIL to compile, `could not find 'delivery' in 'server'`.

- [ ] **Step 3: Implement delivery**

Create `src/server/delivery.rs`. The rule that carries the whole design:

```rust
//! What an agent receives, and when.
//!
//! There is no passive buffer. A frame's contents are always computed from the
//! cursor, so the same passive event cannot be delivered once by a poll and
//! again by the next wake-up. Every change here must preserve that property.

use crate::server::event::{is_active, Event, Frame};
use crate::server::http::Shared;
use std::time::{Duration, Instant};

pub const PASSIVE_MIN_GAP: Duration = Duration::from_secs(30);
pub const PASSIVE_MAX_AGE: Duration = Duration::from_secs(300);

/// Everything after `cursor` up to and **including** the first active event.
/// `None` when nothing active is waiting: in digest mode passive traffic does
/// not wake the agent.
pub fn frame_since(shared: &Shared, cursor: u64) -> Option<Frame> {
    let core = shared.core.lock().unwrap();
    let pending: Vec<Event> = core.log.read_since(cursor).ok()?;
    let stop = pending.iter().position(|e| is_active(&e.r#type))?;
    Some(Frame::of(pending[..=stop].to_vec()))
}

/// The cursor is persisted as an event, because it is state and all state
/// folds from the log. A cursor kept only in memory would send an agent the
/// whole history again after a server restart.
pub fn ack(shared: &Shared, name: &str, seq: u64) {
    let mut core = shared.core.lock().unwrap();
    let _ = core.log.append(
        "-",
        0,
        crate::server::event::Actor::Server,
        "cursor.acked",
        serde_json::json!({ "agent": name, "acked_seq": seq }),
    );
    core.cursors.insert(name.to_string(), seq);
}

pub fn cursor_for(shared: &Shared, name: &str) -> u64 {
    shared.core.lock().unwrap().cursors.get(name).copied().unwrap_or(0)
}

/// Live mode only. A **rate limit, not a count**: an earlier draft flushed at
/// 100 buffered events, which bounds nothing, because a storm of a thousand
/// edits a minute would produce ten frames a minute rather than the two this
/// rule allows.
pub fn should_flush_passive(shared: &Shared, now: Instant) -> bool {
    let core = shared.core.lock().unwrap();
    if !core.passive_live {
        return false;
    }
    match core.last_passive_flush {
        None => true,
        Some(last) => now.duration_since(last) >= PASSIVE_MIN_GAP,
    }
}

pub fn mark_flushed(shared: &Shared, now: Instant) {
    shared.core.lock().unwrap().last_passive_flush = Some(now);
}
```

`Core` gains `pub cursors: HashMap<String, u64>`, `pub passive_live: bool`, and `pub last_passive_flush: Option<Instant>`. On start, the fold replays `cursor.acked` events into `cursors`, which is what makes the restart test pass.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --test server_delivery`
Expected: PASS, 7 tests.

- [ ] **Step 5: Verify the gate**

Run: `cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --all`
Expected: all clean.

- [ ] **Step 6: Commit**

```bash
git add src/server/delivery.rs src/server/http.rs src/server/mod.rs tests/server_delivery.rs
git commit -m "feat(server): cursor-driven delivery with digest and live passive rules"
```

---

### Task 10: `events` and `await`

**Why this task exists:** these are the two ways an agent hears anything, and they are the commands whose failure modes are invisible until an agent is actually driving a review. Spec section 5 is explicit that `await` exits 0 for every non-error outcome, because agents treat a non-zero exit as a failed tool call rather than "poll again", and that it reconnects on its own so a server restart mid-wait does not surface as an error.

**Files:**
- Create: `src/commands/agent.rs`, `tests/server_agent.rs`
- Modify: `src/cli.rs`, `src/commands/mod.rs`, `src/server/http.rs`

**Interfaces:**
- Consumes: `delivery::{frame_since, ack, cursor_for}` (Task 9), `lease::{acquire, validate, release, Mode}` (Task 7), `client::Client` (Task 8).
- Produces:
  - `pub fn events(args: &EventsArgs) -> anyhow::Result<()>`
  - `pub fn await_cmd(args: &AwaitArgs) -> anyhow::Result<()>`
  - `pub fn ack_cmd(args: &AckArgs) -> anyhow::Result<()>`
  - Server routes `GET /cli/events`, `GET /cli/await`, `POST /cli/ack`
  - Result shape: `{"ok":true,"status":"...","seq":N,"session":"...","events":[...]}`
  - `pub const DEFAULT_TIMEOUT_SECS: u64 = 90;`

**The five `await` statuses**, from spec section 5: `submitted`, `chat`, `idle`, `away`, `timeout`, `stopped`. Every one of them exits 0.

- [ ] **Step 1: Write the failing tests**

Create `tests/server_agent.rs`:

```rust
mod support;
use support::Harness;

#[test]
fn await_returns_on_a_chat_event_with_the_events_before_it() {
    let h = Harness::start();
    let session = h.take_lease("claude");
    h.log_passive("thread.opened");
    h.log_active("chat.sent");

    let result = h.await_now(&session, 5);
    assert_eq!(result["status"], "chat");
    assert_eq!(result["ok"], true);
    assert_eq!(result["seq"], 2);
    assert_eq!(result["events"].as_array().unwrap().len(), 2, "the passive event rides along");
}

#[test]
fn await_returns_timeout_with_status_zero_when_nothing_happens() {
    let h = Harness::start();
    let session = h.take_lease("claude");
    let result = h.await_now(&session, 1);
    assert_eq!(result["status"], "timeout");
    assert_eq!(result["ok"], true, "a timeout is not a failure; the agent polls again");
}

#[test]
fn await_returns_at_the_earliest_active_event_not_the_latest() {
    let h = Harness::start();
    let session = h.take_lease("claude");
    h.log_active("chat.sent");        // seq 1
    h.log_active("review.submitted"); // seq 2

    let result = h.await_now(&session, 5);
    assert_eq!(result["status"], "chat", "events are handled in the order they happened");
    assert_eq!(result["seq"], 1);
}

#[test]
fn await_returns_submitted_and_names_the_feedback_path() {
    let h = Harness::start();
    let session = h.take_lease("claude");
    h.log_submitted("plan:x");

    let result = h.await_now(&session, 5);
    assert_eq!(result["status"], "submitted");
    assert!(result["events"][0]["data"]["path"].is_string(), "the agent needs the file path");
}

#[test]
fn await_without_a_server_exits_4() {
    let repo = support::git_repo();
    support::artefacto(repo.path())
        .args(["await", "--timeout", "1s"])
        .assert()
        .code(4);
}

#[test]
fn a_mutation_with_a_superseded_token_exits_6() {
    let h = Harness::start();
    let first = h.take_lease("claude");
    let _second = h.take_lease_with_takeover("codex");

    let out = h.run_cli(&["ack", "--seq", "1", "--session", &first]);
    assert_eq!(out.code, 6, "a stale agent that lost the lease must not be able to write");
}

#[test]
fn a_second_agent_without_takeover_exits_6_and_names_the_holder() {
    let h = Harness::start();
    let _first = h.take_lease("claude");
    let out = h.run_cli(&["await", "--agent", "codex", "--timeout", "1s"]);
    assert_eq!(out.code, 6);
    assert!(out.stderr.contains("claude"), "the refusal names the holder and its age");
}

#[test]
fn calling_await_again_acknowledges_the_previous_frame() {
    let h = Harness::start();
    let session = h.take_lease("claude");
    h.log_active("chat.sent");
    let first = h.await_now(&session, 5);
    assert_eq!(first["seq"], 1);

    h.log_active("chat.sent");
    let second = h.await_now(&session, 5);
    assert_eq!(second["seq"], 2);
    assert_eq!(
        second["events"].as_array().unwrap().len(),
        1,
        "the previous call's frame was acknowledged by this call"
    );
}

#[test]
fn events_since_prints_the_backlog_as_ndjson_and_exits() {
    let h = Harness::start();
    h.log_active("chat.sent");
    h.log_active("chat.sent");
    let out = h.run_cli(&["events", "--since", "0"]);
    assert_eq!(out.code, 0);
    let lines: Vec<_> = out.stdout.lines().filter(|l| !l.is_empty()).collect();
    assert!(!lines.is_empty(), "the backlog prints one frame per line");
    for line in lines {
        let v: serde_json::Value = serde_json::from_str(line).expect("each line is one JSON frame");
        assert_eq!(v["format"], "artefacto.frame/1");
    }
}

#[test]
fn await_survives_a_server_restart_mid_wait() {
    let h = Harness::start();
    let session = h.take_lease("claude");
    let waiting = h.await_in_background(&session, 20);

    h.restart_server_in_place();
    h.log_active("chat.sent");

    let result = waiting.join();
    assert_eq!(
        result["status"], "chat",
        "await reconnects against the same cursor rather than surfacing the restart"
    );
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test server_agent`
Expected: FAIL, `unrecognized subcommand 'await'`.

- [ ] **Step 3: Add the CLI types**

```rust
    /// Wait for something the agent should act on.
    Await(AwaitArgs),
    /// Stream or replay the event log.
    Events(EventsArgs),
    /// Acknowledge events up to a sequence number.
    Ack(AckArgs),
```

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
    #[arg(long)]
    pub takeover: bool,
}

#[derive(Args, Debug)]
pub struct EventsArgs {
    #[arg(long)]
    pub since: Option<u64>,
    #[arg(long)]
    pub follow: bool,
    #[arg(long, default_value = "agent")]
    pub agent: String,
    #[arg(long)]
    pub takeover: bool,
}

#[derive(Args, Debug)]
pub struct AckArgs {
    #[arg(long)]
    pub seq: u64,
    #[arg(long)]
    pub session: String,
}
```

- [ ] **Step 4: Implement the long poll**

The server route holds the request until something is deliverable or the deadline passes. Blocking is the point: the thread is the wait.

```rust
/// `GET /cli/await?timeout=90&since=N`. Holds the request open. One thread is
/// blocked here for the duration, which is exactly what a thread-per-request
/// server can afford and an async one would need a task for.
fn await_route(shared: Arc<Shared>, request: Request, q: AwaitQuery) -> Response<Cursor<Vec<u8>>> {
    let deadline = Instant::now() + q.timeout;
    let cursor = q.since.unwrap_or_else(|| delivery::cursor_for(&shared, &q.agent));

    loop {
        if let Some(frame) = delivery::frame_since(&shared, cursor) {
            let status = status_for(&frame);
            return json_response(200, &result_json(status, &frame, &q.session));
        }
        if shared.stopping() {
            return json_response(200, &stopped_json(cursor, &q.session));
        }
        if Instant::now() >= deadline {
            // A timeout still carries whatever passive events accumulated, so
            // the agent's terminal is not silent about a busy review.
            let passive = delivery::passive_since(&shared, cursor);
            return json_response(200, &timeout_json(cursor, passive, &q.session));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The status is named by the event that ended the frame, which is always the
/// last one in it.
fn status_for(frame: &Frame) -> &'static str {
    match frame.events.last().map(|e| e.r#type.as_str()) {
        Some("review.submitted") => "submitted",
        Some("chat.sent") => "chat",
        Some("reviewer.idle") => "idle",
        Some("reviewer.away") => "away",
        Some("server.stopping") => "stopped",
        _ => "timeout",
    }
}
```

Client side, the two behaviours the spec names:

- **`await` reconnects on its own.** A connection error or a refused connection before the absolute deadline is retried against the same cursor after 250 ms, not surfaced. Only the absolute deadline ends the wait, and it ends it as `timeout`. Without this the restart-mid-wait test cannot pass.
- **Calling `await` or `events` again acknowledges the previous frame.** The client sends the previous result's `seq` as its cursor, and the server persists it. `ack --seq N` exists for an agent that wants to acknowledge only part of a frame.

Polling at 50 ms rather than a condition variable is a deliberate simplification: the wait is bounded by a deadline anyway, one reviewer generates events at human speed, and a condvar here would add a second synchronization primitive for no measurable gain. Revisit only if a profile shows it matters.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --test server_agent`
Expected: PASS, 10 tests.

- [ ] **Step 6: Verify the gate**

Run: `cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --all`
Expected: all clean.

- [ ] **Step 7: Commit**

```bash
git add src/commands/agent.rs src/cli.rs src/commands/mod.rs src/server/http.rs tests/server_agent.rs
git commit -m "feat(agent): events and await over the lease cursor"
```

---

### Task 11: `plan push`

**Why this task exists:** push is how a plan becomes a reviewable artifact, and it carries the one concurrency check in the whole system. Spec section 5 is emphatic that `--base-revision` is supplied by the caller, taken from the revision the agent last saw, because reading the current revision at push time would make the check vacuous — it would always match. Section 4.2 requires the revision event to carry the whole validated plan, so the log alone can rebuild the page after a restart.

**Files:**
- Modify: `src/cli.rs`, `src/commands/plan.rs`, `src/server/http.rs`
- Create: `tests/server_push.rs`

**Interfaces:**
- Consumes: `plan::model` (already shipped), `client::Client` (Task 8), `log::EventLog` (Task 3).
- Produces:
  - `pub fn push(args: &PushArgs) -> anyhow::Result<()>`
  - Server route `POST /cli/push`
  - Exit code **7** when `base_revision` is behind the server

- [ ] **Step 1: Write the failing tests**

Create `tests/server_push.rs`:

```rust
mod support;
use support::Harness;

#[test]
fn the_first_push_needs_neither_flag() {
    let h = Harness::start();
    let out = h.push(&["--session", &h.take_lease("claude")]);
    assert_eq!(out.code, 0);
    let v: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(v["revision"], 1);
    assert!(v["artifact"].is_string());
    assert!(v["url"].as_str().unwrap().contains("/b/"), "push returns a bootstrap URL");
    assert!(v["plan_hash"].as_str().unwrap().starts_with("sha256:"));
}

#[test]
fn a_later_push_must_pass_base_revision_or_force() {
    let h = Harness::start();
    let session = h.take_lease("claude");
    h.push(&["--session", &session]);
    let out = h.push(&["--session", &session]);
    assert_eq!(out.code, 2, "a later push without either flag is a usage error");
    assert!(out.stderr.contains("--base-revision"));
}

#[test]
fn a_stale_base_revision_is_refused_with_exit_7() {
    let h = Harness::start();
    let session = h.take_lease("claude");
    h.push(&["--session", &session]);                      // revision 1
    h.push(&["--session", &session, "--base-revision", "1"]); // revision 2

    let out = h.push(&["--session", &session, "--base-revision", "1"]);
    assert_eq!(out.code, 7, "the server is ahead of what this agent last saw");
    assert!(out.stderr.contains("status --json"), "the message says how to catch up");
}

#[test]
fn force_overrides_a_stale_base_revision() {
    let h = Harness::start();
    let session = h.take_lease("claude");
    h.push(&["--session", &session]);
    h.push(&["--session", &session, "--base-revision", "1"]);
    let out = h.push(&["--session", &session, "--force"]);
    assert_eq!(out.code, 0);
}

#[test]
fn an_invalid_plan_is_refused_and_nothing_is_logged() {
    let h = Harness::start();
    let session = h.take_lease("claude");
    let before = h.last_seq();
    let out = h.push_file("invalid-cycle.json", &["--session", &session]);
    assert_eq!(out.code, 1);
    assert_eq!(h.last_seq(), before, "a rejected push must not append to the log");
}

#[test]
fn a_revision_event_carries_the_whole_plan_so_the_log_can_rebuild_it() {
    let h = Harness::start();
    let session = h.take_lease("claude");
    h.push(&["--session", &session]);

    let event = h.last_event_of_type("revision.published");
    assert!(event["data"]["plan"]["phases"].is_array(), "not just a change summary");
    assert!(event["data"]["plan_hash"].as_str().unwrap().starts_with("sha256:"));
}

#[test]
fn the_page_body_survives_a_restart_from_the_log_alone() {
    let h = Harness::start();
    let session = h.take_lease("claude");
    h.push(&["--session", &session]);
    let before = h.rendered_body("plan:auth-refactor");

    let restarted = h.restart();
    let after = restarted.rendered_body("plan:auth-refactor");
    assert_eq!(before, after, "the log alone must rebuild the page");
}

#[test]
fn resolutions_address_many_threads_in_one_push() {
    let h = Harness::start();
    let session = h.take_lease("claude");
    h.push(&["--session", &session]);
    let t1 = h.open_thread("task:t-a");
    let t2 = h.open_thread("task:t-b");

    let file = h.write_resolutions(&[(&t1, "changed", "fixed"), (&t2, "declined", "out of scope")]);
    let out = h.push(&[
        "--session", &session,
        "--base-revision", "1",
        "--resolutions", file.to_str().unwrap(),
    ]);
    assert_eq!(out.code, 0);
    assert_eq!(h.thread_status(&t1), "changed");
    assert_eq!(h.thread_status(&t2), "declined");
}

#[test]
fn a_push_with_a_superseded_session_exits_6() {
    let h = Harness::start();
    let stale = h.take_lease("claude");
    let _fresh = h.take_lease_with_takeover("codex");
    let out = h.push(&["--session", &stale]);
    assert_eq!(out.code, 6);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test server_push`
Expected: FAIL, `unrecognized argument '--session'`.

- [ ] **Step 3: Add the CLI arguments**

Add to `PlanAction`:

```rust
    /// Publish a plan to the review server.
    Push {
        file: PathBuf,
        #[arg(long)]
        session: String,
        /// The revision this agent last saw. Required after the first push
        /// unless --force is given.
        #[arg(long)]
        base_revision: Option<u32>,
        #[arg(long)]
        force: bool,
        /// A JSON file of {thread, status, note} entries.
        #[arg(long)]
        resolutions: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
```

- [ ] **Step 4: Implement push**

Order matters, and each step exists to avoid a specific failure:

1. **Validate locally first.** An invalid plan never reaches the server, so a bad push cannot append to the log. The test above asserts `last_seq` is unchanged.
2. **Start the server if none is running.** Push is the command an agent runs first, so requiring a separate `serve` would be a papercut on every session.
3. **Compare and append atomically.** The server holds the lock across the `base_revision` comparison and the append. Comparing and then appending in two steps would let two agents both pass the check.
4. **Append `revision.published` carrying the whole validated plan and its hash.** Plans are large, so later events reference the plan by hash rather than repeating it.
5. **Apply `--resolutions` in the same append batch**, so a reviewer never sees a new body beside unresolved threads from the old one.
6. **Open the browser on the first push only.** A push per task would otherwise open a tab per task.

The comparison itself:

```rust
/// `base_revision` comes from the caller, never from the server. Reading the
/// current revision here instead would compare the server to itself and always
/// pass, which is the bug this check exists to prevent.
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

`PushError::Stale` maps to exit 7 and prints: `the server is at revision {current}, you pushed against {seen}; re-read with 'artefacto status --json' or pass --force`.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --test server_push`
Expected: PASS, 9 tests.

- [ ] **Step 6: Verify the gate**

Run: `cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --all`
Expected: all clean.

- [ ] **Step 7: Commit**

```bash
git add src/cli.rs src/commands/plan.rs src/server/http.rs tests/server_push.rs
git commit -m "feat(plan): push a revision with a compare-and-append base check"
```

---

### Task 12: `reply`, `resolve`, and the end-to-end loop

**Why this task exists:** these are the last two agent verbs, and finishing them closes the loop this whole plan exists to serve: a reviewer comments, the agent hears it, the agent answers, and the reviewer sees the answer. Until one test drives that whole path, every earlier task is only unit-tested.

**Files:**
- Modify: `src/cli.rs`, `src/commands/agent.rs`, `src/server/http.rs`
- Create: `tests/server_loop.rs`

**Interfaces:**
- Consumes: everything above.
- Produces:
  - `pub fn reply(args: &ReplyArgs) -> anyhow::Result<()>`
  - `pub fn resolve(args: &ResolveArgs) -> anyhow::Result<()>`
  - Server routes `POST /cli/reply`, `POST /cli/resolve`

- [ ] **Step 1: Write the failing tests**

Create `tests/server_loop.rs`:

```rust
mod support;
use support::Harness;

#[test]
fn a_reply_reaches_the_page_as_a_frame() {
    let h = Harness::start();
    let session = h.take_lease("claude");
    h.push(&["--session", &session]);
    let mut page = h.connect_page();
    let thread = h.open_thread("task:t-a");

    h.run_cli(&["reply", "--session", &session, "--thread", &thread, "on it"]);

    let frame = page.next_frame();
    let event = &frame["events"][0];
    assert_eq!(event["type"], "thread.replied");
    assert_eq!(event["actor"], "agent");
    assert_eq!(event["data"]["text"], "on it");
}

#[test]
fn reply_may_omit_the_artifact_when_there_is_only_one() {
    let h = Harness::start();
    let session = h.take_lease("claude");
    h.push(&["--session", &session]);
    let out = h.run_cli(&["reply", "--session", &session, "a page-level note"]);
    assert_eq!(out.code, 0, "one artifact means --artifact is not needed");
}

#[test]
fn resolve_records_the_status_and_the_note() {
    let h = Harness::start();
    let session = h.take_lease("claude");
    h.push(&["--session", &session]);
    let thread = h.open_thread("task:t-a");

    h.run_cli(&["resolve", &thread, "--session", &session, "--declined", "--note", "out of scope"]);

    let event = h.last_event_of_type("thread.resolved");
    assert_eq!(event["data"]["status"], "declined");
    assert_eq!(event["data"]["note"], "out of scope");
    assert_eq!(h.thread_status(&thread), "declined");
}

#[test]
fn resolve_requires_exactly_one_of_changed_or_declined() {
    let h = Harness::start();
    let session = h.take_lease("claude");
    h.push(&["--session", &session]);
    let thread = h.open_thread("task:t-a");
    let out = h.run_cli(&["resolve", &thread, "--session", &session]);
    assert_eq!(out.code, 2, "a resolution with no verdict is a usage error");
}

#[test]
fn comment_text_is_never_rendered_as_markup() {
    let h = Harness::start();
    let session = h.take_lease("claude");
    h.push(&["--session", &session]);
    let thread = h.open_thread_with_text("task:t-a", "<img src=x onerror=alert(1)>");

    let body = h.rendered_body("plan:auth-refactor");
    assert!(
        !body.contains("<img src=x onerror"),
        "reviewer text is rendered as text, never as markup (spec section 8)"
    );
}

#[test]
fn the_whole_loop_runs_once_through() {
    let h = Harness::start();
    let session = h.take_lease("claude");

    // 1. the agent publishes
    let pushed = h.push_json(&["--session", &session]);
    assert_eq!(pushed["revision"], 1);

    // 2. the reviewer opens the page and asks a question
    let mut page = h.connect_page();
    let thread = h.open_thread("task:t-a");
    h.page_chat(&mut page, &thread, "why a trait here?");

    // 3. the agent hears it
    let heard = h.await_now(&session, 5);
    assert_eq!(heard["status"], "chat");
    assert_eq!(heard["events"].as_array().unwrap().last().unwrap()["data"]["text"],
               "why a trait here?");

    // 4. the agent answers, and the reviewer sees it
    h.run_cli(&["reply", "--session", &session, "--thread", &thread, "so Redis can slot in"]);
    let frame = page.next_frame();
    assert_eq!(frame["events"][0]["data"]["text"], "so Redis can slot in");

    // 5. the reviewer submits, and the agent is told where the file landed
    h.page_submit(&mut page, "approve");
    let submitted = h.await_now(&session, 5);
    assert_eq!(submitted["status"], "submitted");
    let path = submitted["events"].as_array().unwrap().last().unwrap()["data"]["path"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(std::path::Path::new(&path).exists(), "the feedback document is written to disk");

    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(doc["format"], "artefacto.feedback/1");
    assert_eq!(doc["verdict"], "approve");
    assert_eq!(doc["base_revision"], 1);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test server_loop`
Expected: FAIL, `unrecognized subcommand 'reply'`.

- [ ] **Step 3: Add the CLI types**

```rust
    /// Reply to a thread, or post a page-level message.
    Reply(ReplyArgs),
    /// Mark a thread addressed or declined.
    Resolve(ResolveArgs),
```

```rust
#[derive(Args, Debug)]
pub struct ReplyArgs {
    #[arg(long)]
    pub session: String,
    #[arg(long, conflicts_with = "artifact")]
    pub thread: Option<String>,
    #[arg(long)]
    pub artifact: Option<String>,
    /// The reply text. Omit and pass --stdin to read it from standard input.
    pub text: Option<String>,
    #[arg(long)]
    pub stdin: bool,
}

#[derive(Args, Debug)]
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
```

Mark the group required so clap produces the exit-2 usage error the test expects:

```rust
#[command(group = clap::ArgGroup::new("verdict").required(true))]
```

- [ ] **Step 4: Implement the two verbs**

Both follow the same four steps, and both must be safe to run twice, because delivery is at-least-once:

1. Validate the session token. A superseded generation is exit 6.
2. Append the event, with `actor: "agent"` and the lease name in `data.agent`.
3. Broadcast the resulting frame to every connected page.
4. Print the new `seq` as JSON when `--json` is given.

`reply` resolves its target this way: `--thread` names a thread; otherwise `--artifact` makes it page-level chat; otherwise, when the server has exactly one artifact, that one is used. Zero or several artifacts with neither flag is a usage error naming the artifacts it found.

**Text is data, not markup.** Reply, chat, answer, and comment text is escaped on render. Only plan markdown goes through the sanitizer. The test above pins this, and it is the one place where getting it wrong is a security bug rather than a display bug.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --test server_loop`
Expected: PASS, 6 tests.

- [ ] **Step 6: Verify the whole suite**

Run: `cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --all`
Expected: all clean, including every test from plan 1.

- [ ] **Step 7: Commit**

```bash
git add src/cli.rs src/commands/agent.rs src/server/http.rs tests/server_loop.rs
git commit -m "feat(agent): reply and resolve, closing the review loop"
```

---

### Task 13: Presence, the nudge timers, and the feedback document

**Why this task exists:** the self-review of this plan found three spec requirements with no task behind them. Spec section 6.3 needs `agent.attached`, `agent.detached`, and `nudge`; section 6.2 needs `reviewer.idle`, `reviewer.away`, and `reviewer.back`; section 6.7 requires the feedback document to be written to a repo-local file on submit, so the file-based loop keeps working. Without this task the server would deliver events nobody emits, and `await` could return `idle` or `away` forever without either ever firing.

**Files:**
- Create: `src/server/presence.rs`, `tests/server_presence.rs`
- Modify: `src/server/http.rs`, `src/server/lease.rs`, `src/server/socket.rs`, `src/commands/agent.rs`

**Interfaces:**
- Consumes: `lease::{acquire, release, Mode}` (Task 7), `delivery` (Task 9), `socket::page_count` (Task 6).
- Produces:
  - `pub fn on_page_ping(shared: &Shared, now: Instant)` — throttled activity mark
  - `pub fn tick(shared: &Shared, now: Instant)` — called from the accept loop's idle branch
  - `pub fn write_feedback(shared: &Shared, artifact: &str, doc: &serde_json::Value) -> anyhow::Result<PathBuf>`
  - `pub const PING_THROTTLE: Duration = Duration::from_secs(30);`
- Adds to `Core`: `pub last_ping: Option<Instant>`, `pub idle_fired: bool`, `pub away_fired: bool`, `pub all_closed_at: Option<Instant>`, `pub submitted: HashSet<String>`

- [ ] **Step 1: Write the failing tests**

Create `tests/server_presence.rs`:

```rust
mod support;
use artefacto::server::presence;
use support::Harness;

#[test]
fn taking_the_lease_announces_the_agent_and_its_mode() {
    let h = Harness::start();
    let _session = h.take_lease("claude");
    let event = h.last_event_of_type("agent.attached");
    assert_eq!(event["data"]["agent"], "claude");
    assert_eq!(event["data"]["mode"], "waiting", "await holds the lease as waiting");
}

#[test]
fn releasing_the_lease_announces_the_detach() {
    let h = Harness::start();
    let session = h.take_lease("claude");
    h.release_lease(&session);
    assert_eq!(h.last_event_of_type("agent.detached")["data"]["agent"], "claude");
}

#[test]
fn idle_fires_once_per_quiet_period_and_rearms_after_activity() {
    let h = Harness::start_with_idle(std::time::Duration::from_secs(900));
    let mut page = h.connect_page();
    let t0 = std::time::Instant::now();
    presence::on_page_ping(&h.shared, t0);

    presence::tick(&h.shared, t0 + std::time::Duration::from_secs(901));
    assert_eq!(h.count_events("reviewer.idle"), 1);

    // Still quiet: it must not fire again.
    presence::tick(&h.shared, t0 + std::time::Duration::from_secs(1200));
    assert_eq!(h.count_events("reviewer.idle"), 1, "once per quiet period, not once per tick");

    // The reviewer comes back, then goes quiet again.
    presence::on_page_ping(&h.shared, t0 + std::time::Duration::from_secs(1300));
    presence::tick(&h.shared, t0 + std::time::Duration::from_secs(2300));
    assert_eq!(h.count_events("reviewer.idle"), 2, "activity re-arms it");
    drop(page);
}

#[test]
fn a_reviewer_who_reads_for_twenty_minutes_is_not_idle() {
    let h = Harness::start_with_idle(std::time::Duration::from_secs(900));
    let _page = h.connect_page();
    let t0 = std::time::Instant::now();
    // Scroll and pointer activity, throttled to one ping per 30 seconds.
    for minute in 0..20 {
        presence::on_page_ping(&h.shared, t0 + std::time::Duration::from_secs(minute * 60));
        presence::tick(&h.shared, t0 + std::time::Duration::from_secs(minute * 60 + 30));
    }
    assert_eq!(
        h.count_events("reviewer.idle"),
        0,
        "idle is measured from activity, not from the last comment"
    );
}

#[test]
fn a_ping_storm_is_throttled_to_one_mark_per_thirty_seconds() {
    let h = Harness::start();
    let t0 = std::time::Instant::now();
    presence::on_page_ping(&h.shared, t0);
    let first = h.last_activity();
    presence::on_page_ping(&h.shared, t0 + std::time::Duration::from_secs(5));
    assert_eq!(h.last_activity(), first, "a ping inside the throttle window is ignored");
    presence::on_page_ping(&h.shared, t0 + std::time::Duration::from_secs(31));
    assert_ne!(h.last_activity(), first);
}

#[test]
fn away_fires_once_when_every_page_closes_and_back_fires_on_return() {
    let h = Harness::start_with_away(std::time::Duration::from_secs(300));
    let page = h.connect_page();
    let t0 = std::time::Instant::now();
    drop(page);
    h.wait_for(|| h.page_count() == 0, "the page should be gone");

    presence::tick(&h.shared, t0 + std::time::Duration::from_secs(301));
    assert_eq!(h.count_events("reviewer.away"), 1);
    presence::tick(&h.shared, t0 + std::time::Duration::from_secs(600));
    assert_eq!(h.count_events("reviewer.away"), 1, "fires once, not every tick");

    let _returned = h.connect_page();
    h.wait_for(|| h.count_events("reviewer.back") == 1, "reconnecting should fire back");
}

#[test]
fn away_does_not_fire_once_the_review_is_submitted() {
    let h = Harness::start_with_away(std::time::Duration::from_secs(300));
    let session = h.take_lease("claude");
    h.push(&["--session", &session]);
    let mut page = h.connect_page();
    h.page_submit(&mut page, "approve");
    let t0 = std::time::Instant::now();
    drop(page);
    h.wait_for(|| h.page_count() == 0, "the page should be gone");

    presence::tick(&h.shared, t0 + std::time::Duration::from_secs(301));
    assert_eq!(
        h.count_events("reviewer.away"),
        0,
        "a finished review is not an abandoned one"
    );
}

#[test]
fn stopping_the_server_wakes_a_waiting_agent() {
    let h = Harness::start();
    let session = h.take_lease("claude");
    let waiting = h.await_in_background(&session, 20);
    h.request_stop();
    let result = waiting.join();
    assert_eq!(result["status"], "stopped");
    assert_eq!(result["ok"], true, "a shutdown is not a failed tool call");
}

#[test]
fn submitting_writes_the_feedback_document_beside_the_plan() {
    let h = Harness::start();
    let session = h.take_lease("claude");
    h.push(&["--session", &session]);
    let mut page = h.connect_page();
    h.page_submit(&mut page, "request_changes");

    let path = h.plan_path().with_file_name(format!(
        "{}-feedback.json",
        h.plan_path().file_stem().unwrap().to_string_lossy()
    ));
    assert!(path.exists(), "the file-based loop keeps working without an agent attached");
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(doc["format"], "artefacto.feedback/1");
    assert_eq!(doc["verdict"], "request_changes");
    assert_eq!(doc["base_revision"], 1);
    assert!(doc["comments"].is_array());
    assert!(doc["answers"].is_array());
    assert!(doc["reviewed"].is_array());
}

#[test]
fn a_nudge_reaches_the_page() {
    let h = Harness::start();
    let session = h.take_lease("claude");
    h.push(&["--session", &session]);
    let mut page = h.connect_page();
    h.run_cli(&["reply", "--session", &session, "--nudge", "have a look at phase 2"]);
    let frame = page.next_frame();
    assert_eq!(frame["events"][0]["type"], "nudge");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test server_presence`
Expected: FAIL to compile, `could not find 'presence' in 'server'`.

- [ ] **Step 3: Implement presence and the timers**

Create `src/server/presence.rs`:

```rust
//! When the reviewer stopped doing anything, and when they left.
//!
//! Activity is measured from a throttled page ping on scroll, keys, pointer,
//! and visibility — not from the last comment. A reviewer who reads carefully
//! for twenty minutes without typing is not idle, and nudging them would be
//! exactly wrong.

use crate::server::http::Shared;
use std::time::{Duration, Instant};

pub const PING_THROTTLE: Duration = Duration::from_secs(30);

/// At most one mark per 30 seconds. The page throttles too; this is the
/// server-side half, because a page is not a trusted rate limiter.
pub fn on_page_ping(shared: &Shared, now: Instant) {
    let mut core = shared.core.lock().unwrap();
    if let Some(last) = core.last_ping {
        if now.duration_since(last) < PING_THROTTLE {
            return;
        }
    }
    core.last_ping = Some(now);
    core.last_activity = now;
    core.idle_fired = false; // activity re-arms the nudge
}

/// Called from the accept loop's idle branch, four times a second. Each rule
/// fires **once** per quiet period; `idle_fired` and `away_fired` are what
/// make that true, and both reset when the condition that set them clears.
pub fn tick(shared: &Shared, now: Instant) {
    let (idle_after, away_after) = {
        let core = shared.core.lock().unwrap();
        (core.idle_after, core.away_after)
    };

    // Idle: a page is open, and nothing has happened for --idle.
    if crate::server::socket::page_count(shared) > 0 {
        let mut core = shared.core.lock().unwrap();
        let quiet = now.saturating_duration_since(core.last_activity);
        if !core.idle_fired && quiet >= idle_after {
            core.idle_fired = true;
            let _ = core.log.append(
                "-", 0, crate::server::event::Actor::Reviewer, "reviewer.idle",
                serde_json::json!({ "quiet_secs": quiet.as_secs() }),
            );
        }
    }

    // Away: every socket has been shut for --away and the review is unsubmitted.
    let mut core = shared.core.lock().unwrap();
    match core.all_closed_at {
        Some(since) if !core.away_fired && now.saturating_duration_since(since) >= away_after => {
            if !core.everything_submitted() {
                core.away_fired = true;
                let _ = core.log.append(
                    "-", 0, crate::server::event::Actor::Reviewer, "reviewer.away",
                    serde_json::json!({ "away_secs": now.saturating_duration_since(since).as_secs() }),
                );
            }
        }
        _ => {}
    }
}
```

`socket::handle_upgrade` sets `all_closed_at = None` and, if `away_fired` was true, appends `reviewer.back` and clears the flag. When the last socket goes, it sets `all_closed_at = Some(Instant::now())`.

`lease::acquire` appends `agent.attached` with `{agent, mode}`; `lease::release` and expiry collection append `agent.detached`. Putting them in the lease rather than in the commands is what makes presence derive from the lease, so the page's pill does not flicker between poll cycles.

- [ ] **Step 4: Write the feedback document on submit**

```rust
/// Also written to a repo-local file, so the file-based loop keeps working
/// when no agent is attached. Default: beside the pushed plan as
/// `<stem>-feedback.json`, overridable per push.
pub fn write_feedback(shared: &Shared, artifact: &str, doc: &serde_json::Value) -> Result<PathBuf> {
    let source = {
        let core = shared.core.lock().unwrap();
        core.source_path(artifact).context("no source path recorded for this artifact")?
    };
    let stem = source.file_stem().unwrap_or_default().to_string_lossy().to_string();
    let path = source.with_file_name(format!("{stem}-feedback.json"));
    std::fs::write(&path, serde_json::to_string_pretty(doc)?)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}
```

The `review.submitted` handler appends the event carrying the whole document, calls `write_feedback`, and puts the resulting path in the event's `data.path`, which is what Task 10's `submitted` status hands the agent.

- [ ] **Step 5: Wire the tick into the accept loop**

In `http::run`'s `Ok(None)` branch, before the self-exit check:

```rust
                crate::server::presence::tick(&shared, Instant::now());
```

The idle branch already runs four times a second, so no extra thread is needed.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test --test server_presence`
Expected: PASS, 10 tests.

- [ ] **Step 7: Verify the gate**

Run: `cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test --all`
Expected: all clean.

- [ ] **Step 8: Commit**

```bash
git add src/server/presence.rs src/server/http.rs src/server/lease.rs src/server/socket.rs src/commands/agent.rs tests/server_presence.rs
git commit -m "feat(server): presence, idle and away nudges, and the feedback document"
```

---

## What this plan deliberately leaves out

- **The page rewrite.** Plan 3. This plan serves the existing static render with a nonce stamped on its inline tags, and tests every socket path against a fake page client. The re-entrant mount, the server-side store, the in-place revision swap, threads, and chat in the browser are all plan 3.
- **The artifact index, `list`, and posters.** Plan 4. `open` here requires `--artifact`; the no-argument form that opens the index arrives with the index.
- **`skill --print` and `--install`, and cargo-dist releases.** Plan 5.
- **The loadout dispatcher.** Plan 6. Nothing in this plan edits the rosita repository.
- **The file-based fallback transport** from spec section 13. Three spikes cleared the loopback path on the primary target before this plan was written, so building a second transport now would be speculative. It stays a documented contingency, and the spike results are recorded above so a future reader knows what was tested and where.
- **An agent-role WebSocket.** Spec section 6.5 defers it: it would save one process and cost a second auth path with a token in the transcript.
- **`clean`.** It truncates the log and rotates the secret, and it is easier to write once the index exists to be preserved across it. Plan 4.

**What this leaves uncovered, stated plainly:** this plan has no browser-level test, for the same reason plan 1 had none. Every socket path is driven by a fake page client, which proves the server's half of the protocol and nothing about the reviewer's. The real page arrives in plan 3 and brings the headless-Chromium smoke with it. Until then, "the server delivers a frame" is verified and "the reviewer sees it" is not.

## Verification for the whole plan

When all thirteen tasks are done:

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --all
```

Then drive the loop by hand once, because a fake page client cannot tell you the page is usable:

```bash
cargo run --quiet -- plan push tests/fixtures/plan/kitchen-sink.json --session "$(cargo run --quiet -- await --timeout 1s | python3 -c 'import json,sys; print(json.load(sys.stdin)["session"])')"
cargo run --quiet -- status --json
```

Open the URL push printed. Confirm in the browser's devtools console that there are no CSP violations, that the WebSocket connects, and that the page reconnects after `artefacto stop` followed by `artefacto serve`. Then check the daemon really detached:

```bash
ps -o pid,ppid,stat -p "$(python3 -c "import json,os;print(json.load(open(os.path.expanduser('~/.local/state/artefacto/<repo-hash>/server.json')))['pid'])")"
```

Expected: `PPID` is `1`. Report what you saw. Do not claim the loop works without opening the page.

---

## Appendix A: the test harness

Every server test drives the real binary or the real `Shared` state through the helpers below. They live in `tests/support/mod.rs`, and each is added by the task that first uses it — the "added by" column says which. An implementer working Task 9 does not need to write Task 12's helpers.

None of them touch the developer's real state directory: `Harness::start` creates a `tempfile::TempDir` and every spawned CLI gets `XDG_STATE_HOME` pointing inside it.

**Construction and lifecycle**

| helper | signature | contract | added by |
|---|---|---|---|
| `Harness::start` | `fn start() -> Harness` | Server on a random port over a temp state dir, digest mode, idle and away effectively disabled (one hour). | 4 |
| `Harness::start_live` | `fn start_live() -> Harness` | As `start`, with `passive_live = true`. | 9 |
| `Harness::start_with_idle` | `fn start_with_idle(after: Duration) -> Harness` | As `start`, with `idle_after` set. | 13 |
| `Harness::start_with_away` | `fn start_with_away(after: Duration) -> Harness` | As `start`, with `away_after` set. | 13 |
| `restart` | `fn restart(self) -> Harness` | Drops the server, then starts a new one **on the same state directory**. This is the log-sufficiency check: everything the new server knows, it read from the log. | 9 |
| `restart_server_in_place` | `fn restart_server_in_place(&self)` | Stops and restarts the listener on the same port without dropping the `Harness`, so a client blocked in `await` sees a real restart. | 10 |
| `request_stop` | `fn request_stop(&self)` | Sets the stopping flag and appends `server.stopping`, the way `artefacto stop` does. | 13 |

**Raw HTTP**

| helper | signature | contract | added by |
|---|---|---|---|
| `raw` | `fn raw(&self, request: &str) -> String` | Sends bytes verbatim. Needed because a polite client normalizes exactly the `Host` values these tests attack with. | 4 |
| `get` | `fn get(&self, path: &str, headers: &[(&str, &str)]) -> String` | A well-formed GET with the correct `Host` already set. | 4 |
| `Harness::status_of` | `fn status_of(response: &str) -> u16` | Parses the status line. | 4 |

**Page auth and sockets**

| helper | signature | contract | added by |
|---|---|---|---|
| `mint_bootstrap` | `fn mint_bootstrap(&self, artifact: &str) -> String` | Mints through the server's own state, as `push` and `open` do. | 5 |
| `mint_bootstrap_aged` | `fn mint_bootstrap_aged(&self, artifact: &str, age: Duration) -> String` | Backdates the token so expiry is testable without waiting five minutes. | 5 |
| `session_cookie` | `fn session_cookie(&self, artifact: &str) -> String` | Walks the real bootstrap redirect and returns the `Set-Cookie` value. | 5 |
| `connect_page` | `fn connect_page(&self) -> FakePage` | A socket with a valid cookie and a matching Origin. Panics if refused. | 6 |
| `connect_page_raw` | `fn connect_page_raw(&self, cookie: Option<&str>, origin: Option<&str>) -> Result<FakePage, String>` | The same, with either header omitted or wrong. | 6 |
| `page_count` | `fn page_count(&self) -> usize` | Connected sockets. | 6 |
| `broadcast_test_frame` | `fn broadcast_test_frame(&self, seq: u64)` | Pushes an empty frame, to test the transport without an event. | 6 |
| `wait_for` | `fn wait_for(&self, cond: impl FnMut() -> bool, what: &str)` | Polls for up to five seconds. Never `sleep` a fixed time in a test: it is either slow or flaky, usually both. | 6 |
| `FakePage::next_frame` | `fn next_frame(&mut self) -> serde_json::Value` | Blocks for the next text frame, parsed. | 6 |
| `FakePage::send` | `fn send(&mut self, value: serde_json::Value)` | Sends one JSON message as the page. | 6 |
| `page_chat` | `fn page_chat(&self, page: &mut FakePage, thread: &str, text: &str)` | Sends a `chat.sent` the way the page will. | 12 |
| `page_submit` | `fn page_submit(&self, page: &mut FakePage, verdict: &str)` | Sends `review.submitted` with the current threads, answers, and reviewed marks. | 12 |

**Log and state inspection**

| helper | signature | contract | added by |
|---|---|---|---|
| `log_passive` | `fn log_passive(&self, kind: &str)` | Appends a passive event directly, bypassing the page. | 9 |
| `log_active` | `fn log_active(&self, kind: &str)` | The same for an active one. | 9 |
| `log_submitted` | `fn log_submitted(&self, artifact: &str)` | Appends a `review.submitted` carrying a minimal valid feedback document. | 10 |
| `last_seq` | `fn last_seq(&self) -> u64` | The log's highest sequence number. | 11 |
| `last_event_of_type` | `fn last_event_of_type(&self, kind: &str) -> serde_json::Value` | The most recent event of that type. Panics if there is none, so a missing event fails loudly. | 11 |
| `count_events` | `fn count_events(&self, kind: &str) -> usize` | How many of that type are in the log. | 13 |
| `last_activity` | `fn last_activity(&self) -> Instant` | `Core::last_activity`, for the ping-throttle test. | 13 |
| `age_lease` | `fn age_lease(&self, by: Duration)` | Backdates the lease so TTL behaviour is testable. | 7 |
| `rendered_body` | `fn rendered_body(&self, artifact: &str) -> String` | The HTML the server would serve now. | 11 |
| `thread_status` | `fn thread_status(&self, thread: &str) -> String` | `open`, `changed`, `declined`, or `unanchored`. | 11 |
| `plan_path` | `fn plan_path(&self) -> PathBuf` | The fixture the harness pushes, copied into the temp repo so a test may write beside it. | 13 |

**Driving the CLI**

| helper | signature | contract | added by |
|---|---|---|---|
| `support::git_repo` | `fn git_repo() -> tempfile::TempDir` | A `git init`-ed temp directory, because the state directory is keyed by the git root. | 8 |
| `support::artefacto` | `fn artefacto(dir: &Path) -> assert_cmd::Command` | The built binary, `current_dir` set, `XDG_STATE_HOME` inside the temp dir. | 8 |
| `run_cli` | `fn run_cli(&self, args: &[&str]) -> CliOut` | Runs against **this harness's** server. `CliOut` is `{ code: i32, stdout: String, stderr: String }`. | 10 |
| `take_lease` | `fn take_lease(&self, name: &str) -> String` | Acquires a lease and returns the session token. | 7 |
| `take_lease_with_takeover` | `fn take_lease_with_takeover(&self, name: &str) -> String` | The same with `--takeover`, bumping the generation. | 7 |
| `release_lease` | `fn release_lease(&self, session: &str)` | Releases it, as a `--follow` disconnect does. | 13 |
| `await_now` | `fn await_now(&self, session: &str, timeout_secs: u64) -> serde_json::Value` | Runs `await` to completion and returns the parsed result. | 10 |
| `await_in_background` | `fn await_in_background(&self, session: &str, timeout_secs: u64) -> Waiting` | Starts `await` on a thread. `Waiting::join(self) -> serde_json::Value` blocks for it. | 10 |
| `push` | `fn push(&self, extra: &[&str]) -> CliOut` | Pushes the default fixture with the extra flags. | 11 |
| `push_json` | `fn push_json(&self, extra: &[&str]) -> serde_json::Value` | The same, parsing `--json` output. Panics on a non-zero exit. | 11 |
| `push_file` | `fn push_file(&self, fixture: &str, extra: &[&str]) -> CliOut` | Pushes a named fixture from `tests/fixtures/plan/`. | 11 |
| `open_thread` | `fn open_thread(&self, target: &str) -> String` | Opens a thread on a ref as the reviewer; returns its id. | 11 |
| `open_thread_with_text` | `fn open_thread_with_text(&self, target: &str, text: &str) -> String` | The same with chosen text, for the escaping test. | 12 |
| `write_resolutions` | `fn write_resolutions(&self, entries: &[(&str, &str, &str)]) -> PathBuf` | Writes a `--resolutions` file of `{thread, status, note}`. | 11 |

**One rule for all of them:** a helper may reach into `Shared` to *observe* state or to *simulate the reviewer*, but never to skip a code path the production caller would take. `session_cookie` walks the real redirect rather than reading the cookie out of `Core`, and `take_lease` goes through `lease::acquire` rather than assigning a token. A harness that shortcuts the code under test proves nothing.
