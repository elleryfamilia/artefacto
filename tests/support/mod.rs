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
