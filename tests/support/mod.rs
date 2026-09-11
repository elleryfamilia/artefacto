//! Test support: real repositories, real servers, real sockets.
//!
//! Nothing here touches the developer's own state directory, and **nothing
//! sets a process-global environment variable**. Cargo runs a test binary's
//! tests on threads of one process, so `std::env::set_var` is both
//! order-dependent and a documented data race; every spawned command gets
//! `XDG_STATE_HOME` through `Command::env` instead, and test-side paths are
//! computed with `state_dir_in`.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub fn bin() -> PathBuf {
    // The integration test binary lives in target/<profile>/deps.
    let mut p = std::env::current_exe().expect("test binary path");
    p.pop();
    if p.ends_with("deps") {
        p.pop();
    }
    p.join("artefacto")
}

pub struct Repo {
    pub dir: tempfile::TempDir,
}

impl Repo {
    /// A `git init`-ed temp directory. The state directory is keyed by the git
    /// root, so a repository is the unit of isolation.
    pub fn new() -> Repo {
        let dir = tempfile::tempdir().expect("tempdir");
        let ok = Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir.path())
            .status()
            .expect("git init")
            .success();
        assert!(ok, "git init failed");
        Repo { dir }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    pub fn state_root(&self) -> PathBuf {
        self.dir.path().join("state")
    }

    /// Computed the way the child computes it, without reading the
    /// environment: `git rev-parse` resolves symlinks (on macOS `/var` is one)
    /// so the digest must be taken over the resolved path.
    pub fn state_dir(&self) -> PathBuf {
        let root = artefacto::server::state_dir::repo_root(self.dir.path()).expect("repo root");
        artefacto::server::state_dir::state_dir_in(&self.state_root(), &root)
    }

    pub fn server_json(&self) -> PathBuf {
        self.state_dir().join("server.json")
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(bin());
        c.args(args)
            .current_dir(self.dir.path())
            .env("XDG_STATE_HOME", self.state_root());
        c
    }

    pub fn run(&self, args: &[&str]) -> Out {
        let out = self.command(args).output().expect("running artefacto");
        Out {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        }
    }

    pub fn spawn(&self, args: &[&str]) -> std::process::Child {
        self.command(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawning artefacto")
    }

    pub fn json(&self, args: &[&str]) -> serde_json::Value {
        let out = self.run(args);
        assert_eq!(out.code, 0, "{args:?} failed: {}", out.stderr);
        serde_json::from_str(&out.stdout).expect("json output")
    }

    /// Best effort: a test that already stopped the server must not fail here.
    pub fn stop(&self) {
        let _ = self.run(&["stop"]);
    }

    fn field(&self, key: &str) -> serde_json::Value {
        let raw = std::fs::read_to_string(self.server_json()).expect("server.json");
        let v: serde_json::Value = serde_json::from_str(&raw).expect("server.json is json");
        v[key].clone()
    }

    pub fn port(&self) -> u16 {
        self.field("port").as_u64().expect("port") as u16
    }

    pub fn pid(&self) -> u32 {
        self.field("pid").as_u64().expect("pid") as u32
    }

    pub fn secret(&self) -> String {
        self.field("secret").as_str().expect("secret").to_string()
    }
}

pub struct Out {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Out {
    pub fn success(&self) -> &Self {
        assert_eq!(
            self.code, 0,
            "expected success, got {}: {}",
            self.code, self.stderr
        );
        self
    }
}

/// A one-shot GET with the correct `Host`, so the tests need no HTTP client.
pub fn get(port: u16, path: &str) -> String {
    raw(
        port,
        &format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"),
    )
}

/// Bytes verbatim, so a test can send a hostile `Host` or omit a header. A
/// polite HTTP client normalizes exactly what these tests attack with.
pub fn raw(port: u16, request: &str) -> String {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect");
    s.set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .expect("timeout");
    s.write_all(request.as_bytes()).expect("write");
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    out
}

pub fn status_of(response: &str) -> u16 {
    response
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0)
}

pub fn parent_of(pid: u32) -> u32 {
    let out = Command::new("ps")
        .args(["-o", "ppid=", "-p", &pid.to_string()])
        .output()
        .expect("ps");
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or(0)
}

pub fn is_alive(pid: u32) -> bool {
    artefacto::server::state_dir::is_alive(pid)
}

/// Polls a condition rather than sleeping a fixed time: a fixed sleep is
/// either slow or flaky, usually both.
pub fn wait_for(mut cond: impl FnMut() -> bool, what: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if cond() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!("timed out waiting: {what}");
}

// ---------------------------------------------------------------------------
// In-process server, for tests that need to reach into `Shared` — broadcasting
// a frame, counting pages, inspecting the log. The lifecycle tests drive the
// real binary instead; both are real servers over real TCP.
// ---------------------------------------------------------------------------

use artefacto::server::http::{run, Shared};
use std::sync::Arc;

pub struct InProcess {
    pub port: u16,
    pub shared: Arc<Shared>,
    pub dir: Option<tempfile::TempDir>,
    server: Arc<tiny_http::Server>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl InProcess {
    pub fn start() -> InProcess {
        InProcess::start_with_idle(std::time::Duration::from_secs(3600))
    }

    pub fn start_with_idle(idle: std::time::Duration) -> InProcess {
        let dir = tempfile::tempdir().expect("tempdir");
        let secret = artefacto::server::state_dir::new_secret();
        InProcess::boot(dir, secret, idle)
    }

    fn boot(dir: tempfile::TempDir, secret: String, idle: std::time::Duration) -> InProcess {
        let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").expect("bind"));
        let port = server.server_addr().to_ip().expect("ip").port();
        let shared = Arc::new(Shared::new(dir.path(), secret, port).expect("shared"));
        let s = Arc::clone(&server);
        let sh = Arc::clone(&shared);
        let thread = std::thread::spawn(move || run(sh, s, idle));
        InProcess {
            port,
            shared,
            dir: Some(dir),
            server,
            thread: Some(thread),
        }
    }

    /// Stop this server and start a new one over the **same** state directory
    /// and secret. Everything the new server knows, it read from the log.
    pub fn restart(mut self) -> InProcess {
        let dir = self.dir.take().expect("a harness restarts once per step");
        let secret = self.shared.secret.clone();
        self.shutdown();
        InProcess::boot(dir, secret, std::time::Duration::from_secs(3600))
    }

    fn shutdown(&mut self) {
        self.shared.request_stop();
        self.server.unblock();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }

    pub fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn get(&self, path: &str, headers: &[(&str, &str)]) -> String {
        let mut req = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n", self.port);
        for (k, v) in headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str("Connection: close\r\n\r\n");
        raw(self.port, &req)
    }

    pub fn mint(&self, artifact: &str) -> String {
        artefacto::server::page::mint_bootstrap(&self.shared, artifact).expect("valid id")
    }

    /// Walks the real bootstrap redirect and returns the cookie it set. Not a
    /// shortcut through `Shared`: a harness that skips the code under test
    /// proves nothing.
    pub fn session_cookie(&self, artifact: &str) -> String {
        let token = self.mint(artifact);
        let response = self.get(&format!("/b/{token}"), &[]);
        response
            .lines()
            .find(|l| l.to_ascii_lowercase().starts_with("set-cookie:"))
            .and_then(|l| l.split_once(": ").map(|(_, v)| v))
            .and_then(|v| v.split(';').next())
            .expect("bootstrap must set a cookie")
            .trim()
            .to_string()
    }

    pub fn page_count(&self) -> usize {
        artefacto::server::socket::page_count(&self.shared)
    }

    /// A frame carrying one real event; spec 6.1 says a frame has one or more.
    pub fn broadcast_test_frame(&self, seq: u64) {
        let event = artefacto::server::event::Event {
            format: artefacto::server::event::EVENT_FORMAT.to_string(),
            seq,
            ts: "2026-09-09T00:00:00Z".to_string(),
            artifact: "plan:x".to_string(),
            revision: 1,
            actor: artefacto::server::event::Actor::Reviewer,
            r#type: "thread.opened".to_string(),
            data: serde_json::json!({ "thread": "c-1" }),
        };
        let frame = artefacto::server::event::Frame::of(vec![event]);
        artefacto::server::socket::broadcast(&self.shared, &frame);
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
        use tungstenite::client::IntoClientRequest;
        let mut req = format!("ws://127.0.0.1:{}/ws", self.port)
            .into_client_request()
            .map_err(|e| e.to_string())?;
        if let Some(c) = cookie {
            req.headers_mut().insert("Cookie", c.parse().unwrap());
        }
        if let Some(o) = origin {
            req.headers_mut().insert("Origin", o.parse().unwrap());
        }
        // Keep the stream type tungstenite returns. Unwrapping it and
        // rebuilding with `from_raw_socket` discards the codec's buffer,
        // losing any frame that arrived right after the handshake.
        tungstenite::connect(req)
            .map(|(ws, _)| FakePage { ws })
            .map_err(|e| e.to_string())
    }
}

impl Drop for InProcess {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub struct FakePage {
    ws: tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
}

impl FakePage {
    /// Bounded, so a regression is a failing test rather than a hung job.
    pub fn next_frame(&mut self) -> serde_json::Value {
        self.set_deadline(Some(std::time::Duration::from_secs(5)));
        loop {
            match self.ws.read().expect("the socket must stay open") {
                tungstenite::Message::Text(t) => {
                    return serde_json::from_str(&t).expect("frames are JSON")
                }
                tungstenite::Message::Close(_) => panic!("the server closed the socket"),
                _ => continue,
            }
        }
    }

    /// True when nothing arrives within `d`.
    pub fn no_frame_within(&mut self, d: std::time::Duration) -> bool {
        self.set_deadline(Some(d));
        self.ws.read().is_err()
    }

    fn set_deadline(&mut self, d: Option<std::time::Duration>) {
        if let tungstenite::stream::MaybeTlsStream::Plain(s) = self.ws.get_mut() {
            let _ = s.set_read_timeout(d);
        }
    }
}

impl InProcess {
    /// Push a minimal valid plan straight through the committer, so tests that
    /// need an artifact do not need the whole `push` command.
    pub fn seed_artifact(&self) -> String {
        use artefacto::server::event::Actor;
        use artefacto::server::http::Committer;
        let c = Committer::open(&self.shared);
        c.append(
            "plan:demo",
            1,
            Actor::Agent,
            "revision.published",
            serde_json::json!({
                "plan": {
                    "format": "artefacto.plan/1",
                    "meta": { "id": "demo", "title": "Demo" },
                    "phases": [{ "id": "p-one", "tasks": [{ "id": "t-a" }, { "id": "t-b" }] }]
                },
                "plan_hash": "sha256:abc",
                "source_path": "/tmp/demo.json",
                "summary": "first"
            }),
        )
        .expect("seed");
        "plan:demo".to_string()
    }

    pub fn last_seq(&self) -> u64 {
        self.shared.log.lock().unwrap().last_seq()
    }

    /// Every logged event of one type, as JSON.
    pub fn events_of_type(&self, kind: &str) -> Vec<serde_json::Value> {
        let log = self.shared.log.lock().unwrap();
        log.since(0)
            .iter()
            .filter(|e| e.r#type == kind)
            .map(|e| serde_json::to_value(e).expect("an event serializes"))
            .collect()
    }

    pub fn last_event_of_type(&self, kind: &str) -> serde_json::Value {
        self.events_of_type(kind)
            .pop()
            .unwrap_or_else(|| panic!("no {kind} event was logged"))
    }

    /// Push the lease's freshness clock into the past.
    ///
    /// The clock is signed milliseconds since the server started, so this
    /// cannot underflow the way `Instant` subtraction would on a machine
    /// booted minutes ago.
    pub fn age_lease(&self, by: std::time::Duration) {
        let mut core = self.shared.core.lock().unwrap();
        core.lease_seen_ms -= by.as_millis() as i64;
    }

    /// A bearer-authenticated `/cli/` GET, raw, so a test can assert on what
    /// the response does *not* contain as well as what it does.
    pub fn cli_raw(&self, route: &str) -> String {
        self.get(
            &format!("/cli/{route}"),
            &[("Authorization", &format!("Bearer {}", self.shared.secret))],
        )
    }

    /// Is the accept loop still running? `false` once the server self-exited.
    pub fn serving(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }

    pub fn thread_count(&self) -> usize {
        artefacto::server::http::with_review(&self.shared, |r| {
            r.artifacts
                .get("plan:demo")
                .map(|a| a.threads.len())
                .unwrap_or(0)
        })
    }

    pub fn thread_status(&self, id: &str) -> String {
        artefacto::server::http::with_review(&self.shared, |r| {
            r.artifacts
                .get("plan:demo")
                .and_then(|a| a.thread(id))
                .map(|t| t.status.as_str().to_string())
                .unwrap_or_else(|| "missing".to_string())
        })
    }

    pub fn reviewer_idle_for(&self) -> std::time::Duration {
        self.shared
            .core
            .lock()
            .unwrap()
            .last_reviewer_activity_at
            .elapsed()
    }

    /// POST a command the way the page will, with the cookie and a strict
    /// Origin. Returns the parsed reply.
    pub fn post_cmd(
        &self,
        cookie: &str,
        artifact: &str,
        body: serde_json::Value,
    ) -> serde_json::Value {
        let payload = body.to_string();
        let req = format!(
            "POST /a/{artifact}/cmd HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nCookie: {cookie}\r\n\
             Origin: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{payload}",
            self.port,
            self.origin(),
            payload.len()
        );
        let response = raw(self.port, &req);
        let body = response.split("\r\n\r\n").nth(1).unwrap_or("");
        serde_json::from_str(body).unwrap_or_else(|e| panic!("reply was not json: {e}\n{response}"))
    }

    /// A raw POST, so a test can send a wrong Origin or a bad method.
    pub fn post_cmd_raw(&self, headers: &str, artifact: &str, body: &str) -> String {
        raw(
            self.port,
            &format!(
                "POST /a/{artifact}/cmd HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n{headers}\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                self.port,
                body.len()
            ),
        )
    }
}

impl FakePage {
    /// The first frame every socket sends, carrying this page's own id.
    pub fn hello(&mut self) -> u64 {
        let frame = self.next_frame();
        assert_eq!(
            frame["format"], "artefacto.hello/1",
            "hello is always first"
        );
        frame["page"].as_u64().expect("a page id")
    }
}
